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

#[cfg(target_os = "macos")]
pub mod macos {
    use super::AuthorizationProof;
    use crate::model::BridgeError;
    use std::{ffi::{c_char, c_void}, ptr};
    use core_foundation::{base::TCFType, string::{CFString, CFStringRef}, error::{CFError, CFErrorRef}};

    /// Installed with an authenticate-admin policy by the privileged installer.
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
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFBundleGetMainBundle() -> *const c_void;
        fn CFBundleGetValueForInfoDictionaryKey(bundle: *const c_void, key: CFStringRef) -> *const c_void;
    }
    /// Called only by the installed root helper. Preserve an administrator's
    /// existing policy; register a new right with Apple's authenticate-admin rule.
    pub fn register_right() -> Result<(), BridgeError> {
        if unsafe { libc::geteuid() } != 0 { return Err(BridgeError::new("EPERM", "Only the installed helper can register bridge authorization.")); }
        match unsafe { AuthorizationRightGet(RIGHT_C.as_ptr().cast(), ptr::null_mut()) } {
            0 => return Ok(()),
            -60005 => (), // No definition exists.
            code => return status(code),
        }
        let authorization = Authorization::empty()?;
        let rule = CFString::new("authenticate-admin");
        let description = CFString::new("Configure bridge networking for eink-qemu.");
        status(unsafe { AuthorizationRightSet(authorization.0, RIGHT_C.as_ptr().cast(), rule.as_CFTypeRef(),
            description.as_concrete_TypeRef(), ptr::null(), ptr::null()) })
    }
    /// SMJobBless supports the app's macOS 12.3 baseline. It verifies matching
    /// app/helper code requirements and installs the helper at Apple's protected
    /// location. No executable path or installer command comes from IPC.
    pub fn authorize_install() -> Result<(), BridgeError> {
        let key = CFString::new("SMPrivilegedExecutables");
        let bundle = unsafe { CFBundleGetMainBundle() };
        if bundle.is_null() || unsafe { CFBundleGetValueForInfoDictionaryKey(bundle, key.as_concrete_TypeRef()) }.is_null() {
            return Err(BridgeError::new("EPACKAGE", "Install the signed desktop app to set up bridge support."));
        }
        let authorization = Authorization::empty()?;
        authorization.check_right(b"com.apple.ServiceManagement.blesshelper\0", (1 << 0) | (1 << 1))?;
        let label = CFString::new(HELPER_LABEL);
        let mut error = ptr::null_mut();
        if unsafe { SMJobBless(kSMDomainSystemLaunchd, label.as_concrete_TypeRef(), authorization.0, &mut error) } != 0 { return Ok(()); }
        let detail = if error.is_null() { "Service Management returned no diagnostic.".into() }
            else { let error = unsafe { CFError::wrap_under_create_rule(error) }; format!("{} ({})", error.description(), error.code()) };
        Err(BridgeError { code: "EINSTALL".into(), message: "macOS could not install bridge support. Check that the app and helper are signed by the same developer.".into(), detail: Some(detail) })
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
        fn cancellation_is_distinct_from_denial() {
            assert_eq!(super::status(-60006).unwrap_err().code, "EAUTHCANCELLED");
            assert_eq!(super::status(-60005).unwrap_err().code, "EAUTHDENIED");
        }
    }
}
