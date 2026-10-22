//! Explicit privileged support removal; never an ordinary helper IPC request.
use crate::model::BridgeError;
use crate::{authorization::macos::HELPER_LABEL, coordinator::Coordinator,
    macos_platform::MacPlatform, macos_service::{validate_protected_file, NativeService, INSTALL_ROOT, JOURNAL_ROOT, SOCKET_ROOT}, model::OperationId};
use serde::{Deserialize, Serialize};
use std::{fs::{self, File, OpenOptions}, io::{Read, Write},
    os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf}, time::{Duration, Instant}};
pub const HELPER_PATH: &str = "/Library/PrivilegedHelperTools/net.serialx.eink-qemu.bridge-helper";
pub const HELPER_PLIST: &str = "/Library/LaunchDaemons/net.serialx.eink-qemu.bridge-helper.plist";
pub const MAINTENANCE_LABEL: &str = "net.serialx.eink-qemu.bridge-maintenance";
pub const RESULT_PATH: &str = "/Library/Application Support/eink-qemu/bridge/maintenance-result.json";

/// Verify the fixed, root-owned executable against this signed app's payload
/// before asking Service Management to execute it with administrator rights.
pub fn verify_installed_helper(requirement: &str) -> Result<(), BridgeError> {
    let executable = std::env::current_exe().map_err(failure)?;
    let contents = executable.parent().and_then(Path::parent)
        .ok_or_else(|| BridgeError::new("EPACKAGE", "The signed app bundle is unavailable."))?;
    let bundled = contents.join("Library/LaunchServices").join(HELPER_LABEL);
    let installed = read_protected(Path::new(HELPER_PATH), 64 * 1024 * 1024)?;
    let payload = fs::read(bundled).map_err(|_| BridgeError::new("EPACKAGE", "The signed app's bridge helper is missing."))?;
    if installed != payload {
        return Err(BridgeError::new("EUPDATE", "Update bridge support from this app before uninstalling it."));
    }
    let requirement = format!("={requirement}");
    let (code, _) = crate::macos_service::command("/usr/bin/codesign", &["--verify", "--strict", "-R", &requirement, HELPER_PATH])?;
    if code != Some(0) { return Err(BridgeError::new("ESIGNATURE", "The installed bridge helper's signature is invalid. Its files have been preserved.")); }
    Ok(())
}

fn failure(error: impl std::fmt::Display) -> BridgeError { BridgeError::new("ECLEANUP", error.to_string()) }
fn conflict() -> BridgeError { BridgeError::new("ECONFLICT", "Bridge support was changed outside the app. Its files have been preserved.") }
fn validate_helper_plist(bytes: &[u8]) -> Result<(), BridgeError> {
    let value = plist::Value::from_reader(std::io::Cursor::new(bytes)).map_err(|_| conflict())?;
    let mut expected = plist::Dictionary::new();
    expected.insert("Label".into(), HELPER_LABEL.into());
    expected.insert("Program".into(), HELPER_PATH.into());
    expected.insert("ProgramArguments".into(), plist::Value::Array(vec![HELPER_PATH.into()]));
    expected.insert("RunAtLoad".into(), true.into());
    expected.insert("KeepAlive".into(), true.into());
    expected.insert("ThrottleInterval".into(), 5u64.into());
    expected.insert("UserName".into(), "root".into());
    if value != plist::Value::Dictionary(expected) { return Err(conflict()); }
    Ok(())
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaintenanceResult { pub id: OperationId, pub result: Result<(), BridgeError> }

fn read_protected(path: &Path, limit: u64) -> Result<Vec<u8>, BridgeError> {
    let file = validate_protected_file(path)?;
    if file.metadata().map_err(failure)?.len() > limit { return Err(conflict()); }
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes).map_err(failure)?;
    if bytes.len() as u64 > limit { return Err(conflict()); }
    Ok(bytes)
}

pub fn read_result(id: &OperationId) -> Result<Option<Result<(), BridgeError>>, BridgeError> {
    match fs::symlink_metadata(RESULT_PATH) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(failure(e)),
        Ok(_) => (),
    }
    let result: MaintenanceResult = serde_json::from_slice(&read_protected(Path::new(RESULT_PATH), 65536)?)
        .map_err(failure)?;
    Ok((result.id == *id).then_some(result.result))
}

