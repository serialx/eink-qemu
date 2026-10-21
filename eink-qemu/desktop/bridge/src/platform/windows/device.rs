//! Read-only PnP identities. Display names and interface indexes are not device
//! ownership: neither is accepted in place of a GUID and device instance ID.
use crate::model::BridgeError;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, ptr};
use windows_sys::Win32::{Devices::DeviceAndDriverInstallation::*, Foundation::*, System::Registry::*};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeviceIdentity {
    pub guid: String,
    pub instance_id: String,
    pub hardware_ids: Vec<String>,
    pub service: String,
}
impl DeviceIdentity {
    pub fn is_tap(&self) -> bool {
        self.service.eq_ignore_ascii_case("tap0901") && self.hardware_ids.iter().any(|id| id.eq_ignore_ascii_case("tap0901"))
    }
    pub fn is_bridge(&self) -> bool {
        self.hardware_ids.iter().any(|id| id.eq_ignore_ascii_case(r"COMPOSITEBUS\MS_IMPLAT_MP"))
    }
    pub fn fingerprint(&self, permanent_address: &[u8]) -> String {
        let mut digest = Sha256::new();
        digest.update(serde_json::to_vec(self).expect("device identity contains only strings"));
        digest.update(permanent_address);
        format!("{:x}", digest.finalize())
    }
}
pub fn guid(value: &str) -> Result<String, BridgeError> {
    let bare = if value.starts_with('{') && value.ends_with('}') { &value[1..value.len()-1] } else { value };
    if bare.len() != 36 || !bare.bytes().enumerate().all(|(index, byte)| {
        if [8, 13, 18, 23].contains(&index) { byte == b'-' } else { byte.is_ascii_hexdigit() }
    }) { return Err(BridgeError::new("EINVAL", "Invalid Windows network adapter identity.")); }
    Ok(format!("{{{}}}", bare.to_ascii_lowercase()))
}
fn wide(value: &str) -> Vec<u16> { value.encode_utf16().chain(Some(0)).collect() }
fn error() -> BridgeError { BridgeError::new("EDISCOVERY", format!("Cannot inspect Windows device identities: {}", std::io::Error::last_os_error())) }
struct Devices(HDEVINFO);
impl Drop for Devices { fn drop(&mut self) { unsafe { SetupDiDestroyDeviceInfoList(self.0); } } }
struct Key(HKEY);
impl Drop for Key { fn drop(&mut self) { unsafe { RegCloseKey(self.0); } } }
fn string(data: &[u16]) -> Option<String> {
    let end = data.iter().position(|value| *value == 0)?;
    if end == 0 { return None; }
    String::from_utf16(&data[..end]).ok()
}
fn registry_string(key: HKEY, name: &str) -> Option<String> {
    let mut data = vec![0u16; 2048];
    let mut size = (data.len() * 2) as u32;
    if unsafe { RegGetValueW(key, ptr::null(), wide(name).as_ptr(), RRF_RT_REG_SZ,
        ptr::null_mut(), data.as_mut_ptr().cast(), &mut size) } != ERROR_SUCCESS || size < 2 || size as usize > data.len() * 2 || size % 2 != 0 { return None; }
    string(&data[..size as usize / 2])
}
fn property(devices: HDEVINFO, entry: &SP_DEVINFO_DATA, property: SETUP_DI_REGISTRY_PROPERTY, expected: u32) -> Option<Vec<String>> {
    let mut data = vec![0u16; 32768];
    let mut size = 0;
    let mut kind = 0;
    if unsafe { SetupDiGetDeviceRegistryPropertyW(devices, entry, property, &mut kind,
        data.as_mut_ptr().cast(), (data.len() * 2) as u32, &mut size) } == 0
        || kind != expected || size < 2 || size as usize > data.len() * 2 || size % 2 != 0 { return None; }
    let data = &data[..size as usize / 2];
    if data.last() != Some(&0) { return None; }
    let mut values = data.split(|value| *value == 0).filter(|part| !part.is_empty())
        .map(String::from_utf16).collect::<Result<Vec<_>, _>>().ok()?;
    if values.is_empty() || (expected == REG_SZ && values.len() != 1) { return None; }
    for value in &mut values { *value = value.to_ascii_uppercase(); }
    values.sort(); values.dedup(); Some(values)
}
pub fn enumerate() -> Result<BTreeMap<String, DeviceIdentity>, BridgeError> {
    let devices = unsafe { SetupDiGetClassDevsW(&GUID_DEVCLASS_NET, ptr::null(), ptr::null_mut(), DIGCF_PRESENT) };
    if devices == -1 { return Err(error()); }
    let devices = Devices(devices);
    let mut result = BTreeMap::new();
    for index in 0..65536 {
        let mut entry = SP_DEVINFO_DATA { cbSize: std::mem::size_of::<SP_DEVINFO_DATA>() as u32, ..Default::default() };
        if unsafe { SetupDiEnumDeviceInfo(devices.0, index, &mut entry) } == 0 {
            if unsafe { GetLastError() } == ERROR_NO_MORE_ITEMS { return Ok(result); }
            return Err(error());
        }
        // Devices still being installed may lack a driver key or identity. Do
        // not infer their identity from a friendly name or offer conversion.
        let key = unsafe { SetupDiOpenDevRegKey(devices.0, &entry, DICS_FLAG_GLOBAL, 0, DIREG_DRV, KEY_QUERY_VALUE) };
        if key == INVALID_HANDLE_VALUE { continue; }
        let key = Key(key);
        let Some(net_guid) = registry_string(key.0, "NetCfgInstanceId").and_then(|value| guid(&value).ok()) else { continue; };
        let mut instance = [0u16; 4096];
        if unsafe { SetupDiGetDeviceInstanceIdW(devices.0, &entry, instance.as_mut_ptr(), instance.len() as u32, ptr::null_mut()) } == 0 { continue; }
        let Some(instance_id) = string(&instance) else { continue; };
        let Some(hardware_ids) = property(devices.0, &entry, SPDRP_HARDWAREID, REG_MULTI_SZ) else { continue; };
        let Some(service) = property(devices.0, &entry, SPDRP_SERVICE, REG_SZ).and_then(|values| values.into_iter().next()) else { continue; };
        let identity = DeviceIdentity { guid: net_guid.clone(), instance_id: instance_id.to_ascii_uppercase(), hardware_ids, service };
        if result.insert(net_guid, identity).is_some() {
            return Err(BridgeError::new("EDISCOVERY", "Windows returned duplicate network device identities. Refresh bridge setup."));
        }
    }
    Err(BridgeError::new("EDISCOVERY", "Too many Windows network devices to inspect safely."))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identities_are_canonical_and_replacement_changes_the_fingerprint() {
        let guid = guid("ABCDEF12-1234-5678-9ABC-1234567890AB").unwrap();
        assert_eq!(guid, "{abcdef12-1234-5678-9abc-1234567890ab}");
        for invalid in ["Ethernet", "1", "{123}", "abcdef12-1234-5678-9abc-1234567890ab & cmd", "abcdef12-1234-5678-9abc-1234567890az"] {
            assert!(super::guid(invalid).is_err());
        }
        let original = DeviceIdentity { guid, instance_id: "USB\\DEVICE\\ONE".into(), hardware_ids: vec!["USB\\1234".into()], service: "DRIVER".into() };
        let replacement = DeviceIdentity { instance_id: "USB\\DEVICE\\TWO".into(), ..original.clone() };
        assert_ne!(original.fingerprint(&[1, 2, 3]), replacement.fingerprint(&[1, 2, 3]));
        assert_ne!(original.fingerprint(&[1, 2, 3]), original.fingerprint(&[4, 5, 6]));
        assert!(!original.is_tap());
        assert!(!original.is_bridge());
    }
    #[test]
    fn tap_identification_requires_both_native_service_and_hardware_id() {
        let mut device = DeviceIdentity { guid: guid("11111111-2222-3333-4444-555555555555").unwrap(),
            instance_id: "ROOT\\NET\\0001".into(), hardware_ids: vec!["TAP0901".into()], service: "OTHER".into() };
        assert!(!device.is_tap());
        device.service = "tap0901".into(); assert!(device.is_tap());
        device.hardware_ids = vec![r"COMPOSITEBUS\MS_IMPLAT_MP".into()];
        assert!(!device.is_tap()); assert!(device.is_bridge());
    }
}
