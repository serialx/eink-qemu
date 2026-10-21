//! Validate native ownership and ACLs on the open object, not a path supplied
//! by a client. The service runs as LocalSystem; tests may use their own token.
use crate::model::BridgeError;
use std::{fs::File, os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle}, ptr};
use windows_sys::Win32::{Foundation::{HANDLE, LocalFree}, Security::{*, Authorization::*},
    Storage::FileSystem::{GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION},
    System::{Threading::{GetCurrentProcess, OpenProcessToken}, SystemServices::{SE_GROUP_ENABLED, SE_GROUP_USE_FOR_DENY_ONLY}}};
const SYSTEM: &str = "S-1-5-18";
const ADMINISTRATORS: &str = "S-1-5-32-544";
// Generic write/all, delete, write ACL/owner, file data/EA/attributes and
// directory child deletion. Read and synchronization do not grant mutation.
const WRITE_MASK: u32 = 0x500d0156;
fn denied() -> BridgeError { BridgeError::new("EPERM", "Bridge support has unsafe Windows ownership or permissions. Reinstall its support package.") }
struct LocalAllocation(*mut std::ffi::c_void);
impl Drop for LocalAllocation { fn drop(&mut self) { unsafe { LocalFree(self.0); } } }

pub fn sid_string(sid: PSID) -> Result<String, BridgeError> {
    if sid.is_null() || unsafe { IsValidSid(sid) } == 0 { return Err(denied()); }
    let mut text = ptr::null_mut();
    if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 { return Err(denied()); }
    let _allocation = LocalAllocation(text.cast());
    // A SID is at most 184 characters including terminator.
    let mut len = 0;
    while len < 184 && unsafe { *text.add(len) } != 0 { len += 1; }
    if len == 184 { return Err(denied()); }
    String::from_utf16(unsafe { std::slice::from_raw_parts(text, len) }).map_err(|_| denied())
}
pub fn token_sid(token: HANDLE) -> Result<String, BridgeError> {
    let mut length = 0;
    unsafe { GetTokenInformation(token, TokenUser, ptr::null_mut(), 0, &mut length); }
    if length < std::mem::size_of::<TOKEN_USER>() as u32 || length > 65536 { return Err(denied()); }
    let mut buffer = vec![0u64; (length as usize).div_ceil(8)];
    if unsafe { GetTokenInformation(token, TokenUser, buffer.as_mut_ptr().cast(), length, &mut length) } == 0 { return Err(denied()); }
    sid_string(unsafe { (*(buffer.as_ptr().cast::<TOKEN_USER>())).User.Sid })
}
/// Membership in Administrators alone is insufficient: UAC filtered tokens
/// retain that SID as deny-only. Restricted tokens cannot authorize changes.
pub fn token_is_elevated_admin(token: HANDLE) -> Result<bool, BridgeError> {
    let mut elevation: TOKEN_ELEVATION = unsafe { std::mem::zeroed() };
    let mut length = 0;
    if unsafe { GetTokenInformation(token, TokenElevation, (&mut elevation as *mut TOKEN_ELEVATION).cast(),
        std::mem::size_of_val(&elevation) as u32, &mut length) } == 0 { return Err(denied()); }
    if elevation.TokenIsElevated == 0 || unsafe { IsTokenRestricted(token) } != 0 { return Ok(false); }
    unsafe { GetTokenInformation(token, TokenGroups, ptr::null_mut(), 0, &mut length); }
    let offset = std::mem::offset_of!(TOKEN_GROUPS, Groups);
    if (length as usize) < offset || length > 1024 * 1024 { return Err(denied()); }
    let mut buffer = vec![0u64; (length as usize).div_ceil(8)];
    if unsafe { GetTokenInformation(token, TokenGroups, buffer.as_mut_ptr().cast(), length, &mut length) } == 0 { return Err(denied()); }
    let groups = buffer.as_ptr().cast::<TOKEN_GROUPS>();
    let count = unsafe { (*groups).GroupCount } as usize;
    if offset + count.saturating_mul(std::mem::size_of::<SID_AND_ATTRIBUTES>()) > length as usize { return Err(denied()); }
    let rows = unsafe { std::slice::from_raw_parts(buffer.as_ptr().cast::<u8>().add(offset).cast::<SID_AND_ATTRIBUTES>(), count) };
    for row in rows {
        if row.Attributes & SE_GROUP_ENABLED as u32 != 0 && row.Attributes & SE_GROUP_USE_FOR_DENY_ONLY as u32 == 0
            && sid_string(row.Sid)? == ADMINISTRATORS { return Ok(true); }
    }
    Ok(false)
}
pub fn current_sid() -> Result<String, BridgeError> {
    let mut token = ptr::null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 { return Err(denied()); }
    let token = unsafe { OwnedHandle::from_raw_handle(token) };
    token_sid(token.as_raw_handle())
}
pub fn require_system() -> Result<(), BridgeError> {
    if current_sid()? == SYSTEM { Ok(()) } else { Err(BridgeError::new("EPERM", "Start bridge support through its installed Windows service.")) }
}
fn trusted(sid: &str, owner: Option<&str>) -> bool { sid == SYSTEM || sid == ADMINISTRATORS || owner == Some(sid) }

