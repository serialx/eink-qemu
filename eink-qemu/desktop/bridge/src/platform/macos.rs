use crate::model::*;
use std::{collections::BTreeMap, ffi::{c_char, c_void, CStr}};

#[link(name = "vmnet", kind = "framework")]
extern "C" { fn vmnet_copy_shared_interface_list() -> *mut c_void; }
extern "C" {
    fn xpc_array_get_count(array: *mut c_void) -> usize;
    fn xpc_array_get_string(array: *mut c_void, index: usize) -> *const c_char;
    fn xpc_release(object: *mut c_void);
}
struct Xpc(*mut c_void);
impl Drop for Xpc { fn drop(&mut self) { unsafe { xpc_release(self.0); } } }
struct Addresses(*mut libc::ifaddrs);
impl Drop for Addresses { fn drop(&mut self) { unsafe { libc::freeifaddrs(self.0); } } }

pub fn adapters() -> Result<Vec<Adapter>, BridgeError> {
    // vmnet itself decides eligibility; interface names alone are insufficient.
    let supported = unsafe { vmnet_copy_shared_interface_list() };
    if supported.is_null() { return Err(BridgeError::new("EDISCOVERY", "macOS could not list bridge-capable interfaces.")); }
    let supported = Xpc(supported);
    let mut names = Vec::new();
    for i in 0..unsafe { xpc_array_get_count(supported.0) } {
        let name = unsafe { xpc_array_get_string(supported.0, i) };
        if !name.is_null() { names.push(unsafe { CStr::from_ptr(name) }.to_string_lossy().into_owned()); }
    }
    let mut ptr = std::ptr::null_mut();
    if unsafe { libc::getifaddrs(&mut ptr) } != 0 {
        return Err(BridgeError::new("EDISCOVERY", "macOS could not inspect network interfaces."));
    }
    let _addresses = Addresses(ptr);
    let mut links = BTreeMap::new();
    while !ptr.is_null() {
        let addr = unsafe { &*ptr };
        if !addr.ifa_name.is_null() && !addr.ifa_addr.is_null()
            && unsafe { (*addr.ifa_addr).sa_family as i32 } == libc::AF_LINK {
            let name = unsafe { CStr::from_ptr(addr.ifa_name) }.to_string_lossy().into_owned();
            let link = unsafe { &*addr.ifa_addr.cast::<libc::sockaddr_dl>() };
            // sockaddr_dl has variable-length trailing data; bound by its own length.
            let offset = std::mem::offset_of!(libc::sockaddr_dl, sdl_data) + usize::from(link.sdl_nlen);
            let count = usize::from(link.sdl_alen);
            if count > 0 && offset + count <= usize::from(link.sdl_len) {
                let bytes = unsafe { std::slice::from_raw_parts(addr.ifa_addr.cast::<u8>().add(offset), count) };
                let hardware = bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
                links.insert(name, (hardware, addr.ifa_flags & (libc::IFF_RUNNING as u32) != 0));
            }
        }
        ptr = addr.ifa_next;
    }
    let mut result = Vec::new();
    for name in names {
        if let Some((hardware, link_up)) = links.get(&name) {
            result.push(Adapter { id: AdapterId::new(&name)?, name: name.clone(), kind: AdapterKind::Other,
                link_up: *link_up, eligible: true, reason: None, fingerprint: format!("{name}:{hardware}") });
        }
    }
    Ok(result)
}
