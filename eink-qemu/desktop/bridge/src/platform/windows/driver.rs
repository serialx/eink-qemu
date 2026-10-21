//! Dedicated TAP device provisioning. Call only from an authorized service
//! transaction after persisting its resource ID. Never update all tap0901
//! devices, uninstall the shared driver, or adopt an existing device by name.
use crate::{model::{BridgeError, ResourceId}, windows_device::{self, DeviceIdentity}, windows_security};
use sha2::{Digest, Sha256};
use std::{fs::{File, OpenOptions}, io::Read, os::windows::{ffi::OsStrExt, fs::OpenOptionsExt, io::AsRawHandle},
    path::{Path, PathBuf}, ptr, time::{Duration, Instant}};
use windows_sys::Win32::{Devices::DeviceAndDriverInstallation::*, Foundation::*, Security::WinTrust::*,
    Storage::FileSystem::*, System::Registry::REG_MULTI_SZ};

include!(concat!(env!("OUT_DIR"), "/tap-pins.rs"));
fn error(stage: &str) -> BridgeError { BridgeError::new("EDRIVER", format!("{stage}: {}", std::io::Error::last_os_error())) }
fn invalid(message: &str) -> BridgeError { BridgeError::new("EDRIVER", message) }
fn wide(value: &str) -> Vec<u16> { value.encode_utf16().chain(Some(0)).collect() }
pub fn instance_id(resource: &ResourceId) -> String { format!(r"ROOT\EINKQEMUTAP\{}", resource.as_str().to_ascii_uppercase()) }

fn native_x64() -> Result<bool, BridgeError> {
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, IsWow64Process2};
    let mut process = 0; let mut native = 0;
    if unsafe { IsWow64Process2(GetCurrentProcess(), &mut process, &mut native) } == 0 {
        return Err(error("Cannot inspect native Windows architecture"));
    }
    Ok(cfg!(target_arch = "x86_64") && native == 0x8664)
}

/// Files and directories remain open without delete sharing until installation
/// completes. The installer supplies this fixed protected package directory;
/// it must never come from a WebView or helper request.
pub struct VerifiedPackage { inf: PathBuf, _locks: Vec<File> }
impl VerifiedPackage {
    pub fn open(root: &Path) -> Result<Self, BridgeError> {
        if !native_x64()? { return Err(BridgeError::new("EUNSUPPORTED", "Automatic Windows bridge setup requires x64 Windows.")); }
        if !root.is_absolute() || root.as_os_str().encode_wide().any(|c| c == 0)
            || root.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
            return Err(invalid("Invalid installed TAP package location."));
        }
        let mut locks = Vec::new();
        for ancestor in root.ancestors().collect::<Vec<_>>().into_iter().rev() {
            let file = OpenOptions::new().read(true).share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT).open(ancestor)
                .map_err(|_| invalid("Cannot lock the installed TAP package directory."))?;
            let mut info = BY_HANDLE_FILE_INFORMATION::default();
            if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0
                || info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
                || info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY == 0 { return Err(invalid("TAP package directories cannot be reparse points.")); }
            if ancestor == root { windows_security::validate_handle(&file, None)?; }
            locks.push(file);
        }
        for &(name, length, expected) in FILES {
            let mut file = OpenOptions::new().read(true).share_mode(FILE_SHARE_READ)
                .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT).open(root.join(name))
                .map_err(|_| invalid("The installed TAP driver package is incomplete."))?;
            windows_security::validate_handle(&file, None)?;
            let metadata = file.metadata().map_err(|_| invalid("Cannot inspect the TAP driver package."))?;
            if !metadata.is_file() || metadata.len() != length { return Err(invalid("The TAP driver package has an unexpected size or architecture.")); }
            let mut bytes = Vec::new();
            (&mut file).take(length + 1).read_to_end(&mut bytes).map_err(|_| invalid("Cannot read the TAP driver package."))?;
            if format!("{:x}", Sha256::digest(&bytes)) != expected { return Err(invalid("The TAP driver package checksum does not match this release.")); }
            if name.ends_with(".cat") { verify_signature(&root.join(name), &file)?; }
            locks.push(file);
        }
        Ok(Self { inf: root.join("OemVista.inf"), _locks: locks })
    }
}
fn verify_signature(path: &Path, file: &File) -> Result<(), BridgeError> {
    let path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut info = WINTRUST_FILE_INFO { cbStruct: std::mem::size_of::<WINTRUST_FILE_INFO>() as u32,
        pcwszFilePath: path.as_ptr(), hFile: file.as_raw_handle(), ..Default::default() };
    let mut trust = WINTRUST_DATA { cbStruct: std::mem::size_of::<WINTRUST_DATA>() as u32,
        dwUIChoice: WTD_UI_NONE, fdwRevocationChecks: WTD_REVOKE_NONE, dwUnionChoice: WTD_CHOICE_FILE,
        Anonymous: WINTRUST_DATA_0 { pFile: &mut info }, dwStateAction: WTD_STATEACTION_VERIFY,
        // Setup runs without a network dependency; Windows still verifies the
        // pinned catalog signature and cached certificate/revocation state.
        dwProvFlags: WTD_CACHE_ONLY_URL_RETRIEVAL, ..Default::default() };
    let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
    let result = unsafe { WinVerifyTrust(INVALID_HANDLE_VALUE, &mut action, (&mut trust as *mut WINTRUST_DATA).cast()) };
    trust.dwStateAction = WTD_STATEACTION_CLOSE;
    unsafe { WinVerifyTrust(INVALID_HANDLE_VALUE, &mut action, (&mut trust as *mut WINTRUST_DATA).cast()); }
    if result != 0 { return Err(invalid(&format!("Windows cannot verify the pinned TAP driver catalog (0x{:08x}).", result as u32))); }
    Ok(())
}
struct Devices(HDEVINFO);
impl Devices {
    fn new() -> Result<Self, BridgeError> {
        let value = unsafe { SetupDiCreateDeviceInfoList(&GUID_DEVCLASS_NET, ptr::null_mut()) };
        if value == -1 { Err(error("Cannot open network device installation")) } else { Ok(Self(value)) }
    }
}
impl Drop for Devices { fn drop(&mut self) { unsafe { SetupDiDestroyDeviceInfoList(self.0); } } }
fn data() -> SP_DEVINFO_DATA { SP_DEVINFO_DATA { cbSize: std::mem::size_of::<SP_DEVINFO_DATA>() as u32, ..Default::default() } }
fn parameters(devices: &Devices, entry: &SP_DEVINFO_DATA) -> Result<SP_DEVINSTALL_PARAMS_W, BridgeError> {
    let mut params = SP_DEVINSTALL_PARAMS_W { cbSize: std::mem::size_of::<SP_DEVINSTALL_PARAMS_W>() as u32, ..Default::default() };
    if unsafe { SetupDiGetDeviceInstallParamsW(devices.0, entry, &mut params) } == 0 { return Err(error("Cannot inspect device installation")); }
    Ok(params)
}
fn reboot(devices: &Devices, entry: &SP_DEVINFO_DATA) -> Result<bool, BridgeError> {
    Ok(parameters(devices, entry)?.Flags & (DI_NEEDREBOOT | DI_NEEDRESTART) != 0)
}
#[derive(Debug)]
pub struct InstalledDevice { pub identity: Option<DeviceIdentity>, pub reboot_required: bool }