fn write_result(value: &MaintenanceResult) -> Result<(), BridgeError> {
    // The result contains no authorization credential or caller-supplied path.
    // Keep the protected journal for diagnostics and future reinstallation.
    crate::transport::unix::validate_system_directory(Path::new(INSTALL_ROOT))?;
    if fs::symlink_metadata(RESULT_PATH).is_ok() { validate_protected_file(Path::new(RESULT_PATH))?; }
    let mut file = tempfile::NamedTempFile::new_in(INSTALL_ROOT).map_err(failure)?;
    file.write_all(&serde_json::to_vec(value).map_err(failure)?).map_err(failure)?;
    file.as_file().set_permissions(fs::Permissions::from_mode(0o644)).map_err(failure)?;
    file.as_file().sync_all().map_err(failure)?;
    file.persist(RESULT_PATH).map_err(failure)?;
    File::open(INSTALL_ROOT).and_then(|file| file.sync_all()).map_err(failure)
}

fn protected_lock(name: &str) -> Result<File, BridgeError> {
    crate::transport::unix::validate_system_directory(Path::new(JOURNAL_ROOT))?;
    let path = Path::new(JOURNAL_ROOT).join(name);
    let file = OpenOptions::new().read(true).write(true).create(true).mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC).open(&path).map_err(failure)?;
    validate_protected_file(&path)?;
    file.try_lock().map_err(|_| BridgeError::new("EBUSY", "Bridge support is busy. Wait for its current operation to finish."))?;
    Ok(file)
}

/// Serialize signed app instances across accounts, including the very first
/// install, before any authorization prompt. The stable root-owned directory
/// already exists before support is installed and is never deleted by removal.
/// This advisory lock changes no directory contents or permissions. Using a
/// freshly created lock file would leave a first-install/uninstall race.
pub fn client_gate() -> Result<File, BridgeError> {
    let root = Path::new("/Library");
    crate::transport::unix::validate_system_directory(root)?;
    lock_directory(root)
}
fn lock_directory(path: &Path) -> Result<File, BridgeError> {
    let file = OpenOptions::new().read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_DIRECTORY)
        .open(path).map_err(failure)?;
    file.try_lock().map_err(|_| BridgeError::new("EBUSY",
        "Another app is installing or removing bridge support. Finish that operation first."))?;
    Ok(file)
}

struct SupportFile { path: PathBuf, bytes: Vec<u8>, mode: u32 }
fn restore_files(files: &[SupportFile], mut read: impl FnMut(&Path) -> Result<Vec<u8>, BridgeError>) -> Result<(), BridgeError> {
    // Check every survivor first. Never overwrite outside edits or symlinks.
    let mut missing = Vec::new();
    for file in files {
        match fs::symlink_metadata(&file.path) {
            Ok(_) => if read(&file.path)? != file.bytes { return Err(conflict()); },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => missing.push(file),
            Err(e) => return Err(failure(e)),
        }
    }
    for file in missing {
        let parent = file.path.parent().ok_or_else(conflict)?;
        let mut temp = tempfile::NamedTempFile::new_in(parent).map_err(failure)?;
        temp.write_all(&file.bytes).map_err(failure)?;
        temp.as_file().set_permissions(fs::Permissions::from_mode(file.mode)).map_err(failure)?;
        temp.as_file().sync_all().map_err(failure)?;
        // A concurrent administrator edit must win over our recovery snapshot.
        temp.persist_noclobber(&file.path).map_err(failure)?;
        File::open(parent).and_then(|f| f.sync_all()).map_err(failure)?;
    }
    Ok(())
}

/// A launchd job authorized through Service Management invokes this fixed mode.
/// Ordinary requests cannot invoke all-owner cleanup or choose installation paths.
pub fn run_privileged(id: &str) -> Result<(), BridgeError> {
    if unsafe { libc::geteuid() } != 0 { return Err(BridgeError::new("EPERM", "Administrator authorization is required to uninstall bridge support.")); }
    let id = OperationId::new(id)?;
    let _maintenance = protected_lock("maintenance.lock")?;
    let mut native = NativeMaintenance { files: Vec::new(), service_lock: None, removing_support: false };
    let result = remove_with(&mut native);
    write_result(&MaintenanceResult { id, result: result.clone() })?;
    result
}

