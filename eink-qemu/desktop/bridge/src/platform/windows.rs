use crate::model::*;
use std::ffi::CStr;
use windows_sys::Win32::{Foundation::{ERROR_BUFFER_OVERFLOW, NO_ERROR},
    NetworkManagement::IpHelper::{GetAdaptersAddresses, IP_ADAPTER_ADDRESSES_LH, GAA_FLAG_INCLUDE_ALL_INTERFACES},
    Networking::WinSock::AF_UNSPEC};

pub fn adapters() -> Result<Vec<Adapter>, BridgeError> {
    let mut bytes = 16384u32;
    for _ in 0..3 {
        // The native structures require pointer alignment, unlike Vec<u8>.
        let mut buffer = vec![0u64; (bytes as usize).div_ceil(8)];
        let first = buffer.as_mut_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
        let status = unsafe { GetAdaptersAddresses(AF_UNSPEC as u32, GAA_FLAG_INCLUDE_ALL_INTERFACES,
            std::ptr::null(), first, &mut bytes) };
        if status == ERROR_BUFFER_OVERFLOW {
            if bytes > 16 * 1024 * 1024 { break; }
            continue;
        }
        if status != NO_ERROR { return Err(BridgeError::new("EDISCOVERY", format!("Cannot inspect Windows adapters ({status})."))); }
        let mut adapters = Vec::new(); let mut current = first;
        while !current.is_null() {
            let entry = unsafe { &*current };
            if !entry.AdapterName.is_null() && entry.IfType != 24 {
                let id = unsafe { CStr::from_ptr(entry.AdapterName.cast()) }.to_string_lossy().into_owned();
                let name = unsafe { wide_string(entry.FriendlyName) };
                let description = unsafe { wide_string(entry.Description) };
                let kind = if description.to_ascii_lowercase().contains("tap-windows") { AdapterKind::Tap }
                    else if entry.IfType == 6 { AdapterKind::Ethernet }
                    else if entry.IfType == 71 { AdapterKind::Wifi } else { AdapterKind::Other };
                let hardware = entry.PhysicalAddress[..(entry.PhysicalAddressLength as usize).min(8)]
                    .iter().map(|b| format!("{b:02x}")).collect::<String>();
                let eligible = matches!(kind, AdapterKind::Ethernet | AdapterKind::Tap);
                adapters.push(Adapter { id: AdapterId::new(&id)?, name, kind, link_up: entry.OperStatus == 1,
                    eligible, reason: if eligible { None } else { Some("Choose an Ethernet connection or a dedicated TAP adapter.".into()) },
                    fingerprint: format!("{id}:{hardware}") });
            }
            current = entry.Next;
        }
        adapters.sort_by(|a,b| a.id.as_str().cmp(b.id.as_str()));
        return Ok(adapters);
    }
    Err(BridgeError::new("EDISCOVERY", "Windows adapters changed during inspection. Try again."))
}
unsafe fn wide_string(ptr: *const u16) -> String {
    if ptr.is_null() { return String::new(); }
    let mut len = 0;
    while *ptr.add(len) != 0 { len += 1; }
    String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len))
}