/// The resource ID must already exist in a durable undo record. A crash after
/// registration is recovered by this exact instance ID, even before it has a
/// network GUID. An existing instance is a conflict, never an installation
/// retry: recovery must inspect/undo the interrupted transaction first.
pub fn create(package: &VerifiedPackage, resource: &ResourceId) -> Result<InstalledDevice, BridgeError> {
    windows_security::require_system()?;
    // Stage the exact signed package without updating any existing TAP device.
    let source: Vec<u16> = package.inf.as_os_str().encode_wide().chain(Some(0)).collect();
    if unsafe { SetupCopyOEMInfW(source.as_ptr(), ptr::null(), SPOST_PATH, 0,
        ptr::null_mut(), 0, ptr::null_mut(), ptr::null_mut()) } == 0 {
        return Err(error("Cannot stage the signed TAP driver package"));
    }
    let devices = Devices::new()?;
    let mut entry = data();
    let instance = wide(&instance_id(resource));
    if unsafe { SetupDiOpenDeviceInfoW(devices.0, instance.as_ptr(), ptr::null_mut(), 0, &mut entry) } != 0 {
        return Err(BridgeError::new("ECONFLICT", "The planned TAP device already exists. Recover the previous setup first."));
    }
    if unsafe { GetLastError() } != 0xe000020b { return Err(error("Cannot verify that the planned TAP device is absent")); }
    // Without DICD_GENERATE_ID, Windows uses this exact durable instance ID.
    if unsafe { SetupDiCreateDeviceInfoW(devices.0, instance.as_ptr(), &GUID_DEVCLASS_NET,
        wide("eink-qemu managed TAP").as_ptr(), ptr::null_mut(), 0, &mut entry) } == 0 { return Err(error("Cannot create the dedicated TAP device")); }
    let hardware = wide("tap0901\0");
    if unsafe { SetupDiSetDeviceRegistryPropertyW(devices.0, &mut entry, SPDRP_HARDWAREID,
        hardware.as_ptr().cast(), (hardware.len() * 2) as u32) } == 0 { return Err(error("Cannot identify the dedicated TAP device")); }
    let mut params = parameters(&devices, &entry)?;
    let inf: Vec<u16> = package.inf.as_os_str().encode_wide().chain(Some(0)).collect();
    if inf.len() > params.DriverPath.len() { return Err(invalid("The installed TAP package path is too long.")); }
    params.DriverPath[..inf.len()].copy_from_slice(&inf);
    params.Flags |= DI_ENUMSINGLEINF | DI_QUIETINSTALL;
    if unsafe { SetupDiSetDeviceInstallParamsW(devices.0, &entry, &params) } == 0
        || unsafe { SetupDiBuildDriverInfoList(devices.0, &mut entry, SPDIT_COMPATDRIVER) } == 0 {
        return Err(error("Cannot select the pinned TAP driver"));
    }
    // The list is restricted to the verified INF and this device's hardware ID.
    if unsafe { SetupDiCallClassInstaller(DIF_SELECTBESTCOMPATDRV, devices.0, &entry) } == 0
        || unsafe { SetupDiCallClassInstaller(DIF_REGISTERDEVICE, devices.0, &entry) } == 0 {
        return Err(error("Cannot register the dedicated TAP device"));
    }
    #[cfg(test)]
    if std::env::var("EINK_DRIVER_TEST_CRASH_RESOURCE").as_deref() == Ok(resource.as_str()) {
        std::process::exit(86);
    }
    let mut driver = SP_DRVINFO_DATA_V2_W { cbSize: std::mem::size_of::<SP_DRVINFO_DATA_V2_W>() as u32, ..Default::default() };
    if unsafe { SetupDiGetSelectedDriverW(devices.0, &entry, &mut driver) } == 0 { return Err(error("Cannot inspect the selected TAP driver")); }
    let mut need_reboot = 0;
    // DiInstallDevice performs the complete per-device installation sequence,
    // including the network class installer's parameter initialization. Calling
    // only DIF_INSTALLDEVICE leaves TAP's vendor defaults uninitialized.
    if unsafe { DiInstallDevice(ptr::null_mut(), devices.0, &entry, &driver, 0, &mut need_reboot) } == 0 {
        return Err(error("Cannot install the dedicated TAP device"));
    }
    let reboot_required = need_reboot != 0 || reboot(&devices, &entry)?;
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let identity = windows_device::enumerate()?.into_values().find(|device| device.instance_id == instance_id(resource));
        if let Some(identity) = identity {
            if !identity.is_tap() { return Err(BridgeError::new("ECONFLICT", "The installed device no longer matches the planned TAP adapter.")); }
            if reboot_required { return Ok(InstalledDevice { identity: Some(identity), reboot_required }); }
            match crate::windows_verify::probe(&identity.guid) {
                Ok(_) => return Ok(InstalledDevice { identity: Some(identity), reboot_required: false }),
                Err(error) if error.code == "EDISCONNECTED" => (), // PnP identity precedes the device endpoint.
                Err(error) => return Err(error),
            }
        }
        if reboot_required { return Ok(InstalledDevice { identity: None, reboot_required }); }
        if Instant::now() >= deadline {
            let mut status = 0; let mut problem = 0;
            unsafe { CM_Get_DevNode_Status(&mut status, &mut problem, entry.DevInst, 0); }
            return Err(invalid(&format!("Windows has not started the new TAP connection (status {status:#x}, problem {problem}). Recover setup before retrying.")));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Remove only the instance authorized by a durable transaction. If a GUID was
/// recorded, require it too. Shared TAP driver files and other devices stay.
/// Call before undoing failed creation, or after committed removal has its own
/// durable cleanup record; deleting a device cannot preserve its old GUID.
pub fn remove(resource: &ResourceId, expected: Option<&DeviceIdentity>) -> Result<bool, BridgeError> {
    windows_security::require_system()?;
    if !native_x64()? { return Err(BridgeError::new("EUNSUPPORTED", "Automatic Windows bridge setup requires x64 Windows.")); }
    let instance = instance_id(resource);
    if expected.is_some_and(|device| device.instance_id != instance || !device.is_tap()) {
        return Err(BridgeError::new("ECONFLICT", "The recorded TAP device identity is inconsistent."));
    }
    let devices = Devices::new()?;
    let mut entry = data();
    if unsafe { SetupDiOpenDeviceInfoW(devices.0, wide(&instance).as_ptr(), ptr::null_mut(), 0, &mut entry) } == 0 {
        // SetupAPI's ERROR_NO_SUCH_DEVINST, not arbitrary discovery failures.
        if unsafe { GetLastError() } == 0xe000020b { return Ok(false); }
        return Err(error("Cannot inspect the TAP device for removal"));
    }
    let identities = windows_device::enumerate()?;
    let current = identities.values().find(|device| device.instance_id == instance);
    if let Some(expected) = expected {
        if current != Some(expected) { return Err(BridgeError::new("ECONFLICT", "The TAP device changed after setup. Its current configuration was preserved.")); }
    } else if current.is_some_and(|device| !device.is_tap()) {
        return Err(BridgeError::new("ECONFLICT", "The planned device is no longer a TAP adapter."));
    }
    // A partially installed device can lack a service/GUID, but registration
    // always follows writing its exact hardware ID. Refuse foreign replacements.
    let mut hardware = [0u16; 64]; let mut size = 0; let mut kind = 0;
    if unsafe { SetupDiGetDeviceRegistryPropertyW(devices.0, &entry, SPDRP_HARDWAREID, &mut kind,
        hardware.as_mut_ptr().cast(), (hardware.len() * 2) as u32, &mut size) } == 0
        || kind != REG_MULTI_SZ || size as usize != wide("tap0901\0").len() * 2
        || hardware[..size as usize / 2] != wide("tap0901\0") {
        return Err(BridgeError::new("ECONFLICT", "The TAP device hardware identity changed. The device was preserved."));
    }
    let params = SP_REMOVEDEVICE_PARAMS { ClassInstallHeader: SP_CLASSINSTALL_HEADER {
        cbSize: std::mem::size_of::<SP_CLASSINSTALL_HEADER>() as u32, InstallFunction: DIF_REMOVE }, Scope: DI_REMOVEDEVICE_GLOBAL, HwProfile: 0 };
    if unsafe { SetupDiSetClassInstallParamsW(devices.0, &entry, &params.ClassInstallHeader, std::mem::size_of_val(&params) as u32) } == 0
        || unsafe { SetupDiCallClassInstaller(DIF_REMOVE, devices.0, &entry) } == 0 { return Err(error("Cannot remove the dedicated TAP device")); }
    reboot(&devices, &entry)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "requires LocalSystem and the explicit disposable Windows test opt-in"]
    fn interrupted_registration_is_recovered_by_durable_instance_id() {
        use std::io::Write;
        assert_eq!(std::env::var("EINK_BRIDGE_DISPOSABLE_WINDOWS_TESTS").as_deref(), Ok("1"));
        windows_security::require_system().unwrap();
        let root = PathBuf::from(std::env::var_os("EINK_DRIVER_TEST_FIXTURE").expect("protected native fixture directory"));
        let package = VerifiedPackage::open(&root.join("driver")).unwrap();
        if let Ok(id) = std::env::var("EINK_DRIVER_TEST_CRASH_RESOURCE") {
            let resource = ResourceId::new(id).unwrap();
            create(&package, &resource).unwrap();
            panic!("expected process exit immediately after device registration");
        }
        let before = windows_device::enumerate().unwrap();
        let mut random = [0u8; 16]; getrandom::fill(&mut random).unwrap();
        let resource = ResourceId::new(random.iter().map(|b| format!("{b:02x}")).collect::<String>()).unwrap();
        let record = root.join("interrupted-device.json");
        let mut file = OpenOptions::new().create_new(true).write(true).open(&record).unwrap();
        windows_security::validate_handle(&file, None).unwrap();
        serde_json::to_writer(&mut file, &resource).unwrap(); file.flush().unwrap(); file.sync_all().unwrap(); drop(file);
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "windows_driver::tests::interrupted_registration_is_recovered_by_durable_instance_id", "--include-ignored"])
            .env("EINK_DRIVER_TEST_CRASH_RESOURCE", resource.as_str()).spawn().unwrap();
        let status = child.wait().unwrap();
        // Re-read what survived the other process, not a returned native GUID.
        let saved: ResourceId = serde_json::from_reader(File::open(&record).unwrap()).unwrap();
        assert!(!remove(&saved, None).unwrap());
        assert!(!remove(&saved, None).unwrap());
        assert_eq!(before, windows_device::enumerate().unwrap());
        std::fs::remove_file(record).unwrap();
        assert_eq!(status.code(), Some(86));
    }
    #[test]
    fn unsigned_catalog_is_rejected_by_windows_trust() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("unsigned.cat");
        std::fs::write(&path, b"not a signed driver catalog").unwrap();
        let file = File::open(&path).unwrap();
        assert_eq!(verify_signature(&path, &file).unwrap_err().code, "EDRIVER");
    }
    #[test]
    fn device_instance_is_deterministic_and_cannot_target_an_existing_vpn() {
        let resource = ResourceId::new("0123456789abcdef0123456789abcdef").unwrap();
        assert_eq!(instance_id(&resource), r"ROOT\EINKQEMUTAP\0123456789ABCDEF0123456789ABCDEF");
        assert!(ResourceId::new(r"..\NET\0000").is_err());
    }
}