/// `owner` is obtained from the current process token for an ordinary private
/// journal. Pass None when checking a service object handed to an ordinary
/// client, which must be owned and writable only by SYSTEM/administrators.
pub fn validate_handle(file: &File, owner: Option<&str>) -> Result<(), BridgeError> {
    validate_access(file, owner, WRITE_MASK)
}
pub fn validate_private_handle(file: &File, owner: &str) -> Result<(), BridgeError> {
    // Journal snapshots are private even when their directory is enumerable.
    validate_access(file, Some(owner), WRITE_MASK | 0x80000001)
}
fn validate_access(file: &File, owner: Option<&str>, forbidden: u32) -> Result<(), BridgeError> {
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0
        || info.dwFileAttributes & 0x400 != 0
        || (info.dwFileAttributes & 0x10 == 0 && info.nNumberOfLinks != 1) { return Err(denied()); }
    validate_acl(file.as_raw_handle(), owner, forbidden, false)
}
/// Clients can exchange data but cannot create another server instance or
/// change the endpoint's security. Only a SYSTEM-owned endpoint is accepted.
pub fn validate_pipe(file: &File) -> Result<(), BridgeError> {
    validate_acl(file.as_raw_handle(), None, 0x500d0004, true)
}
fn validate_acl(handle: HANDLE, owner: Option<&str>, forbidden: u32, system_owner: bool) -> Result<(), BridgeError> {
    let mut sid = ptr::null_mut();
    let mut acl = ptr::null_mut();
    let mut descriptor = ptr::null_mut();
    if unsafe { GetSecurityInfo(handle, SE_FILE_OBJECT, OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
        &mut sid, ptr::null_mut(), &mut acl, ptr::null_mut(), &mut descriptor) } != 0 { return Err(denied()); }
    let _allocation = LocalAllocation(descriptor);
    let owner_sid = sid_string(sid)?;
    if !trusted(&owner_sid, owner) || (system_owner && owner_sid != SYSTEM)
        || acl.is_null() || unsafe { IsValidAcl(acl) } == 0 { return Err(denied()); }
    for index in 0..unsafe { (*acl).AceCount } {
        let mut ace = ptr::null_mut();
        if unsafe { GetAce(acl, index as u32, &mut ace) } == 0 { return Err(denied()); }
        let header = unsafe { &*ace.cast::<ACE_HEADER>() };
        if header.AceFlags & INHERIT_ONLY_ACE as u8 != 0 { continue; }
        match header.AceType {
            0 => {
                if (header.AceSize as usize) < std::mem::size_of::<ACCESS_ALLOWED_ACE>() { return Err(denied()); }
                let allowed = unsafe { &*ace.cast::<ACCESS_ALLOWED_ACE>() };
                let sid = (&allowed.SidStart as *const u32).cast_mut().cast();
                if allowed.Mask & forbidden != 0 && !trusted(&sid_string(sid)?, owner) { return Err(denied()); }
            },
            1 => (), // A deny ACE cannot broaden access.
            _ => return Err(denied()), // Conditional/object grants need explicit support.
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::ffi::OsStrExt;
    fn policy(path: &std::path::Path, sddl: &str) {
        let wide: Vec<u16> = sddl.encode_utf16().chain(Some(0)).collect();
        let mut descriptor = ptr::null_mut();
        assert_ne!(unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(wide.as_ptr(), 1, &mut descriptor, ptr::null_mut()) }, 0);
        let _allocation = LocalAllocation(descriptor);
        let mut present = 0;
        let mut defaulted = 0;
        let mut acl = ptr::null_mut();
        assert_ne!(unsafe { GetSecurityDescriptorDacl(descriptor, &mut present, &mut acl, &mut defaulted) }, 0);
        assert_ne!(present, 0);
        let path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        assert_eq!(unsafe { SetNamedSecurityInfoW(path.as_ptr(), SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            ptr::null_mut(), ptr::null_mut(), acl, ptr::null_mut()) }, 0);
    }
    #[test]
    fn writable_acl_and_hard_links_cannot_be_trusted_as_private_journals() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("journal");
        let file = File::create(&path).unwrap();
        drop(file);
        let owner = current_sid().unwrap();
        let base = format!("D:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;FA;;;{owner})");
        policy(&path, &format!("{base}(A;;GR;;;WD)"));
        let file = File::open(&path).unwrap();
        validate_handle(&file, Some(&owner)).unwrap();
        assert_eq!(validate_private_handle(&file, &owner).unwrap_err().code, "EPERM");
        policy(&path, &format!("{base}(A;;GW;;;WD)"));
        assert_eq!(validate_handle(&file, Some(&owner)).unwrap_err().code, "EPERM");
        policy(&path, &base);
        std::fs::hard_link(&path, root.path().join("second-name")).unwrap();
        assert_eq!(validate_handle(&file, Some(&owner)).unwrap_err().code, "EPERM");
    }
}
