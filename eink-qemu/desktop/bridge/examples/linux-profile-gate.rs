#[cfg(target_os = "linux")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use eink_bridge::{linux_network_manager::NetworkManager, linux_profiles::{migrate, same_configuration, Profile}};
    let args = std::env::args().collect::<Vec<_>>();
    if args.len() != 4 || !matches!(args[1].as_str(), "--inspect" | "--exercise") { return Err("requires --inspect/--exercise exact-hostname adapter".into()); }
    if std::fs::read_to_string("/etc/hostname")?.trim() != args[2] { return Err("wrong disposable host".into()); }
    let nm = NetworkManager::connect()?;
    let active = nm.active_profile(&args[3])?;
    let bytes = serde_json::to_vec(&active.profile)?;
    let decoded: Profile = serde_json::from_slice(&bytes)?;
    assert!(same_configuration(&active.profile, &decoded));
    let mut migration = migrate(&decoded, &args[3], "ebrgatetest", "fd77321a-8520-4d05-8075-420b465d0846", "29457488-31f2-47e2-a46a-871e32b69271")?;
    assert_eq!(migration.bridge["ipv4"], decoded["ipv4"]);
    assert_eq!(migration.bridge["ipv6"], decoded["ipv6"]);
    if args[1] == "--exercise" {
        // Inactive, disposable profiles only. This gate never activates a
        // connection, changes the original profile or creates a kernel bridge.
        for profile in [&mut migration.bridge, &mut migration.port] {
            profile.get_mut("connection").unwrap().insert("autoconnect".into(), false.into());
        }
        let record = "/var/tmp/eink-linux-profile-gate-record.json";
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(record)?;
        serde_json::to_writer(&mut file, &migration)?;
        file.sync_all()?;
        for (label, profile) in [("bridge", &migration.bridge), ("port", &migration.port)] {
            let path = nm.add_profile(profile)?;
            let saved = nm.read_profile(&path)?;
            // Deletion rechecks the native profile against the just-read
            // expected content and refuses an intervening change.
            nm.delete_profile(&saved)?;
            if !same_configuration(profile, &saved) {
                println!("normalizedProfile={label}");
                for (section, fields) in saved.iter() {
                    for (key, value) in fields {
                        if profile.get(section).and_then(|s| s.get(key)) != Some(value) { println!("normalizedField={section}.{key} value={value:?}"); }
                    }
                }
                return Err("NetworkManager normalized the new profile; review before lifecycle implementation".into());
            }
        }
        std::fs::remove_file(record)?;
        println!("nativeInactiveProfileCreateCompareRemove=passed");
    }
    println!("nativeNetworkManagerSnapshotRoundtrip=passed");
    println!("nativeNetworkManagerProfileMigration=passed");
    Ok(())
}
#[cfg(not(target_os = "linux"))]
fn main() { panic!("Linux only"); }
