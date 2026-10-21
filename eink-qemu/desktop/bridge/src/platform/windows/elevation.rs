//! Desktop-side UAC handoff. Only the fixed installed helper is elevated; the
//! desktop and QEMU remain ordinary processes. Approval travels over the same
//! authenticated pipe and never grants arbitrary command execution.
use crate::{authorization::AuthorizationProof, model::{BridgeError, OperationId, VERSION},
    protocol::{HelperRequest, ReplyKind, RequestKind}, transport::HelperClient, windows_security};
use std::{ffi::OsString, fs::{File, OpenOptions}, os::windows::{ffi::{OsStrExt, OsStringExt}, fs::OpenOptionsExt,
    io::{AsRawHandle, FromRawHandle, OwnedHandle}}, path::{Path, PathBuf}, ptr};
use windows_sys::Win32::{Foundation::*, Storage::FileSystem::*, System::{Com::{CoTaskMemFree, CoInitializeEx, CoUninitialize,
    COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE}, Threading::*},
    UI::{Shell::*, WindowsAndMessaging::SW_HIDE}};

fn known_folder(id: &windows_sys::core::GUID) -> Result<PathBuf, BridgeError> {
    let mut value = ptr::null_mut();
    if unsafe { SHGetKnownFolderPath(id, 0, ptr::null_mut(), &mut value) } < 0 || value.is_null() {
        return Err(BridgeError::new("EINSTALL", "Windows could not locate bridge support's system directory."));
    }
    struct Allocation(*mut u16);
    impl Drop for Allocation { fn drop(&mut self) { unsafe { CoTaskMemFree(self.0.cast()); } } }
    let _allocation = Allocation(value);
    let mut length = 0;
    while length < 32768 && unsafe { *value.add(length) } != 0 { length += 1; }
    if length == 32768 { return Err(BridgeError::new("EINSTALL", "Invalid Windows system directory.")); }
    let path = PathBuf::from(OsString::from_wide(unsafe { std::slice::from_raw_parts(value, length) }));
    if !path.is_absolute() { return Err(BridgeError::new("EINSTALL", "Invalid Windows system directory.")); }
    Ok(path)
}
pub fn installation_root() -> Result<PathBuf, BridgeError> { Ok(known_folder(&FOLDERID_ProgramFilesX64)?.join("eink-qemu/bridge")) }
pub fn journal_root() -> Result<PathBuf, BridgeError> { Ok(known_folder(&FOLDERID_ProgramData)?.join("eink-qemu/bridge")) }
fn missing() -> BridgeError { BridgeError::new("ENEEDSSETUP", "Install or update Windows bridge support before configuring the connection.") }
struct InstalledHelper { path: PathBuf, _locks: Vec<File> }
impl InstalledHelper {
    fn open() -> Result<Self, BridgeError> { Self::open_at(&installation_root()?) }
    fn open_at(root: &Path) -> Result<Self, BridgeError> {
        if !root.is_absolute() || root.components().any(|c| matches!(c, std::path::Component::ParentDir)) { return Err(missing()); }
        let mut locks = Vec::new();
        // Keep every ancestor pinned against rename and reject reparse points.
        // ProgramData/drive roots can legitimately permit creating unrelated
        // children; the actual installation directory and file must be trusted.
        for ancestor in root.ancestors().collect::<Vec<_>>().into_iter().rev() {
            let file = OpenOptions::new().read(true).share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT).open(ancestor).map_err(|_| missing())?;
            let mut info = BY_HANDLE_FILE_INFORMATION::default();
            if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0
                || info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 || info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY == 0 { return Err(missing()); }
            if ancestor == root { windows_security::validate_handle(&file, None)?; }
            locks.push(file);
        }
        let path = root.join("eink-bridge-helper.exe");
        let file = OpenOptions::new().read(true).share_mode(FILE_SHARE_READ).custom_flags(FILE_FLAG_OPEN_REPARSE_POINT).open(&path).map_err(|_| missing())?;
        windows_security::validate_handle(&file, None)?;
        if !file.metadata().map_err(|_| missing())?.is_file() { return Err(missing()); }
        locks.push(file);
        Ok(Self { path, _locks: locks })
    }
}
fn wide(value: &str) -> Vec<u16> { value.encode_utf16().chain(Some(0)).collect() }
fn status(code: u32) -> BridgeError {
    if code == ERROR_CANCELLED { BridgeError::new("EAUTHCANCELLED", "Windows administrator approval was cancelled.") }
    else { BridgeError::new("EAUTHDENIED", "Windows administrator approval did not complete. Reopen bridge setup to retry.") }
}
/// Call from the ordinary desktop's blocking task after its plan review. The
/// service binds the returned ticket to this process, not the elevated helper.
pub fn request(plan_id: OperationId) -> Result<AuthorizationProof, BridgeError> {
    let helper = InstalledHelper::open()?;
    let reply = HelperClient::connect()?.request(HelperRequest { version: VERSION, request: RequestKind::BeginAuthorization { plan_id } })?;
    let ReplyKind::Authorization { authorization } = reply.reply else { return Err(status(0)); };
    // A reused runtime worker may already have an incompatible COM apartment.
    // Give the shell its own initialized STA for the administrator prompt.
    std::thread::Builder::new().name("bridge-approval".into()).spawn(move || launch(helper, authorization))
        .map_err(|_| status(0))?.join().map_err(|_| status(0))?
}
fn launch(helper: InstalledHelper, authorization: AuthorizationProof) -> Result<AuthorizationProof, BridgeError> {
    if unsafe { CoInitializeEx(ptr::null(), (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32) } < 0 { return Err(status(0)); }
    struct Apartment;
    impl Drop for Apartment { fn drop(&mut self) { unsafe { CoUninitialize(); } } }
    let _apartment = Apartment;
    // Serialization is already restricted to 64 lowercase hexadecimal digits.
    // The command line contains only a one-use capability, never a password,
    // caller-supplied path, user identity, or network command.
    let encoded = serde_json::to_value(&authorization).map_err(|_| status(0))?;
    let ticket = encoded.as_str().ok_or_else(|| status(0))?;
    let parameters = wide(&format!("--approve {ticket}"));
    let executable: Vec<u16> = helper.path.as_os_str().encode_wide().chain(Some(0)).collect();
    let directory: Vec<u16> = helper.path.parent().ok_or_else(missing)?.as_os_str().encode_wide().chain(Some(0)).collect();
    let verb = wide("runas");
    let mut launch = SHELLEXECUTEINFOW { cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
        lpVerb: verb.as_ptr(), lpFile: executable.as_ptr(), lpParameters: parameters.as_ptr(), lpDirectory: directory.as_ptr(),
        nShow: SW_HIDE, ..Default::default() };
    if unsafe { ShellExecuteExW(&mut launch) } == 0 { return Err(status(unsafe { GetLastError() })); }
    if launch.hProcess.is_null() || launch.hProcess == INVALID_HANDLE_VALUE { return Err(status(0)); }
    let process = unsafe { OwnedHandle::from_raw_handle(launch.hProcess) };
    if unsafe { WaitForSingleObject(process.as_raw_handle(), 120_000) } != WAIT_OBJECT_0 { return Err(status(0)); }
    let mut code = 1;
    if unsafe { GetExitCodeProcess(process.as_raw_handle(), &mut code) } == 0 || code != 0 { return Err(status(0)); }
    Ok(authorization)
}
/// Fixed helper entry point reached through UAC. The service independently
/// verifies this connection's elevation before marking the ticket approved.
pub fn approve(ticket: &str) -> Result<(), BridgeError> {
    if ticket.len() != 64 || !ticket.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) { return Err(status(0)); }
    let authorization: AuthorizationProof = serde_json::from_str(&format!("\"{ticket}\"")).map_err(|_| status(0))?;
    let reply = HelperClient::connect()?.request(HelperRequest { version: VERSION, request: RequestKind::ApproveAuthorization { authorization } })?;
    if matches!(reply.reply, ReplyKind::Authorized) { Ok(()) } else { Err(status(0)) }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cancellation_is_distinct_and_command_tokens_are_strict() {
        assert_eq!(status(ERROR_CANCELLED).code, "EAUTHCANCELLED");
        assert_eq!(status(ERROR_ACCESS_DENIED).code, "EAUTHDENIED");
        for invalid in ["", "--serve", "ab;whoami", &"AB".repeat(32), &"a".repeat(65)] {
            assert_eq!(approve(invalid).unwrap_err().code, "EAUTHDENIED");
        }
    }
    #[test]
    fn installed_paths_use_native_known_folders_and_missing_helper_cannot_launch() {
        let install = installation_root().unwrap(); let journal = journal_root().unwrap();
        assert!(install.is_absolute() && journal.is_absolute());
        assert_ne!(install, journal);
        assert!(install.ends_with("eink-qemu/bridge"));
        let absent = tempfile::tempdir().unwrap();
        assert!(InstalledHelper::open_at(&absent.path().join("missing")).is_err());
    }
    #[test]
    #[ignore = "requires LocalSystem in a disposable Windows VM"]
    fn helper_file_stays_pinned_and_hard_links_are_rejected() {
        windows_security::require_system().unwrap();
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("eink-bridge-helper.exe");
        std::fs::write(&path, b"test fixture, never executed").unwrap();
        let pinned = InstalledHelper::open_at(root.path()).unwrap();
        assert!(std::fs::write(&path, b"replacement").is_err());
        assert!(std::fs::rename(&path, root.path().join("moved.exe")).is_err());
        drop(pinned);
        std::fs::hard_link(&path, root.path().join("other.exe")).unwrap();
        assert!(InstalledHelper::open_at(root.path()).is_err());
    }
}
