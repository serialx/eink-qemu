//! Ordinary-process TAP access check. It never brings media up or changes
//! device permissions; QEMU must be able to open the same GUID itself.
use crate::{model::{BridgeError, LaunchTarget}, windows_device};
use std::{os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle}, ptr};
use windows_sys::Win32::{Foundation::*, Storage::FileSystem::*, System::{IO::*, Threading::*}};
fn wide(value: &str) -> Vec<u16> { value.encode_utf16().chain(Some(0)).collect() }
fn error(code: u32) -> BridgeError {
    let (kind, message) = match code {
        ERROR_SHARING_VIOLATION => ("EBUSY", "This TAP adapter is already in use. Stop its emulator before verifying or starting it again."),
        ERROR_ACCESS_DENIED => ("EACCESS", "Windows did not allow this user to open the bridge adapter. Repair bridge support and try again."),
        ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND => ("EDISCONNECTED", "The bridge adapter is no longer available. Reopen bridge setup."),
        _ => ("ETRANSPORT", "Windows could not open the bridge packet transport."),
    };
    BridgeError { code: kind.into(), message: message.into(), detail: Some(format!("Windows status {code}")) }
}
fn open_error(code:u32)->BridgeError{
    // TAP's IRP_MJ_CREATE returns STATUS_UNSUCCESSFUL when TapFileObject is
    // already occupied; Windows maps it to ERROR_GEN_FAILURE, not a sharing
    // violation. Apply this only to opening a verified TAP device, never to
    // failed IOCTLs. https://github.com/OpenVPN/tap-windows6/blob/master/src/device.c
    if code==ERROR_GEN_FAILURE {
        BridgeError {code:"EBUSY".into(),message:"The TAP adapter is unavailable or already in use. Stop its emulator and try again.".into(),
            detail:Some(format!("Windows status {code}"))}
    }else{error(code)}
}
pub fn target(guid: &str) -> Result<LaunchTarget, BridgeError> {
    Ok(LaunchTarget::WindowsTap { name: format!("guid:{}", windows_device::guid(guid)?) })
}
pub fn probe_target(target: &LaunchTarget) -> Result<(), BridgeError> {
    let LaunchTarget::WindowsTap { name } = target else { return Err(BridgeError::new("EPROTOCOL", "Invalid Windows bridge target.")); };
    let guid = name.strip_prefix("guid:").ok_or_else(|| BridgeError::new("EPROTOCOL", "Windows bridge support must identify its adapter by GUID."))?;
    probe(guid).map(|_| ())
}
pub fn probe(guid: &str) -> Result<[u32; 3], BridgeError> {
    let guid = windows_device::guid(guid)?;
    let devices = windows_device::enumerate()?;
    if !devices.get(&guid).is_some_and(|device| device.is_tap()) {
        return Err(BridgeError::new("EDRIVER", "The selected adapter is not the expected TAP driver. Repair bridge setup."));
    }
    let handle = unsafe { CreateFileW(wide(&format!(r"\\.\Global\{guid}.tap")).as_ptr(), GENERIC_READ | GENERIC_WRITE,
        0, ptr::null(), OPEN_EXISTING, FILE_ATTRIBUTE_SYSTEM | FILE_FLAG_OVERLAPPED, ptr::null_mut()) };
    if handle == INVALID_HANDLE_VALUE { return Err(open_error(unsafe { GetLastError() })); }
    let file = unsafe { OwnedHandle::from_raw_handle(handle) };
    let event = unsafe { CreateEventW(ptr::null(), 1, 0, ptr::null()) };
    if event.is_null() { return Err(error(unsafe { GetLastError() })); }
    let event = unsafe { OwnedHandle::from_raw_handle(event) };
    let mut operation: OVERLAPPED = unsafe { std::mem::zeroed() };
    operation.hEvent = event.as_raw_handle();
    let mut version = [0u32; 3];
    let mut bytes = 0;
    // TAP_CONTROL_CODE(2, METHOD_BUFFERED), the same query QEMU uses.
    let result = unsafe { DeviceIoControl(file.as_raw_handle(), 0x00220008, ptr::null(), 0,
        version.as_mut_ptr().cast(), std::mem::size_of_val(&version) as u32, &mut bytes, &mut operation) };
    if result == 0 {
        let code = unsafe { GetLastError() };
        if code != ERROR_IO_PENDING { return Err(error(code)); }
        if unsafe { WaitForSingleObject(event.as_raw_handle(), 3000) } != WAIT_OBJECT_0 {
            // Cancel requests still reference the output buffer and OVERLAPPED
            // until the kernel reports completion. Drain before returning.
            unsafe { CancelIoEx(file.as_raw_handle(), &operation); GetOverlappedResult(file.as_raw_handle(), &operation, &mut bytes, 1); }
            return Err(BridgeError::new("ETIMEDOUT", "The Windows bridge driver did not answer its access check."));
        }
        if unsafe { GetOverlappedResult(file.as_raw_handle(), &operation, &mut bytes, 0) } == 0 { return Err(error(unsafe { GetLastError() })); }
    }
    if bytes != 12 || version[0] != 9 || version[1] != 27 || version[2] != 0 {
        return Err(BridgeError::new("EDRIVER", "The bridge adapter needs the supported release of TAP-Windows 9.27. Repair bridge support."));
    }
    Ok(version)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tap_exclusive_open_failure_does_not_reclassify_ioctl_failures(){
        assert_eq!(open_error(ERROR_GEN_FAILURE).code,"EBUSY");
        assert_eq!(error(ERROR_GEN_FAILURE).code,"ETRANSPORT");
        assert_eq!(open_error(ERROR_ACCESS_DENIED).code,"EACCESS");
    }
    #[test]
    fn managed_targets_cannot_select_friendly_names_or_device_paths() {
        let expected = "{12345678-1234-5678-9abc-1234567890ab}";
        assert_eq!(target(expected).unwrap(), LaunchTarget::WindowsTap { name: format!("guid:{expected}") });
        for invalid in ["Ethernet", r"\\.\Global\something.tap", "{123}", ""] {
            assert!(target(invalid).is_err());
        }
        assert_eq!(probe_target(&LaunchTarget::WindowsTap { name: "Ethernet".into() }).unwrap_err().code, "EPROTOCOL");
    }
}
