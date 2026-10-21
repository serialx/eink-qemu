//! A protected volatile registry key identifies the current Windows kernel
//! session without depending on wall-clock changes or undocumented NT classes.
//! It survives service restart and hibernation, but disappears on full restart.
//! https://learn.microsoft.com/windows/win32/api/winreg/nf-winreg-regcreatekeyexw
use crate::{model::BridgeError, windows_security};
use serde::{Deserialize, Serialize};
use std::ptr;
use windows_sys::Win32::{Foundation::*, Security::{*, Authorization::*}, System::Registry::*};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BootIdentity(String);
fn failed(code: u32) -> BridgeError {
    BridgeError::new("EBOOT", format!("Windows could not verify the bridge setup boot identity: {}", std::io::Error::from_raw_os_error(code as i32)))
}
fn wide(value: &str) -> Vec<u16> { value.encode_utf16().chain(Some(0)).collect() }
struct Key(HKEY);
impl Drop for Key { fn drop(&mut self) { unsafe { RegCloseKey(self.0); } } }
struct Descriptor(PSECURITY_DESCRIPTOR);
impl Drop for Descriptor { fn drop(&mut self) { unsafe { LocalFree(self.0); } } }
fn child(parent: HKEY, name: &str, volatile: bool) -> Result<Key, BridgeError> {
    let mut key = ptr::null_mut();
    let name = wide(name);
    let rights = KEY_READ | KEY_WRITE | KEY_WOW64_64KEY;
    // Open the link itself, if present, so a registry link cannot redirect a
    // privileged write before its ownership and type have been checked.
    let mut status = unsafe { RegOpenKeyExW(parent, name.as_ptr(), REG_OPTION_OPEN_LINK, rights, &mut key) };
    if status == ERROR_FILE_NOT_FOUND {
        let mut descriptor = ptr::null_mut();
        if unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(wide("O:SYD:P(A;CI;KA;;;SY)(A;CI;KA;;;BA)").as_ptr(),
            1, &mut descriptor, ptr::null_mut()) } == 0 { return Err(failed(unsafe { GetLastError() })); }
        let descriptor = Descriptor(descriptor);
        let attributes = SECURITY_ATTRIBUTES { nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor.0, bInheritHandle: 0 };
        let mut random = [0u8; 32];
        getrandom::fill(&mut random).map_err(|_| failed(ERROR_GEN_FAILURE))?;
        let mut class = wide(&random.iter().map(|byte| format!("{byte:02x}")).collect::<String>());
        // The class is created atomically with the key. A helper crash cannot
        // leave a newly created boot key with a missing identity value.
        status = unsafe { RegCreateKeyExW(parent, name.as_ptr(), 0, class.as_mut_ptr(),
            if volatile { REG_OPTION_VOLATILE } else { REG_OPTION_NON_VOLATILE }, rights,
            &attributes, &mut key, ptr::null_mut()) };
    }
    if status != ERROR_SUCCESS { return Err(failed(status)); }
    let key = Key(key);
    windows_security::validate_registry_key(key.0)?;
    let status = unsafe { RegQueryValueExW(key.0, wide("SymbolicLinkValue").as_ptr(), ptr::null(),
        ptr::null_mut(), ptr::null_mut(), ptr::null_mut()) };
    if status != ERROR_FILE_NOT_FOUND { return Err(failed(ERROR_INVALID_DATA)); }
    Ok(key)
}
fn read(namespace: &str) -> Result<BootIdentity, BridgeError> {
    windows_security::require_system()?;
    let mut software = ptr::null_mut();
    let status = unsafe { RegOpenKeyExW(HKEY_LOCAL_MACHINE, wide("SOFTWARE").as_ptr(), 0,
        KEY_READ | KEY_CREATE_SUB_KEY | KEY_WOW64_64KEY, &mut software) };
    if status != ERROR_SUCCESS { return Err(failed(status)); }
    let software = Key(software);
    let vendor = child(software.0, namespace, false)?;
    let bridge = child(vendor.0, "bridge", false)?;
    let boot = child(bridge.0, "boot", true)?;
    let mut class = [0u16; 128]; let mut length = class.len() as u32;
    let status = unsafe { RegQueryInfoKeyW(boot.0, class.as_mut_ptr(), &mut length, ptr::null(),
        ptr::null_mut(), ptr::null_mut(), ptr::null_mut(), ptr::null_mut(), ptr::null_mut(),
        ptr::null_mut(), ptr::null_mut(), ptr::null_mut()) };
    if status != ERROR_SUCCESS || length != 64 { return Err(failed(ERROR_INVALID_DATA)); }
    let identity = String::from_utf16(&class[..length as usize]).map_err(|_| failed(ERROR_INVALID_DATA))?;
    if !identity.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)) { return Err(failed(ERROR_INVALID_DATA)); }
    Ok(BootIdentity(identity))
}
/// Call only after the service has acquired its protected installation lock.
/// Persist this identity with a driver step that requests reboot; require a
/// different identity and revalidate the installed driver before continuing.
pub fn current() -> Result<BootIdentity, BridgeError> { read("eink-qemu") }

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "requires LocalSystem in the disposable Windows VM"]
    fn boot_marker_is_stable_across_reopen_and_protected_from_untrusted_writes() {
        windows_security::require_system().unwrap();
        let namespace = format!("eink-bridge-boot-test-{:016x}", getrandom::u64().unwrap());
        struct Cleanup(String);
        impl Drop for Cleanup { fn drop(&mut self) { unsafe { RegDeleteTreeW(HKEY_LOCAL_MACHINE, wide(&format!(r"SOFTWARE\{}",self.0)).as_ptr()); } } }
        let _cleanup = Cleanup(namespace.clone());
        let first = read(&namespace).unwrap();
        assert_eq!(first, read(&namespace).unwrap());
        let path = wide(&format!(r"SOFTWARE\{namespace}\bridge\boot"));
        let mut key = ptr::null_mut();
        assert_eq!(unsafe { RegOpenKeyExW(HKEY_LOCAL_MACHINE, path.as_ptr(), 0, KEY_ALL_ACCESS | KEY_WOW64_64KEY, &mut key) }, ERROR_SUCCESS);
        let key = Key(key);
        let mut descriptor = ptr::null_mut();
        assert_ne!(unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(wide("D:P(A;;KA;;;SY)(A;;KA;;;BA)(A;;KW;;;WD)").as_ptr(), 1,
            &mut descriptor, ptr::null_mut()) }, 0);
        let descriptor = Descriptor(descriptor);
        assert_eq!(unsafe { RegSetKeySecurity(key.0, DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION, descriptor.0) }, ERROR_SUCCESS);
        assert!(read(&namespace).is_err());
    }
}
