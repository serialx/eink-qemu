use crate::{model::*, windows_device};
use std::ffi::CStr;
use windows_sys::Win32::{Foundation::{ERROR_BUFFER_OVERFLOW, NO_ERROR},
    NetworkManagement::IpHelper::{GetAdaptersAddresses, GetIfEntry2, MIB_IF_ROW2, IP_ADAPTER_ADDRESSES_LH, GAA_FLAG_INCLUDE_ALL_INTERFACES},
    Networking::WinSock::AF_UNSPEC};

pub fn adapters() -> Result<Vec<Adapter>, BridgeError> {
    let devices = windows_device::enumerate()?;
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
                let id = windows_device::guid(&unsafe { CStr::from_ptr(entry.AdapterName.cast()) }.to_string_lossy())?;
                let name = unsafe { wide_string(entry.FriendlyName) };
                let device = devices.get(&id);
                let mut row = MIB_IF_ROW2 { InterfaceLuid: entry.Luid, ..Default::default() };
                let known_interface = unsafe { GetIfEntry2(&mut row) } == NO_ERROR;
                if known_interface && row.InterfaceAndOperStatusFlags._bitfield & 2 != 0 {
                    // NDIS filter layers are not separate user connections.
                    current = entry.Next;
                    continue;
                }
                let physical = known_interface && row.InterfaceAndOperStatusFlags._bitfield & 1 != 0;
                let kind = if device.is_some_and(|device| device.is_tap()) { AdapterKind::Tap }
                    else if device.is_some_and(|device| device.is_bridge()) { AdapterKind::Bridge }
                    else if entry.IfType == 6 && physical { AdapterKind::Ethernet }
                    else if entry.IfType == 71 { AdapterKind::Wifi } else { AdapterKind::Other };
                let permanent_address = &row.PermanentPhysicalAddress[..(row.PhysicalAddressLength as usize).min(row.PermanentPhysicalAddress.len())];
                let fingerprint = if let Some(device) = device.filter(|_| known_interface) { device.fingerprint(permanent_address) }
                    else { format!("unverified:{id}") };
                let eligible = kind == AdapterKind::Ethernet && device.is_some() && !permanent_address.is_empty();
                let reason = if eligible { None } else { Some(match kind {
                    AdapterKind::Tap => "Existing TAP adapters may belong to another app. Choose the physical Ethernet connection; setup creates its own adapter.",
                    AdapterKind::Bridge => "Existing Windows bridges are not converted by automatic setup.",
                    AdapterKind::Wifi => "Automatic Windows setup currently requires an Ethernet connection.",
                    _ => "This connection does not have a verified physical device identity for automatic setup.",
                }.into()) };
                adapters.push(Adapter { id: AdapterId::new(&id)?, name, kind, link_up: entry.OperStatus == 1,
                    eligible, reason, fingerprint });
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
