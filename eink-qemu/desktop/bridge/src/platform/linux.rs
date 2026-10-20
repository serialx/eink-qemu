use crate::model::*;
use std::{fs, path::Path};

pub fn inspect() -> Result<(Vec<Adapter>, Capabilities), BridgeError> {
    let managed = zbus::blocking::Connection::system().ok().and_then(|connection| {
        let proxy = zbus::blocking::Proxy::new(&connection, "org.freedesktop.NetworkManager",
            "/org/freedesktop/NetworkManager", "org.freedesktop.NetworkManager").ok()?;
        proxy.get_property::<String>("Version").ok()
    }).is_some();
    let adapters = adapters_at(Path::new("/sys/class/net"), managed)?;
    Ok((adapters, Capabilities { create: managed, existing_bridge: true,
        reason: if managed { None } else { Some("Automatic bridge creation needs NetworkManager. You can select an existing bridge.".into()) } }))
}
fn adapters_at(root: &Path, managed: bool) -> Result<Vec<Adapter>, BridgeError> {
    let entries = fs::read_dir(root).map_err(|e| BridgeError::new("EDISCOVERY", format!("Cannot inspect network interfaces: {e}")))?;
    let mut adapters = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| BridgeError::new("EDISCOVERY", e.to_string()))?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let read = |file: &str| fs::read_to_string(path.join(file)).unwrap_or_default().trim().to_string();
        if name == "lo" { continue; }
        let kind = if path.join("bridge").is_dir() { AdapterKind::Bridge }
            else if path.join("wireless").is_dir() { AdapterKind::Wifi }
            else if path.join("device").exists() && read("type") == "1" { AdapterKind::Ethernet }
            else { AdapterKind::Other };
        let hardware = read("address");
        if hardware.is_empty() { continue; } // An interface disappeared during enumeration.
        let eligible = kind == AdapterKind::Bridge || (kind == AdapterKind::Ethernet && managed);
        let reason = if eligible { None } else { Some(match kind {
            AdapterKind::Wifi => "A Wi-Fi station cannot be added to an ordinary Linux bridge.",
            AdapterKind::Ethernet => "Automatic bridge creation needs NetworkManager.",
            _ => "This interface cannot be converted automatically.",
        }.into()) };
        let device = fs::canonicalize(path.join("device")).ok().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
        adapters.push(Adapter { id: AdapterId::new(&name)?, name: name.clone(), kind,
            link_up: read("carrier") == "1", eligible, reason, fingerprint: format!("{name}:{hardware}:{device}") });
    }
    adapters.sort_by(|a, b| a.id.as_str().cmp(b.id.as_str()));
    Ok(adapters)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn an_existing_bridge_is_available_without_network_manager() {
        let tmp = tempfile::tempdir().unwrap(); let br = tmp.path().join("br0");
        fs::create_dir_all(br.join("bridge")).unwrap();
        fs::write(br.join("address"), "02:00:00:00:00:01\n").unwrap();
        fs::write(br.join("carrier"), "1\n").unwrap();
        let result = adapters_at(tmp.path(), false).unwrap();
        assert_eq!(result.len(), 1); assert!(result[0].eligible);
        assert_eq!(result[0].kind, AdapterKind::Bridge);
    }
}
