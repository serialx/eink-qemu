//! OS authorization is separate from the authenticated IPC caller. These
//! credentials stay in native code and must never enter settings, logs or UI.
use serde::{Deserialize, Deserializer, Serialize, Serializer};

pub struct AuthorizationProof([u8; 32]);
impl Clone for AuthorizationProof { fn clone(&self) -> Self { Self(self.0) } }
impl std::fmt::Debug for AuthorizationProof {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str("AuthorizationProof([redacted])") }
}
impl Drop for AuthorizationProof {
    fn drop(&mut self) {
        for byte in &mut self.0 { unsafe { std::ptr::write_volatile(byte, 0); } }
    }
}
impl Serialize for AuthorizationProof {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0.iter().map(|b| format!("{b:02x}")).collect::<String>())
    }
}
impl<'de> Deserialize<'de> for AuthorizationProof {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        if text.len() != 64 || !text.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
            return Err(serde::de::Error::custom("Invalid authorization credential."));
        }
        let mut bytes = [0; 32];
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16).map_err(serde::de::Error::custom)?;
        }
        Ok(Self(bytes))
    }
}

#[cfg(windows)]
#[path = "platform/windows/authorization.rs"]
pub mod windows;

#[cfg(target_os = "macos")]
pub mod macos {
    use super::AuthorizationProof;
    use crate::model::BridgeError;
    use std::{ffi::{c_char, c_void}, ptr};
    use core_foundation::{base::TCFType, string::{CFString, CFStringRef}, error::{CFError, CFErrorRef}};
    use core_foundation::{base::CFType, dictionary::{CFDictionary, CFDictionaryRef}, array::CFArray,
        boolean::CFBoolean, number::CFNumber};

