//! Fixed macOS service identities. These values are derived by the privileged
//! helper from its authenticated caller and native adapter discovery.
use crate::model::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{ffi::CStr, path::PathBuf};

pub const INSTALL_ROOT: &str = "/Library/Application Support/eink-qemu/bridge";
pub const SOCKET_ROOT: &str = "/var/run/eink-qemu";
pub const JOURNAL_ROOT: &str = "/Library/Application Support/eink-qemu/bridge/journal";
pub const SOCKET_VMNET_VERSION: &str = "1.2.2";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalUser { uid: u32, gid: u32 }
impl LocalUser {
    /// Resolve the primary group from the local account database, never JSON.
    pub fn lookup(uid: u32) -> Result<Self, BridgeError> {
        let mut buffer = vec![0u8; 1024];
        loop {
            let mut account: libc::passwd = unsafe { std::mem::zeroed() };
            let mut result = std::ptr::null_mut();
            let rc = unsafe { libc::getpwuid_r(uid, &mut account, buffer.as_mut_ptr().cast(), buffer.len(), &mut result) };
            if rc == libc::ERANGE && buffer.len() < 65536 { buffer.resize(buffer.len() * 2, 0); continue; }
            if rc != 0 || result.is_null() || account.pw_uid != uid { return Err(BridgeError::new("EPERM", "Cannot resolve the local bridge user.")); }
            return Self::new(uid, account.pw_gid);
        }
    }
    pub fn new(uid: u32, gid: u32) -> Result<Self, BridgeError> {
        if !(501..0x80000000).contains(&uid) {
            return Err(BridgeError::new("EPERM", "Bridge setup requires an ordinary local user."));
        }
        Ok(Self { uid, gid })
    }
    pub fn uid(self) -> u32 { self.uid }
    pub fn gid(self) -> u32 { self.gid }
    pub fn acl_identity(self) -> Result<[u8; 16], BridgeError> {
        self.validate()?;
        extern "C" { fn mbr_uid_to_uuid(uid: u32, uuid: *mut u8) -> i32; }
        let mut uuid = [0u8; 16];
        if unsafe { mbr_uid_to_uuid(self.uid, uuid.as_mut_ptr()) } != 0 { return Err(unsafe_file()); }
        Ok(uuid)
    }
    fn validate(self) -> Result<(), BridgeError> { Self::new(self.uid, self.gid).map(|_| ()) }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceSpec {
    user: LocalUser,
    resource_id: ResourceId,
    adapter_id: AdapterId,
    fingerprint: String,
}
impl ServiceSpec {
    pub fn new(user: LocalUser, resource_id: ResourceId, adapter: &Adapter) -> Result<Self, BridgeError> {
        user.validate()?;
        let result = Self { user, resource_id, adapter_id: adapter.id.clone(), fingerprint: adapter.fingerprint.clone() };
        result.validate_adapter(std::slice::from_ref(adapter))?;
        Ok(result)
    }
    pub fn user(&self) -> LocalUser { self.user }
    pub fn resource_id(&self) -> &ResourceId { &self.resource_id }
    pub fn adapter_id(&self) -> &AdapterId { &self.adapter_id }
    pub fn fingerprint(&self) -> &str { &self.fingerprint }
    pub fn label(&self) -> String { format!("net.serialx.eink-qemu.bridge.u{}.{}", self.user.uid, self.resource_id.as_str()) }
    pub fn plist_path(&self) -> PathBuf { PathBuf::from("/Library/LaunchDaemons").join(format!("{}.plist", self.label())) }
    pub fn socket_directory(&self) -> PathBuf { PathBuf::from(SOCKET_ROOT).join(format!("u{}", self.user.uid)) }
    pub fn socket(&self) -> PathBuf { self.socket_directory().join(format!("{}.sock", self.resource_id.as_str())) }
    pub fn validate_adapter(&self, adapters: &[Adapter]) -> Result<(), BridgeError> {
        self.user.validate()?;
        let adapter = adapters.iter().find(|a| a.id == self.adapter_id)
            .ok_or_else(|| BridgeError::new("EDISCONNECTED", "The selected connection is disconnected."))?;
        if adapter.fingerprint != self.fingerprint { return Err(BridgeError::new("ESTALE", "The selected network adapter has changed.")); }
        if !adapter.eligible { return Err(BridgeError::new("EUNSUPPORTED", "macOS does not support bridging this connection.")); }
        if !adapter.link_up { return Err(BridgeError::new("EDISCONNECTED", "Connect the selected network adapter first.")); }
        Ok(())
    }
    pub fn plist(&self) -> Result<Vec<u8>, BridgeError> {
        self.user.validate()?;
        let arguments = vec![format!("{INSTALL_ROOT}/socket_vmnet"), "--vmnet-mode=bridged".into(),
            format!("--vmnet-interface={}", self.adapter_id.as_str()),
            format!("--socket-group={}", group_name(self.user.gid)?), self.socket().to_string_lossy().into_owned()];
        let mut dictionary = plist::Dictionary::new();
        dictionary.insert("Label".into(), self.label().into());
        dictionary.insert("ProgramArguments".into(), plist::Value::Array(arguments.into_iter().map(plist::Value::String).collect()));
        dictionary.insert("RunAtLoad".into(), true.into());
        dictionary.insert("KeepAlive".into(), true.into());
        dictionary.insert("ThrottleInterval".into(), 5u64.into());
        // UserName is explicit; socket_vmnet alone holds vmnet privileges.
        dictionary.insert("UserName".into(), "root".into());
        let mut bytes = Vec::new();
        plist::Value::Dictionary(dictionary).to_writer_xml(&mut bytes)
            .map_err(|e| BridgeError::new("EPLIST", e.to_string()))?;
        Ok(bytes)
    }
}

fn group_name(gid: u32) -> Result<String, BridgeError> {
    // Reentrant lookup: the helper may inspect two users concurrently.
    let mut buffer = vec![0u8; 1024];
    loop {
        let mut group: libc::group = unsafe { std::mem::zeroed() };
        let mut result = std::ptr::null_mut();
        let rc = unsafe { libc::getgrgid_r(gid, &mut group, buffer.as_mut_ptr().cast(), buffer.len(), &mut result) };
        if rc == libc::ERANGE && buffer.len() < 65536 { buffer.resize(buffer.len() * 2, 0); continue; }
        if rc != 0 || result.is_null() || group.gr_name.is_null() {
            return Err(BridgeError::new("EPERM", "Cannot resolve the bridge user's primary group."));
        }
        return unsafe { CStr::from_ptr(group.gr_name) }.to_str().map(str::to_owned)
            .map_err(|_| BridgeError::new("EPERM", "Invalid local group name."));
    }
}

#[derive(Debug, Clone, Copy)]
pub enum PayloadArchitecture { Arm64, X86_64 }
impl PayloadArchitecture {
    pub fn current() -> Self {
        #[cfg(target_arch = "aarch64")] { Self::Arm64 }
        #[cfg(not(target_arch = "aarch64"))] { Self::X86_64 }
    }
}
/// Hashes of the actual executables extracted from the official 1.2.2 archives.
/// Hashing the installed bytes also detects edits after an earlier installation.
pub fn verify_payload(bytes: &[u8], architecture: PayloadArchitecture) -> Result<(), BridgeError> {
    let expected = match architecture {
        PayloadArchitecture::Arm64 => "b8a72a62237312f2f756027dea504a844edeb40014702d4a320292c026d282b0",
        PayloadArchitecture::X86_64 => "526086c810df74a2465da6fd841a6e7a9a6a74679029dc76eee9dee2291b4929",
    };
    if bytes.len() > 4 * 1024 * 1024 || format!("{:x}", Sha256::digest(bytes)) != expected {
        return Err(BridgeError::new("EPAYLOAD", "Bridge support is missing or incompatible. Reinstall it from the app."));
    }
    Ok(())
}

use std::{fs::{self, File, OpenOptions}, io::{Read, Write, Seek}, os::{unix::{fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt}, ffi::OsStrExt}}, path::Path, process::{Command, Stdio}, time::{Duration, Instant}};
fn system_error(error: impl std::fmt::Display) -> BridgeError { BridgeError::new("ESERVICE", error.to_string()) }
fn unsafe_file() -> BridgeError { BridgeError::new("EPERM", "Bridge support has unsafe ownership or permissions.") }

