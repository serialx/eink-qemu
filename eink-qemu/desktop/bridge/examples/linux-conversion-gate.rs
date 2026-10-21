//! Disposable VM only. Exercise real conversion, verify host networking, then
//! restore in all cases. Durable evidence is retained if restoration fails.
#[cfg(target_os = "linux")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use eink_bridge::{linux_network_manager::NetworkManager, linux_profiles::{migrate, same_configuration}, model::BridgeError};
    use std::{fs::OpenOptions, os::unix::fs::OpenOptionsExt};
    use zbus::zvariant::OwnedObjectPath;
    let args = std::env::args().collect::<Vec<_>>();
    let mut name = [0 as libc::c_char; 256];
    if (args.len() != 3 && (args.len() != 4 || args[3] != "--managed-ipv6-fixture")) || unsafe { libc::geteuid() } != 0 || unsafe { libc::gethostname(name.as_mut_ptr(), name.len()) } != 0
        || unsafe { std::ffi::CStr::from_ptr(name.as_ptr()) }.to_str()? != args[1] || args[1] != "eink-bridge-test" { return Err("requires root and exact disposable hostname/interface".into()); }
    let nm = NetworkManager::connect()?;
    let original = nm.active_profile(&args[2])?;
    let before = nm.runtime(&args[2])?;
    let bridge_uuid = "ce1e8774-4312-4d93-a7d9-3a2d19bb89f1";
    let port_uuid = "af7662c1-d697-4103-a44c-3fbd260937aa";
    let bridge_name = "einkconvert0";
    if nm.find_profile(bridge_uuid)?.is_some() || nm.find_profile(port_uuid)?.is_some()
        || std::path::Path::new("/sys/class/net").join(bridge_name).exists() { return Err("preexisting test resource".into()); }
    let record = "/var/tmp/eink-native-conversion-record.json";
    let mut file = OpenOptions::new().write(true).create_new(true).mode(0o600).open(record)?;
    serde_json::to_writer(&mut file, &serde_json::json!({"original": original.profile, "before": before}))?;
    file.sync_all()?;
    let checkpoint = nm.checkpoint(&original.device, 180)?;
    let mut planned = None;
    let result = (|| -> Result<(), BridgeError> {
        let mut source = original.profile.clone();
        if args.len() == 4 {
            // Explicit QA fixture, not production migration: exercise the
            // managed link-local IPv6 case on this cloud image, whose original
            // profile leaves IPv6 unmanaged. The outer checkpoint restores it.
            let ipv6 = source.get_mut("ipv6").ok_or_else(|| BridgeError::new("EFIXTURE", "Missing IPv6 fixture settings"))?;
            if <&str>::try_from(&ipv6["method"]).ok() != Some("ignore") { return Err(BridgeError::new("EFIXTURE", "Expected original unmanaged IPv6 fixture")); }
            ipv6.insert("method".into(), zbus::zvariant::Str::from("link-local").into());
            ipv6.insert("may-fail".into(), false.into());
            nm.update_profile(&original.profile, &source)?;
            let active = nm.activate(&original.path, &original.device)?;
            nm.wait_active(&active)?;
        }
        let baseline = nm.runtime(&args[2])?;
        let (mac, mtu) = nm.link_identity(&args[2])?;
        let mut migration = migrate(&source, &args[2], bridge_name, bridge_uuid, port_uuid)?;
        migration.preserve_runtime(&baseline, mac, mtu)?;
        for profile in [&mut migration.bridge, &mut migration.port] {
            profile.get_mut("connection").unwrap().insert("autoconnect".into(), false.into());
        }
        let mut next = tempfile::NamedTempFile::new_in("/var/tmp").map_err(|e| BridgeError::new("EIO", e.to_string()))?;
        serde_json::to_writer(&mut next, &serde_json::json!({"original": original.profile, "migration": migration, "before": before, "baseline": baseline})).map_err(|e| BridgeError::new("EIO", e.to_string()))?;
        next.as_file().sync_all().map_err(|e| BridgeError::new("EIO", e.to_string()))?;
        next.persist(record).map_err(|e| BridgeError::new("EIO", e.to_string()))?;
        std::fs::File::open("/var/tmp").and_then(|file| file.sync_all()).map_err(|e| BridgeError::new("EIO", e.to_string()))?;
        planned = Some(migration.clone());
        let bridge = nm.add_profile(&migration.bridge)?;
        let port = nm.add_profile(&migration.port)?;
        nm.update_profile(&source, &migration.original_disabled)?;
        let active_bridge = nm.activate(&bridge, &OwnedObjectPath::try_from("/").unwrap())?;
        let active_port = nm.activate(&port, &original.device)?;
        nm.wait_active(&active_port)?;
        nm.wait_active(&active_bridge)?;
        if let Err(error) = nm.wait_preserved(bridge_name, &baseline) {
            println!("before={baseline:?}\nafter={:?}", nm.runtime(bridge_name)?);
            return Err(error);
        }
        if nm.link_identity(bridge_name)? != (mac, mtu) { return Err(BridgeError::new("EHOSTNETWORK", "Link identity changed")); }
        println!("nativeConversionPreservesAddressesRoutesDns=passed");
        Ok(())
    })();
    // Restore the checkpoint before deleting only our exact profiles. A failed
    // cleanup leaves the durable record for explicit recovery, never claims OK.
    nm.rollback_checkpoint(&checkpoint)?;
    if let Some(migration) = planned {
        nm.delete_profile(&migration.port)?;
        nm.delete_profile(&migration.bridge)?;
    }
    if nm.checkpoints()?.contains(&checkpoint) { nm.commit_checkpoint(&checkpoint)?; }
    let restored = nm.active_profile(&args[2])?;
    if !same_configuration(&original.profile, &restored.profile) { return Err("original profile was not restored".into()); }
    nm.wait_preserved(&args[2], &before)?;
    std::fs::remove_file(record)?;
    println!("nativeConversionRestoresOriginalHost=passed");
    result?;
    Ok(())
}
#[cfg(not(target_os = "linux"))]
fn main() { panic!("Linux only"); }