    /// Installed with a private administrator-authentication policy.
    /// Never accept a right name from the caller or silently broaden its policy.
    pub const RIGHT: &str = "net.serialx.eink-qemu.bridge.configure";
    const RIGHT_C: &[u8] = b"net.serialx.eink-qemu.bridge.configure\0";
    pub const HELPER_LABEL: &str = "net.serialx.eink-qemu.bridge-helper";
    #[repr(C)]
    struct Item { name: *const c_char, length: usize, value: *mut c_void, flags: u32 }
    #[repr(C)]
    struct Rights { count: u32, items: *mut Item }
    #[repr(C)]
    struct ExternalForm { bytes: [u8; 32] }
    #[link(name = "Security", kind = "framework")]
    extern "C" {
        fn AuthorizationCreate(rights: *const Rights, environment: *const Rights, flags: u32, authorization: *mut *mut c_void) -> i32;
        fn AuthorizationFree(authorization: *mut c_void, flags: u32) -> i32;
        fn AuthorizationCopyRights(authorization: *mut c_void, rights: *const Rights, environment: *const Rights, flags: u32, result: *mut *mut Rights) -> i32;
        fn AuthorizationMakeExternalForm(authorization: *mut c_void, form: *mut ExternalForm) -> i32;
        fn AuthorizationCreateFromExternalForm(form: *const ExternalForm, authorization: *mut *mut c_void) -> i32;
        fn AuthorizationRightGet(name: *const c_char, definition: *mut *const c_void) -> i32;
        fn AuthorizationRightSet(authorization: *mut c_void, name: *const c_char, definition: *const c_void,
            description: CFStringRef, bundle: *const c_void, table: CFStringRef) -> i32;
    }
    #[link(name = "ServiceManagement", kind = "framework")]
    extern "C" {
        static kSMDomainSystemLaunchd: CFStringRef;
        fn SMJobBless(domain: CFStringRef, label: CFStringRef, authorization: *mut c_void, error: *mut CFErrorRef) -> u8;
        fn SMJobSubmit(domain: CFStringRef, job: CFDictionaryRef, authorization: *mut c_void, error: *mut CFErrorRef) -> u8;
        fn SMJobRemove(domain: CFStringRef, label: CFStringRef, authorization: *mut c_void, wait: u8, error: *mut CFErrorRef) -> u8;
        fn SMJobCopyDictionary(domain: CFStringRef, label: CFStringRef) -> CFDictionaryRef;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFBundleGetMainBundle() -> *const c_void;
        fn CFBundleGetValueForInfoDictionaryKey(bundle: *const c_void, key: CFStringRef) -> *const c_void;
    }
    /// Called only by the installed root helper. Preserve an administrator's
    /// existing policy; register a new right requiring administrator credentials.
    pub fn register_right() -> Result<(), BridgeError> {
        if unsafe { libc::geteuid() } != 0 { return Err(BridgeError::new("EPERM", "Only the installed helper can register bridge authorization.")); }
        match unsafe { AuthorizationRightGet(RIGHT_C.as_ptr().cast(), ptr::null_mut()) } {
            0 => return Ok(()),
            -60005 => (), // No definition exists.
            code => return status(code),
        }
        let authorization = Authorization::empty()?;
        let rule = right_definition();
        let description = CFString::new("Configure bridge networking for eink-qemu.");
        status(unsafe { AuthorizationRightSet(authorization.0, RIGHT_C.as_ptr().cast(), rule.as_CFTypeRef(),
            description.as_concrete_TypeRef(), ptr::null(), ptr::null()) })
    }
    fn right_definition() -> core_foundation::dictionary::CFDictionary<CFString, core_foundation::base::CFType> {
        use core_foundation::{boolean::CFBoolean, dictionary::CFDictionary, number::CFNumber};
        // authenticate-admin has timeout=0: the credential expires between
        // the UI request and helper verification. A private credential allows
        // this one externalized reference to cross the process boundary. New
        // references cannot borrow it, and our guards destroy it after Apply.
        CFDictionary::from_CFType_pairs(&[
            (CFString::new("class"), CFString::new("user").as_CFType()),
            (CFString::new("group"), CFString::new("admin").as_CFType()),
            (CFString::new("authenticate-user"), CFBoolean::true_value().as_CFType()),
            (CFString::new("shared"), CFBoolean::false_value().as_CFType()),
            (CFString::new("allow-root"), CFBoolean::false_value().as_CFType()),
            (CFString::new("timeout"), CFNumber::from(60i32).as_CFType()),
            (CFString::new("version"), CFNumber::from(1i32).as_CFType()),
        ])
    }
    /// SMJobBless supports the app's macOS 12.3 baseline. It verifies matching
    /// app/helper code requirements and installs the helper at Apple's protected
    /// location. No executable path or installer command comes from IPC.
    pub fn authorize_install() -> Result<(), BridgeError> {
        let _gate = crate::macos_maintenance::client_gate()?;
        reject_active_maintenance()?;
        let key = CFString::new("SMPrivilegedExecutables");
        let bundle = unsafe { CFBundleGetMainBundle() };
        if bundle.is_null() || unsafe { CFBundleGetValueForInfoDictionaryKey(bundle, key.as_concrete_TypeRef()) }.is_null() {
            return Err(BridgeError::new("EPACKAGE", "Install the signed desktop app to set up bridge support."));
        }
        let authorization = Authorization::empty()?;
        authorization.check_right(b"com.apple.ServiceManagement.blesshelper\0", (1 << 0) | (1 << 1))?;
        // The client gate spans the prompt. If its former owner crashed,
        // launchd may still be running removal independently of that app.
        reject_active_maintenance()?;
        if maintenance_job().is_some() {
            authorization.check_right(b"com.apple.ServiceManagement.daemons.modify\0", (1 << 0) | (1 << 1))?;
            reject_active_maintenance()?;
            remove_maintenance_job(&authorization)?;
        }
        let label = CFString::new(HELPER_LABEL);
        let mut error = ptr::null_mut();
        if unsafe { SMJobBless(kSMDomainSystemLaunchd, label.as_concrete_TypeRef(), authorization.0, &mut error) } != 0 { return Ok(()); }
        let detail = if error.is_null() { "Service Management returned no diagnostic.".into() }
            else { let error = unsafe { CFError::wrap_under_create_rule(error) }; format!("{} ({})", error.description(), error.code()) };
        Err(BridgeError { code: "EINSTALL".into(), message: "macOS could not install bridge support. Check that the app and helper are signed by the same developer.".into(), detail: Some(detail) })
    }
    fn maintenance_job() -> Option<CFDictionary<CFString, CFType>> {
        let label = CFString::new(crate::macos_maintenance::MAINTENANCE_LABEL);
        let job = unsafe { SMJobCopyDictionary(kSMDomainSystemLaunchd, label.as_concrete_TypeRef()) };
        if job.is_null() { None } else { Some(unsafe { CFDictionary::wrap_under_create_rule(job) }) }
    }
    fn reject_active_maintenance() -> Result<(), BridgeError> {
        if let Some(job) = maintenance_job() {
            job_id(&job)?;
            if job_value(&job, "PID").is_some() || job_value(&job, "LastExitStatus").is_none() {
                return Err(BridgeError::new("EBUSY", "Bridge support removal is still running. Wait for it to finish."));
            }
        }
        Ok(())
    }
    fn completion_after_exit(status: i64, read: impl FnOnce() -> Result<Option<Result<(), BridgeError>>, BridgeError>) -> Result<(), BridgeError> {
        // The worker may publish the result between the first read and exit.
        read()?.unwrap_or_else(|| Err(BridgeError::new("ECLEANUP",
            format!("Bridge maintenance exited without a completion record (status {status}). Keep the app installed and retry setup."))))
    }
    fn helper_requirement() -> Result<String, BridgeError> {
        let invalid = || BridgeError::new("EPACKAGE", "Use the signed desktop app to uninstall bridge support.");
        let bundle = unsafe { CFBundleGetMainBundle() };
        if bundle.is_null() { return Err(invalid()); }
        let key = CFString::new("SMPrivilegedExecutables");
        let raw = unsafe { CFBundleGetValueForInfoDictionaryKey(bundle, key.as_concrete_TypeRef()) };
        if raw.is_null() { return Err(invalid()); }
        let dictionary = unsafe { CFType::wrap_under_get_rule(raw) }.downcast::<CFDictionary>().ok_or_else(invalid)?;
        let dictionary: CFDictionary<CFString, CFType> = unsafe { CFDictionary::wrap_under_get_rule(dictionary.as_concrete_TypeRef()) };
        let requirement = job_value(&dictionary, HELPER_LABEL).and_then(|value| value.downcast::<CFString>()).ok_or_else(invalid)?;
        Ok(requirement.to_string())
    }
    fn job_value(job: &CFDictionary<CFString, CFType>, key: &str) -> Option<CFType> {
        job.find(&CFString::new(key)).map(|v| v.as_CFType())
    }
    fn job_id(job: &CFDictionary<CFString, CFType>) -> Result<crate::model::OperationId, BridgeError> {
        let invalid = || BridgeError::new("ECONFLICT", "Another service uses the bridge maintenance identity. It has not been changed.");
        let args = job_value(job, "ProgramArguments").and_then(|v| v.downcast::<CFArray>()).ok_or_else(invalid)?;
        if args.len() != 3 { return Err(invalid()); }
        let strings = args.iter().map(|v| unsafe { CFType::wrap_under_get_rule(*v) }
            .downcast::<CFString>().map(|s| s.to_string())).collect::<Option<Vec<_>>>().ok_or_else(invalid)?;
        if strings[0] != crate::macos_maintenance::HELPER_PATH || strings[1] != "--uninstall-support" { return Err(invalid()); }
        crate::model::OperationId::new(&strings[2]).map_err(|_| invalid())
    }
    fn sm_error(error: CFErrorRef) -> BridgeError {
        let detail = if error.is_null() { "Service Management returned no diagnostic.".into() }
            else { let error = unsafe { CFError::wrap_under_create_rule(error) }; format!("{} ({})", error.description(), error.code()) };
        BridgeError { code: "ECLEANUP".into(), message: "macOS could not finish bridge support removal. Reopen setup to retry.".into(), detail: Some(detail) }
    }
    fn remove_maintenance_job(authorization: &Authorization) -> Result<(), BridgeError> {
        let label = CFString::new(crate::macos_maintenance::MAINTENANCE_LABEL);
        let mut error = ptr::null_mut();
        if unsafe { SMJobRemove(kSMDomainSystemLaunchd, label.as_concrete_TypeRef(), authorization.0, 1, &mut error) } == 0 { return Err(sm_error(error)); }
        Ok(())
    }
    fn maintenance_dictionary(id: &crate::model::OperationId) -> CFDictionary<CFString, CFType> {
        let args = CFArray::from_CFTypes(&[
            CFString::new(crate::macos_maintenance::HELPER_PATH), CFString::new("--uninstall-support"), CFString::new(id.as_str()),
        ]);
        CFDictionary::from_CFType_pairs(&[
            (CFString::new("Label"), CFString::new(crate::macos_maintenance::MAINTENANCE_LABEL).as_CFType()),
            (CFString::new("ProgramArguments"), args.as_CFType()),
            (CFString::new("UserName"), CFString::new("root").as_CFType()),
            (CFString::new("RunAtLoad"), CFBoolean::true_value().as_CFType()),
            (CFString::new("LaunchOnlyOnce"), CFBoolean::true_value().as_CFType()),
            (CFString::new("KeepAlive"), CFBoolean::false_value().as_CFType()),
        ])
    }
    /// A separate fixed launchd job can stop the blessed service without
    /// terminating the process responsible for transactional resource cleanup.
    /// No all-owner operation is added to the ordinary helper IPC protocol.
    pub fn authorize_uninstall() -> Result<(), BridgeError> {
        use crate::macos_maintenance::{read_result, verify_installed_helper};
        use std::time::{Duration, Instant};
        let _gate = crate::macos_maintenance::client_gate()?;
        reject_active_maintenance()?;
        let requirement = helper_requirement()?;
        verify_installed_helper(&requirement)?;
        let authorization = Authorization::empty()?;
        authorization.check_right(b"com.apple.ServiceManagement.daemons.modify\0", (1 << 0) | (1 << 1))?;
        if let Some(job) = maintenance_job() {
            job_id(&job)?;
            if job_value(&job, "PID").is_some() || job_value(&job, "LastExitStatus").is_none() {
                return Err(BridgeError::new("EBUSY", "Bridge support removal is already running."));
            }
            remove_maintenance_job(&authorization)?;
        }
        // Authorization may take time. Revalidate the fixed executable after it.
        verify_installed_helper(&requirement)?;
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes).map_err(|_| BridgeError::new("EIO", "Cannot create a maintenance identity."))?;
        let id = crate::model::OperationId::new(bytes.iter().map(|b| format!("{b:02x}")).collect::<String>())?;
        let job = maintenance_dictionary(&id);
        let mut error = ptr::null_mut();
        if unsafe { SMJobSubmit(kSMDomainSystemLaunchd, job.as_concrete_TypeRef(), authorization.0, &mut error) } == 0 { return Err(sm_error(error)); }
        let deadline = Instant::now() + Duration::from_secs(180);
        loop {
            if let Some(result) = read_result(&id)? {
                // Do not kill a worker between its durable result and normal exit.
                if let Some(job) = maintenance_job() {
                    if job_id(&job)? != id { return Err(BridgeError::new("ECONFLICT", "The maintenance job changed during removal.")); }
                    if job_value(&job, "PID").is_some() {
                        if Instant::now() >= deadline { return Err(BridgeError::new("EBUSY", "Cleanup finished but the maintenance process has not exited.")); }
                        std::thread::sleep(Duration::from_millis(100)); continue;
                    }
                    remove_maintenance_job(&authorization)?;
                }
                return result;
            }
            if let Some(job) = maintenance_job() {
                if job_id(&job)? != id { return Err(BridgeError::new("ECONFLICT", "The maintenance job changed during removal.")); }
                if job_value(&job, "PID").is_none() {
                    if let Some(status) = job_value(&job, "LastExitStatus").and_then(|v| v.downcast::<CFNumber>()).and_then(|v| v.to_i64()) {
                        let result = completion_after_exit(status, || read_result(&id));
                        remove_maintenance_job(&authorization)?;
                        return result;
                    }
                }
            }
            if Instant::now() >= deadline { return Err(BridgeError::new("EBUSY", "Bridge support removal is still running. Reopen setup later to check its result.")); }
            std::thread::sleep(Duration::from_millis(200));
        }
    }
    fn status(code: i32) -> Result<(), BridgeError> {
        match code {
            0 => Ok(()),
            -60006 => Err(BridgeError::new("EAUTHCANCELLED", "Administrator authorization was cancelled.")),
            _ => Err(BridgeError { code: "EAUTHDENIED".into(), message: "Administrator authorization is required for bridge setup.".into(), detail: Some(format!("Authorization Services status {code}")) }),
        }
    }
    /// Keep this native guard alive until Apply returns. Dropping it invalidates
    /// the acquired rights, including their external representation.
    #[derive(Debug)]
    pub struct Authorization(*mut c_void);
    impl Drop for Authorization {
        fn drop(&mut self) { unsafe { AuthorizationFree(self.0, 1 << 3); } }
    }
    impl Authorization {
        pub fn empty() -> Result<Self, BridgeError> {
            let mut reference = ptr::null_mut();
            status(unsafe { AuthorizationCreate(ptr::null(), ptr::null(), 0, &mut reference) })?;
            if reference.is_null() { return Err(BridgeError::new("EAUTHDENIED", "Authorization Services returned no authorization.")); }
            Ok(Self(reference))
        }
        pub fn request() -> Result<Self, BridgeError> {
            let reference = Self::empty()?;
            // InteractionAllowed | ExtendRights. Do not use PartialRights or
            // PreAuthorize, which can report success without granting rights.
            reference.check((1 << 0) | (1 << 1))?;
            Ok(reference)
        }
        pub fn proof(&self) -> Result<AuthorizationProof, BridgeError> {
            let mut form = ExternalForm { bytes: [0; 32] };
            status(unsafe { AuthorizationMakeExternalForm(self.0, &mut form) })?;
            Ok(AuthorizationProof(form.bytes))
        }
        /// Helper-side check never interacts or extends rights. A real reference
        /// alone is insufficient: the exact bridge right must already be held.
        pub fn verify(proof: &AuthorizationProof) -> Result<Self, BridgeError> {
            let mut reference = ptr::null_mut();
            status(unsafe { AuthorizationCreateFromExternalForm(&ExternalForm { bytes: proof.0 }, &mut reference) })?;
            if reference.is_null() { return Err(BridgeError::new("EAUTHDENIED", "Invalid administrator authorization.")); }
            let reference = Self(reference);
            reference.check(0)?;
            Ok(reference)
        }
        fn check(&self, flags: u32) -> Result<(), BridgeError> {
            self.check_right(RIGHT_C, flags)
        }
        fn check_right(&self, name: &'static [u8], flags: u32) -> Result<(), BridgeError> {
            let mut item = Item { name: name.as_ptr().cast(), length: 0, value: ptr::null_mut(), flags: 0 };
            let rights = Rights { count: 1, items: &mut item };
            status(unsafe { AuthorizationCopyRights(self.0, &rights, ptr::null(), flags, ptr::null_mut()) })
        }
    }
    #[cfg(test)]
    mod tests {
        #[test]
        fn worker_exit_rereads_a_result_written_after_the_previous_poll() {
            use super::*;
            assert!(completion_after_exit(0, || Ok(Some(Ok(())))).is_ok());
            assert_eq!(completion_after_exit(1, || Ok(Some(Err(BridgeError::new("EINUSE", "lease"))))).unwrap_err().code, "EINUSE");
            assert_eq!(completion_after_exit(0, || Ok(None)).unwrap_err().code, "ECLEANUP");
        }
        #[test]
        fn maintenance_job_uses_only_fixed_helper_and_validated_identity() {
            use super::*;
            let id = crate::model::OperationId::new("0123456789abcdef0123456789abcdef").unwrap();
            let job = maintenance_dictionary(&id);
            assert_eq!(job_id(&job).unwrap(), id);
            assert_eq!(job_value(&job, "UserName").unwrap().downcast::<CFString>().unwrap().to_string(), "root");
            assert_eq!(job_value(&job, "KeepAlive").unwrap().downcast::<CFBoolean>().unwrap(), CFBoolean::false_value());
            assert_eq!(job_value(&job, "LaunchOnlyOnce").unwrap().downcast::<CFBoolean>().unwrap(), CFBoolean::true_value());
        }
        #[test]
        fn bridge_right_requires_private_admin_authentication_with_bounded_transfer_time() {
            use core_foundation::{base::TCFType, boolean::CFBoolean, number::CFNumber, string::CFString};
            let rule = super::right_definition();
            let value = |key: &str| rule.find(&CFString::new(key)).unwrap().as_CFType();
            assert_eq!(value("class").downcast::<CFString>().unwrap().to_string(), "user");
            assert_eq!(value("group").downcast::<CFString>().unwrap().to_string(), "admin");
            assert_eq!(value("timeout").downcast::<CFNumber>().unwrap().to_i64(), Some(60));
            assert_eq!(value("shared").downcast::<CFBoolean>().unwrap(), CFBoolean::false_value());
            assert_eq!(value("allow-root").downcast::<CFBoolean>().unwrap(), CFBoolean::false_value());
            assert_eq!(value("authenticate-user").downcast::<CFBoolean>().unwrap(), CFBoolean::true_value());
        }
        #[test]
        fn cancellation_is_distinct_from_denial() {
            assert_eq!(super::status(-60006).unwrap_err().code, "EAUTHCANCELLED");
            assert_eq!(super::status(-60005).unwrap_err().code, "EAUTHDENIED");
        }
    }
}