fn protected_directory(path: &Path) -> Result<(), BridgeError> {
    crate::transport::unix::validate_system_directory(path)
}
pub fn validate_protected_file(path: &Path) -> Result<File, BridgeError> {
    let file = OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC).open(path).map_err(|_| unsafe_file())?;
    let meta = file.metadata().map_err(system_error)?;
    if !meta.is_file() || meta.uid() != 0 || meta.mode() & 0o022 != 0 || meta.nlink() != 1 { return Err(unsafe_file()); }
    crate::transport::unix::validate_no_acl_writes(&file)?;
    protected_directory(path.parent().ok_or_else(unsafe_file)?)?;
    Ok(file)
}
fn directory(path: &Path, mode: u32) -> Result<(), BridgeError> {
    protected_directory(path.parent().ok_or_else(unsafe_file)?)?;
    match fs::DirBuilder::new().mode(mode).create(path) {
        Ok(()) => (),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
        Err(e) => return Err(system_error(e)),
    }
    let meta = fs::symlink_metadata(path).map_err(system_error)?;
    if !meta.is_dir() || meta.uid() != 0 || meta.mode() & 0o777 != mode { return Err(unsafe_file()); }
    Ok(())
}
fn read_small(mut file: File) -> Result<Vec<u8>, BridgeError> {
    if file.metadata().map_err(system_error)?.len() > 4 * 1024 * 1024 { return Err(unsafe_file()); }
    let mut bytes = Vec::new();
    (&mut file).take(4 * 1024 * 1024 + 1).read_to_end(&mut bytes).map_err(system_error)?;
    if bytes.len() > 4 * 1024 * 1024 { return Err(unsafe_file()); }
    Ok(bytes)
}
/// Only fixed OS tools and arguments derived from validated native state call
/// this private runner. Temporary output files prevent pipe-buffer deadlocks.
pub(crate) fn command(program: &str, arguments: &[&str]) -> Result<(Option<i32>, String), BridgeError> {
    let mut output = tempfile::tempfile().map_err(system_error)?;
    let mut child = Command::new(program).args(arguments).env_clear().env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .stdin(Stdio::null()).stdout(output.try_clone().map_err(system_error)?).stderr(output.try_clone().map_err(system_error)?)
        .spawn().map_err(system_error)?;
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(status) = child.try_wait().map_err(system_error)? {
            output.rewind().map_err(system_error)?;
            let mut bytes = Vec::new();
            output.take(16384).read_to_end(&mut bytes).map_err(system_error)?;
            return Ok((status.code(), String::from_utf8_lossy(&bytes).into_owned()));
        }
        if Instant::now() >= deadline || output.metadata().map_err(system_error)?.len() > 1024 * 1024 {
            let _ = child.kill(); let _ = child.wait();
            return Err(BridgeError::new("ETIMEOUT", "The macOS bridge service did not respond in time."));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}
fn required(program: &str, args: &[&str]) -> Result<(), BridgeError> {
    let (status, detail) = command(program, args)?;
    if status == Some(0) { Ok(()) } else {
        Err(BridgeError { code: "ESERVICE".into(), message: "macOS could not update the bridge service.".into(),
            detail: Some(format!("{program} {}: {detail}", args.first().copied().unwrap_or(""))) })
    }
}
pub(crate) fn loaded_status(status: Option<i32>, detail: String) -> Result<bool, BridgeError> {
    match status {
        Some(0) => Ok(true),
        Some(113) => Ok(false), // launchctl: service not found in this domain
        _ => Err(BridgeError { code: "ESERVICE".into(), message: "macOS could not inspect the bridge service.".into(), detail: Some(format!("launchctl print ({status:?}): {detail}")) }),
    }
}
fn loaded(spec: &ServiceSpec) -> Result<bool, BridgeError> {
    let (status, detail) = command("/bin/launchctl", &["print", &format!("system/{}", spec.label())])?;
    loaded_status(status, detail)
}
fn stop(spec: &ServiceSpec) -> Result<(), BridgeError> {
    if !loaded(spec)? { return Ok(()); }
    let (_, detail) = command("/bin/launchctl", &["bootout", &format!("system/{}", spec.label())])?;
    // bootout can leave a terminating job visible briefly. Do not bootstrap a
    // replacement until that identity is absent. A racing second removal is
    // also successful only when the native job is actually gone.
    let deadline = Instant::now() + Duration::from_secs(8);
    while loaded(spec)? {
        if Instant::now() >= deadline {
            return Err(BridgeError { code: "ESERVICE".into(), message: "The previous bridge service has not stopped.".into(), detail: Some(detail) });
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

/// The helper constructs this only after authenticating/authorizing its caller.
/// No operation accepts a caller-supplied executable or installation path.
#[derive(Debug)]
pub struct NativeService { _private: () }
impl NativeService {
    pub fn new() -> Result<Self, BridgeError> {
        if unsafe { libc::geteuid() } != 0 { return Err(BridgeError::new("EAUTH", "Administrator authorization is required.")); }
        Ok(Self { _private: () })
    }
    pub fn prepare_helper_directories(&self) -> Result<(), BridgeError> {
        self.check_payload()?;
        directory(Path::new(JOURNAL_ROOT), 0o700)?;
        directory(Path::new(SOCKET_ROOT), 0o755)
    }
    pub fn install_payload(&self, bytes: &[u8]) -> Result<(), BridgeError> {
        verify_payload(bytes, PayloadArchitecture::current())?;
        directory(Path::new("/Library/Application Support/eink-qemu"), 0o755)?;
        directory(Path::new(INSTALL_ROOT), 0o755)?;
        let path = Path::new(INSTALL_ROOT).join("socket_vmnet");
        if path.try_exists().map_err(system_error)? {
            // Replacing a root-owned app payload is explicit repair; do not
            // follow a symlink or overwrite a hard-linked unrelated binary.
            validate_protected_file(&path)?;
        }
        let mut temp = tempfile::NamedTempFile::new_in(INSTALL_ROOT).map_err(system_error)?;
        temp.write_all(bytes).map_err(system_error)?;
        temp.as_file().set_permissions(fs::Permissions::from_mode(0o755)).map_err(system_error)?;
        temp.as_file().sync_all().map_err(system_error)?;
        temp.persist(&path).map_err(system_error)?;
        File::open(INSTALL_ROOT).and_then(|f| f.sync_all()).map_err(system_error)?;
        Ok(())
    }
    pub fn check_payload(&self) -> Result<(), BridgeError> {
        let file = validate_protected_file(&Path::new(INSTALL_ROOT).join("socket_vmnet"))?;
        verify_payload(&read_small(file)?, PayloadArchitecture::current())
    }
    /// Existing content must match the protected resource record exactly.
    /// Removal and repair never adopt a different launchd configuration.
    pub fn check_configuration(&self, spec: &ServiceSpec) -> Result<bool, BridgeError> {
        match fs::symlink_metadata(spec.plist_path()) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(system_error(e)),
            Ok(_) => {
                let bytes = read_small(validate_protected_file(&spec.plist_path())?)?;
                if bytes != spec.plist()? { return Err(BridgeError::new("ECONFLICT", "This bridge service was changed outside the app. It has not been overwritten.")); }
                Ok(true)
            },
        }
    }
    pub fn state(&self, spec: &ServiceSpec) -> Result<(bool, bool), BridgeError> {
        let configured = self.check_configuration(spec)?;
        let running = loaded(spec)?;
        if !configured && running { return Err(BridgeError::new("ECONFLICT", "An unrecorded service uses this bridge identity.")); }
        Ok((configured, running))
    }
    fn write_configuration(&self, spec: &ServiceSpec) -> Result<(), BridgeError> {
        if self.check_configuration(spec)? { return Ok(()); }
        let mut temp = tempfile::NamedTempFile::new_in("/Library/LaunchDaemons").map_err(system_error)?;
        temp.write_all(&spec.plist()?).map_err(system_error)?;
        temp.as_file().set_permissions(fs::Permissions::from_mode(0o644)).map_err(system_error)?;
        temp.as_file().sync_all().map_err(system_error)?;
        temp.persist_noclobber(spec.plist_path()).map_err(system_error)?;
        File::open("/Library/LaunchDaemons").and_then(|f| f.sync_all()).map_err(system_error)?;
        Ok(())
    }
    pub fn restore(&self, spec: &ServiceSpec, configured: bool, running: bool) -> Result<(), BridgeError> {
        if running && !configured { return Err(BridgeError::new("ECONFLICT", "Invalid prior bridge service state.")); }
        if running { return self.start(spec); }
        self.remove(spec)?;
        if configured { self.write_configuration(spec)?; }
        Ok(())
    }
    pub fn start(&self, spec: &ServiceSpec) -> Result<(), BridgeError> {
        self.check_payload()?;
        spec.validate_adapter(&crate::inspect()?.adapters)?;
        let exists = self.check_configuration(spec)?;
        let active = loaded(spec)?;
        if !exists && active { return Err(BridgeError::new("ECONFLICT", "A service already uses this bridge identity.")); }
        directory(Path::new(SOCKET_ROOT), 0o755)?;
        directory(&spec.socket_directory(), 0o700)?;
        // This directory is root-owned and dedicated to this UID. Preserve the
        // 0700 mode while granting only that user traversal, including after
        // socket_vmnet recreates its staff-group socket on restart.
        set_search_acl(&File::open(spec.socket_directory()).map_err(system_error)?, spec.user)?;
        if !exists { self.write_configuration(spec)?; }
        if active { stop(spec)?; }
        // socket_vmnet unlinks its own old socket before binding. Confirm that
        // an existing leaf cannot refer to a user file or another file type.
        self.check_socket_leaf(spec, false)?;
        required("/bin/launchctl", &["bootstrap", "system", spec.plist_path().to_str().ok_or_else(unsafe_file)?])?;
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            match self.verify(spec) {
                Ok(()) => return Ok(()),
                Err(e) if Instant::now() >= deadline => return Err(e),
                Err(_) => std::thread::sleep(Duration::from_millis(100)),
            }
        }
    }
    /// /var/run is cleared at boot. Recreate only this volatile state for an
    /// unchanged committed service. Existing state is verified, not rewritten.
    pub fn resume(&self, spec: &ServiceSpec) -> Result<(), BridgeError> {
        if !self.check_configuration(spec)? {
            return Err(BridgeError::new("ENEEDSREPAIR", "The saved bridge service configuration is missing."));
        }
        match fs::symlink_metadata(spec.socket_directory()) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => self.start(spec),
            Err(error) => Err(system_error(error)),
            Ok(meta) => {
                if !meta.is_dir() || meta.uid() != 0 || meta.mode() & 0o777 != 0o700 { return Err(unsafe_file()); }
                protected_directory(&spec.socket_directory())?;
                verify_search_acl(&File::open(spec.socket_directory()).map_err(system_error)?, spec.user)?;
                self.check_socket_leaf(spec, false)?;
                // Several committed services can share the same user's runtime
                // directory. Another service may already have recreated it.
                if !spec.socket().try_exists().map_err(system_error)? || !loaded(spec)? { self.start(spec) }
                else { self.verify(spec) }
            },
        }
    }
    fn check_socket_leaf(&self, spec: &ServiceSpec, required: bool) -> Result<(), BridgeError> {
        match fs::symlink_metadata(spec.socket()) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && !required => Ok(()),
            Err(e) => Err(system_error(e)),
            Ok(meta) if meta.file_type().is_socket() && meta.uid() == 0 => Ok(()),
            Ok(_) => Err(unsafe_file()),
        }
    }
    pub fn verify(&self, spec: &ServiceSpec) -> Result<(), BridgeError> {
        spec.validate_adapter(&crate::inspect()?.adapters)?;
        self.check_payload()?;
        if !self.check_configuration(spec)? || !loaded(spec)? { return Err(BridgeError::new("ENEEDSREPAIR", "The bridge service is not running.")); }
        let meta = fs::symlink_metadata(spec.socket_directory()).map_err(system_error)?;
        if !meta.is_dir() || meta.uid() != 0 || meta.mode() & 0o777 != 0o700 { return Err(unsafe_file()); }
        protected_directory(&spec.socket_directory())?;
        verify_search_acl(&File::open(spec.socket_directory()).map_err(system_error)?, spec.user)?;
        self.check_socket_leaf(spec, true)?;
        open_as_user(&spec.socket(), spec.user)
    }
    pub fn remove(&self, spec: &ServiceSpec) -> Result<(), BridgeError> {
        let exists = self.check_configuration(spec)?;
        let active = loaded(spec)?;
        if !exists && active { return Err(BridgeError::new("ECONFLICT", "The bridge service configuration is missing. Manual changes have been preserved.")); }
        if active { stop(spec)?; }
        if exists { fs::remove_file(spec.plist_path()).map_err(system_error)?; }
        if spec.socket_directory().try_exists().map_err(system_error)? {
            protected_directory(&spec.socket_directory())?;
            self.check_socket_leaf(spec, false)?;
            match fs::remove_file(spec.socket()) { Ok(()) => (), Err(e) if e.kind() == std::io::ErrorKind::NotFound => (), Err(e) => return Err(system_error(e)) }
            // Other resources and the helper's shared endpoint remain intact.
            match fs::remove_dir(spec.socket_directory()) {
                Ok(()) => (),
                Err(e) if matches!(e.raw_os_error(), Some(libc::ENOTEMPTY | libc::ENOENT)) => (),
                Err(e) => return Err(system_error(e)),
            }
        }
        Ok(())
    }
}