struct NativeMaintenance { files: Vec<SupportFile>, service_lock: Option<File>, removing_support: bool }
fn loaded() -> Result<bool, BridgeError> {
    let (status, detail) = crate::macos_service::command("/bin/launchctl", &["print", &format!("system/{HELPER_LABEL}")])?;
    crate::macos_service::loaded_status(status, detail)
}
impl Maintenance for NativeMaintenance {
    fn preflight(&mut self) -> Result<(), BridgeError> {
        let executable = std::env::current_exe().map_err(failure)?;
        let running = fs::metadata(&executable).map_err(failure)?;
        let installed = validate_protected_file(Path::new(HELPER_PATH))?.metadata().map_err(failure)?;
        if running.dev() != installed.dev() || running.ino() != installed.ino() { return Err(conflict()); }
        NativeService::new()?.check_payload()?;
        for path in [HELPER_PATH, HELPER_PLIST, "/Library/Application Support/eink-qemu/bridge/socket_vmnet"] {
            let mode = validate_protected_file(Path::new(path))?.metadata().map_err(failure)?.mode() & 0o777;
            self.files.push(SupportFile { path: path.into(), bytes: read_protected(Path::new(path), 64 * 1024 * 1024)?, mode });
        }
        validate_helper_plist(&self.files[1].bytes)?;
        Ok(())
    }
    fn stop(&mut self) -> Result<(), BridgeError> {
        if loaded()? {
            crate::macos_service::command("/bin/launchctl", &["bootout", &format!("system/{HELPER_LABEL}")])?;
        }
        let deadline = Instant::now() + Duration::from_secs(15);
        while loaded()? {
            if Instant::now() >= deadline { return Err(BridgeError::new("EBUSY", "Bridge support has not stopped yet.")); }
            std::thread::sleep(Duration::from_millis(100));
        }
        self.service_lock = Some(protected_lock("service.lock")?);
        Ok(())
    }
    fn clean_resources(&mut self) -> Result<(), BridgeError> {
        let mut coordinator = Coordinator::open(Path::new(JOURNAL_ROOT), "service", MacPlatform::for_recovery()?)?;
        coordinator.uninstall(|resource| {
            let user = resource.original.service().user();
            if resource.owner != format!("uid:{}", user.uid()) { return Err(conflict()); }
            MacPlatform::for_user(user)
        }, crate::macos_server::now)
    }
    fn remove_support(&mut self) -> Result<(), BridgeError> {
        // Revalidate every captured file before removing any installation file.
        for file in &self.files {
            if read_protected(&file.path, 64 * 1024 * 1024)? != file.bytes { return Err(conflict()); }
        }
        let socket = Path::new(SOCKET_ROOT).join("bridge.sock");
        match fs::symlink_metadata(&socket) {
            Ok(meta) if meta.uid() == 0 && meta.file_type().is_socket() => fs::remove_file(&socket).map_err(failure)?,
            Ok(_) => return Err(conflict()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(failure(e)),
        }
        self.removing_support = true;
        for file in self.files.iter().rev() { fs::remove_file(&file.path).map_err(failure)?; }
        for path in [INSTALL_ROOT, "/Library/LaunchDaemons", "/Library/PrivilegedHelperTools"] {
            File::open(path).and_then(|file| file.sync_all()).map_err(failure)?;
        }
        Ok(())
    }
    fn restart(&mut self) -> Result<(), BridgeError> {
        if self.removing_support {
            for file in &self.files {
                crate::transport::unix::validate_system_directory(file.path.parent().ok_or_else(conflict)?)?;
            }
            restore_files(&self.files, |path| read_protected(path, 64 * 1024 * 1024))?;
        }
        for file in &self.files {
            if read_protected(&file.path, 64 * 1024 * 1024)? != file.bytes { return Err(conflict()); }
        }
        self.service_lock.take();
        if loaded()? { return Ok(()); }
        let (code, diagnostic) = crate::macos_service::command("/bin/launchctl", &["bootstrap", "system", HELPER_PLIST])?;
        if code != Some(0) { return Err(failure(diagnostic)); }
        Ok(())
    }
}

trait Maintenance {
    fn preflight(&mut self) -> Result<(), BridgeError>;
    fn stop(&mut self) -> Result<(), BridgeError>;
    fn clean_resources(&mut self) -> Result<(), BridgeError>;
    fn remove_support(&mut self) -> Result<(), BridgeError>;
    fn restart(&mut self) -> Result<(), BridgeError>;
}

fn remove_with(backend: &mut impl Maintenance) -> Result<(), BridgeError> {
    backend.preflight()?;
    let result = backend.stop().and_then(|_| backend.clean_resources())
        .and_then(|_| backend.remove_support());
    if let Err(error) = result {
        return match backend.restart() {
            Ok(()) => Err(error),
            Err(restart) => Err(BridgeError { code: "ECLEANUP".into(),
                message: "Bridge support could not be removed or restarted. Keep the app installed and reopen setup to repair it.".into(),
                detail: Some(format!("{error}; {restart}")) }),
        };
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Default)]
    struct Host { calls: Vec<&'static str>, failure: Option<&'static str> }
    impl Host {
        fn step(&mut self, name: &'static str) -> Result<(), BridgeError> {
            self.calls.push(name);
            if self.failure == Some(name) { Err(BridgeError::new("ECONFLICT", name)) } else { Ok(()) }
        }
    }
    impl Maintenance for Host {
        fn preflight(&mut self) -> Result<(), BridgeError> { self.step("preflight") }
        fn stop(&mut self) -> Result<(), BridgeError> { self.step("stop") }
        fn clean_resources(&mut self) -> Result<(), BridgeError> { self.step("resources") }
        fn remove_support(&mut self) -> Result<(), BridgeError> { self.step("support") }
        fn restart(&mut self) -> Result<(), BridgeError> { self.step("restart") }
    }
    #[test]
    fn support_files_are_removed_only_after_quiescence_and_resource_cleanup() {
        let mut host = Host::default();
        remove_with(&mut host).unwrap();
        assert_eq!(host.calls, ["preflight", "stop", "resources", "support"]);
    }
    #[test]
    fn cleanup_refusal_restores_service_and_preserves_support() {
        let mut host = Host { failure: Some("resources"), ..Host::default() };
        assert_eq!(remove_with(&mut host).unwrap_err().code, "ECONFLICT");
        assert_eq!(host.calls, ["preflight", "stop", "resources", "restart"]);
    }
    #[test]
    fn preflight_failure_does_not_stop_helper() {
        let mut host = Host { failure: Some("preflight"), ..Host::default() };
        assert!(remove_with(&mut host).is_err());
        assert_eq!(host.calls, ["preflight"]);
    }
    #[test]
    fn failed_stop_attempt_restores_helper() {
        let mut host = Host { failure: Some("stop"), ..Host::default() };
        assert!(remove_with(&mut host).is_err());
        assert_eq!(host.calls, ["preflight", "stop", "restart"]);
    }
    #[test]
    fn edited_helper_jobs_cannot_be_removed() {
        let valid = format!(r#"<plist version="1.0"><dict>
            <key>Label</key><string>net.serialx.eink-qemu.bridge-helper</string>
            <key>Program</key><string>{HELPER_PATH}</string>
            <key>ProgramArguments</key><array><string>{HELPER_PATH}</string></array>
            <key>KeepAlive</key><true/><key>RunAtLoad</key><true/>
            <key>ThrottleInterval</key><integer>5</integer><key>UserName</key><string>root</string>
            </dict></plist>"#);
        validate_helper_plist(valid.as_bytes()).unwrap();
        for changed in [valid.replace(HELPER_PATH, "/tmp/other"),
            valid.replace("<true/>", "<false/>"),
            valid.replace("</dict>", "<key>EnvironmentVariables</key><dict/></dict>")] {
            assert_eq!(validate_helper_plist(changed.as_bytes()).unwrap_err().code, "ECONFLICT");
        }
    }
    #[test]
    fn maintenance_gate_excludes_another_client_until_authorization_finishes() {
        let root = tempfile::tempdir().unwrap();
        let first = lock_directory(root.path()).unwrap();
        assert_eq!(lock_directory(root.path()).unwrap_err().code, "EBUSY");
        drop(first);
        lock_directory(root.path()).unwrap();
    }
    #[test]
    fn partial_deletion_restores_missing_files_and_preserves_modes() {
        let root = tempfile::tempdir().unwrap();
        let one = root.path().join("helper");
        let two = root.path().join("payload");
        fs::write(&one, b"helper").unwrap();
        let files = vec![
            SupportFile { path: one.clone(), bytes: b"helper".to_vec(), mode: 0o755 },
            SupportFile { path: two.clone(), bytes: b"payload".to_vec(), mode: 0o755 },
        ];
        restore_files(&files, |path| fs::read(path).map_err(failure)).unwrap();
        assert_eq!(fs::read(&two).unwrap(), b"payload");
        assert_eq!(fs::metadata(&two).unwrap().mode() & 0o777, 0o755);
        fs::write(&one, b"external edit").unwrap();
        fs::remove_file(&two).unwrap();
        assert_eq!(restore_files(&files, |path| fs::read(path).map_err(failure)).unwrap_err().code, "ECONFLICT");
        assert!(!two.exists(), "validate surviving files before restoring any file");
        assert_eq!(fs::read(&one).unwrap(), b"external edit");
    }
    #[test]
    fn ordinary_process_cannot_invoke_privileged_removal() {
        if unsafe { libc::geteuid() } != 0 {
            assert_eq!(run_privileged("0123456789abcdef0123456789abcdef").unwrap_err().code, "EPERM");
        }
    }
}
