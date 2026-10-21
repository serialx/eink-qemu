//! Explicit disposable-VM fixture. Its original profile remains in a durable
//! record until --restore verifies the original live state again.
#[cfg(target_os = "linux")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use eink_bridge::{linux_network_manager::NetworkManager, linux_profiles::Profile, linux_transaction::{Checkpoint, ProfileChange}, linux_verify::HostSnapshot};
    use std::{fs::OpenOptions, os::unix::fs::OpenOptionsExt};
    let args = std::env::args().collect::<Vec<_>>();
    if args.len() != 3 || args[2] != "eink-bridge-test" || unsafe { libc::geteuid() } != 0
        || std::fs::read_to_string("/etc/hostname")?.trim() != args[2] { return Err("requires exact disposable hostname and root".into()); }
    let path = "/var/tmp/eink-managed-ipv6-fixture.json";
    let nm = NetworkManager::connect()?;
    if args[1] == "--prepare" {
        let active = nm.active_profile("enp0s1")?;
        let before = nm.runtime("enp0s1")?;
        let link = nm.link_identity("enp0s1")?;
        let original = active.profile;
        let mut fixture = original.clone();
        let ipv6 = fixture.get_mut("ipv6").ok_or("missing IPv6 fixture")?;
        if <&str>::try_from(&ipv6["method"])? != "ignore" { return Err("expected original unmanaged IPv6 fixture".into()); }
        ipv6.insert("method".into(), zbus::zvariant::Str::from("link-local").into());
        // NM-managed link-local routes use a different metric than the
        // preexisting kernel-managed fixture. This QA baseline deliberately
        // changes IPv6 policy; it must preserve IPv4 connectivity and restore
        // the complete original IPv6 state at the end of the lifecycle tests.
        ipv6.insert("route-metric".into(), 1024i64.into());
        ipv6.insert("may-fail".into(), false.into());
        let mut record = OpenOptions::new().write(true).create_new(true).mode(0o600).open(path)?;
        serde_json::to_writer(&mut record, &serde_json::json!({"original": original, "fixture": fixture, "before": before}))?;
        record.sync_all()?;
        let checkpoint = Checkpoint::create(&nm, &active.device)?;
        let result = (|| -> Result<(), Box<dyn std::error::Error>> {
            ProfileChange::new(Some(original), Some(fixture))?.apply(&nm)?;
            let active = nm.activate(&active.path, &active.device)?;
            nm.wait_active(&active)?;
            let after = nm.runtime("enp0s1")?;
            if !matches!((&before.ipv4, &after.ipv4), (Some(before), Some(after)) if before.preserved_by(after))
                || nm.link_identity("enp0s1")? != link { return Err("fixture changed IPv4 or link identity".into()); }
            checkpoint.disarm(&nm)?;
            Ok(())
        })();
        if result.is_err() { checkpoint.rollback(&nm)?; }
        result?;
        println!("managedIpv6FixturePreservesIpv4AndLinkIdentity=passed");
    } else if args[1] == "--reapply-after-reboot" {
        // QA only: Netplan may have regenerated the recorded original at boot.
        // Require that exact original before reinstating our explicit fixture
        // so the interrupted lifecycle can perform its normal guarded recovery.
        let record: serde_json::Value = serde_json::from_slice(&std::fs::read(path)?)?;
        let original: Profile = serde_json::from_value(record["original"].clone())?;
        let fixture: Profile = serde_json::from_value(record["fixture"].clone())?;
        ProfileChange::new(Some(original), Some(fixture))?.apply(&nm)?;
        let current = nm.active_profile("enp0s1")?;
        let active = nm.activate(&current.path, &current.device)?;
        nm.wait_active(&active)?;
        println!("recordedQaFixtureReapplied=passed");
    } else if args[1] == "--restore" {
        let record: serde_json::Value = serde_json::from_slice(&std::fs::read(path)?)?;
        let original: Profile = serde_json::from_value(record["original"].clone())?;
        let fixture: Profile = serde_json::from_value(record["fixture"].clone())?;
        let before: HostSnapshot = serde_json::from_value(record["before"].clone())?;
        ProfileChange::new(Some(fixture), Some(original))?.apply(&nm)?;
        let current = nm.active_profile("enp0s1")?;
        let active = nm.activate(&current.path, &current.device)?;
        nm.wait_active(&active)?;
        nm.wait_preserved("enp0s1", &before)?;
        std::fs::remove_file(path)?;
        println!("originalUnmanagedIpv6ProfileAndRuntimeRestored=passed");
    } else { return Err("expected --prepare or --restore".into()); }
    Ok(())
}
#[cfg(not(target_os = "linux"))]
fn main() { panic!("Linux only"); }