/// fork's child uses only stack data and async-signal-safe libc calls. It drops
/// all supplementary groups, then permanently drops gid/uid before opening the
/// transport. It neither runs a shell nor changes the multithreaded parent's
/// effective identity. The parent bounds blocked socket connects to five seconds.
fn open_as_user(path: &Path, user: LocalUser) -> Result<(), BridgeError> {
    user.validate()?;
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    let bytes = path.as_os_str().as_bytes();
    if bytes.len() >= address.sun_path.len() { return Err(unsafe_file()); }
    address.sun_family = libc::AF_UNIX as _;
    address.sun_len = std::mem::size_of_val(&address) as _;
    for (dest, byte) in address.sun_path.iter_mut().zip(bytes) { *dest = *byte as _; }
    let pid = unsafe { libc::fork() };
    if pid < 0 { return Err(system_error(std::io::Error::last_os_error())); }
    if pid == 0 {
        unsafe {
            if libc::setgroups(1, &user.gid) != 0 || libc::setgid(user.gid) != 0 || libc::setuid(user.uid) != 0 { libc::_exit(1); }
            let fd = libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0);
            if fd < 0 { libc::_exit(2); }
            if libc::connect(fd, (&address as *const libc::sockaddr_un).cast(), std::mem::size_of_val(&address) as _) != 0 { libc::_exit(3); }
            let mut peer_uid = 1; let mut peer_gid = 0;
            let result = libc::getpeereid(fd, &mut peer_uid, &mut peer_gid);
            libc::close(fd);
            libc::_exit(if result == 0 && peer_uid == 0 { 0 } else { 4 });
        }
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let mut status = 0;
        let waited = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
        if waited == pid {
            return if libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0 { Ok(()) }
                else { Err(BridgeError::new("EACCESS", "The bridge packet socket cannot be opened by your user account.")) };
        }
        if waited < 0 { return Err(system_error(std::io::Error::last_os_error())); }
        if Instant::now() >= deadline {
            unsafe { libc::kill(pid, libc::SIGKILL); libc::waitpid(pid, &mut status, 0); }
            return Err(BridgeError::new("ETIMEOUT", "The bridge packet socket did not respond."));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Replace the app-owned user's directory ACL with one traversal-only entry.
/// Native UUID qualifiers avoid account-name parsing and shell conventions.
pub fn set_search_acl(file: &File, user: LocalUser) -> Result<(), BridgeError> {
    use std::{ffi::c_void, os::fd::AsRawFd};
    extern "C" {
        fn acl_init(count: i32) -> *mut c_void;
        fn acl_free(acl: *mut c_void) -> i32;
        fn acl_create_entry(acl: *mut *mut c_void, entry: *mut *mut c_void) -> i32;
        fn acl_set_tag_type(entry: *mut c_void, tag: i32) -> i32;
        fn acl_set_qualifier(entry: *mut c_void, qualifier: *const c_void) -> i32;
        fn acl_get_permset(entry: *mut c_void, set: *mut *mut c_void) -> i32;
        fn acl_add_perm(set: *mut c_void, permission: i32) -> i32;
        fn acl_set_permset(entry: *mut c_void, set: *mut c_void) -> i32;
        fn acl_set_fd_np(fd: i32, acl: *mut c_void, kind: i32) -> i32;
    }
    let uuid = user.acl_identity()?;
    struct Acl(*mut c_void);
    impl Drop for Acl { fn drop(&mut self) { unsafe { acl_free(self.0); } } }
    let mut acl = Acl(unsafe { acl_init(1) });
    if acl.0.is_null() { return Err(unsafe_file()); }
    let mut entry = std::ptr::null_mut(); let mut set = std::ptr::null_mut();
    let failed = unsafe {
        acl_create_entry(&mut acl.0, &mut entry) != 0 || acl_set_tag_type(entry, 1) != 0 ||
        acl_set_qualifier(entry, uuid.as_ptr().cast()) != 0 || acl_get_permset(entry, &mut set) != 0 ||
        acl_add_perm(set, 1 << 3) != 0 || acl_set_permset(entry, set) != 0 || acl_set_fd_np(file.as_raw_fd(), acl.0, 0x100) != 0
    };
    if failed { return Err(system_error(std::io::Error::last_os_error())); }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn launchd_absence_is_distinct_from_permission_or_service_failure() {
        assert_eq!(loaded_status(Some(0), String::new()).unwrap(), true);
        assert_eq!(loaded_status(Some(113), "Could not find service".into()).unwrap(), false);
        for status in [Some(1), Some(5), Some(64), None] {
            let error = loaded_status(status, "native failure detail".into()).unwrap_err();
            assert_eq!(error.code, "ESERVICE");
            assert!(error.detail.unwrap().contains("native failure detail"));
        }
    }
    #[test]
    fn native_command_failure_retains_diagnostic_output() {
        let error = required("/usr/bin/stat", &["/eink-bridge-no-such-native-test-file"]).unwrap_err();
        assert!(error.detail.as_deref().is_some_and(|text| text.contains("eink-bridge-no-such-native-test-file")));
    }
}

/// Check the complete directory ACL, not merely absence of write grants:
/// another user's traversal grant would expose the staff-group packet socket.
pub fn verify_search_acl(file: &File, user: LocalUser) -> Result<(), BridgeError> {
    use std::{ffi::c_void, os::fd::AsRawFd};
    extern "C" {
        fn acl_get_fd_np(fd: i32, kind: i32) -> *mut c_void;
        fn acl_free(acl: *mut c_void) -> i32;
        fn acl_get_entry(acl: *mut c_void, index: i32, entry: *mut *mut c_void) -> i32;
        fn acl_get_tag_type(entry: *mut c_void, tag: *mut i32) -> i32;
        fn acl_get_qualifier(entry: *mut c_void) -> *mut c_void;
        fn acl_get_permset_mask_np(entry: *mut c_void, mask: *mut u64) -> i32;
    }
    struct Allocation(*mut c_void);
    impl Drop for Allocation { fn drop(&mut self) { unsafe { acl_free(self.0); } } }
    let expected = user.acl_identity()?;
    let acl = Allocation(unsafe { acl_get_fd_np(file.as_raw_fd(), 0x100) });
    if acl.0.is_null() { return Err(unsafe_file()); }
    let mut entry = std::ptr::null_mut();
    if unsafe { acl_get_entry(acl.0, 0, &mut entry) } != 0 { return Err(unsafe_file()); }
    let mut tag = 0; let mut mask = 0;
    if unsafe { acl_get_tag_type(entry, &mut tag) } != 0 || unsafe { acl_get_permset_mask_np(entry, &mut mask) } != 0 || tag != 1 || mask != 1 << 3 {
        return Err(unsafe_file());
    }
    let qualifier = Allocation(unsafe { acl_get_qualifier(entry) });
    if qualifier.0.is_null() || unsafe { std::slice::from_raw_parts(qualifier.0.cast::<u8>(), 16) } != expected { return Err(unsafe_file()); }
    if unsafe { acl_get_entry(acl.0, 1, &mut entry) } == 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::EINVAL) { return Err(unsafe_file()); }
    Ok(())
}
