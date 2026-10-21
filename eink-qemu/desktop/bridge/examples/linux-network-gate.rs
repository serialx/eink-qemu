//! Disposable VM gate for the production network lifecycle, including process
//! death at each step. --recover uses only the durable record from a prior run.
#[cfg(target_os = "linux")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use eink_bridge::{linux_network::{NetworkIdentity, NetworkSetup, NetworkRemoval}, linux_network_manager::NetworkManager, linux_transaction::Checkpoint};
    use serde::{Deserialize, Serialize};
    use std::{fs::OpenOptions, os::unix::fs::OpenOptionsExt};
    #[derive(Serialize, Deserialize)]
    struct Record {
        setup: NetworkSetup, checkpoint: Option<Checkpoint>,
        #[serde(default)] fixture_change: Option<eink_bridge::linux_transaction::ProfileChange>,
        #[serde(default)] removal: Option<NetworkRemoval>,
    }
    let args = std::env::args().collect::<Vec<_>>();
    if args.len() < 3 || args[2] != "eink-bridge-test" || unsafe { libc::geteuid() } != 0
        || std::fs::read_to_string("/etc/hostname")?.trim() != args[2] { return Err("requires exact disposable hostname and root".into()); }
    let path = "/var/tmp/eink-network-lifecycle-record.json";
    let nm = NetworkManager::connect()?;
    let undo_fixture = |record: &Record| -> Result<(), Box<dyn std::error::Error>> {
        if let Some(change) = &record.fixture_change { change.reverse().apply(&nm)?; }
        Ok(())
    };
    let persist = |record: &Record| -> Result<(), Box<dyn std::error::Error>> {
        let mut next = tempfile::NamedTempFile::new_in("/var/tmp")?;
        serde_json::to_writer(&mut next, record)?;
        next.as_file().sync_all()?;
        next.persist(path)?;
        std::fs::File::open("/var/tmp")?.sync_all()?;
        Ok(())
    };
    if args[1] == "--recover" {
        let record: Record = serde_json::from_slice(&std::fs::read(path)?)?;
        undo_fixture(&record)?;
        if let Some(removal) = &record.removal {
            removal.rollback(&nm, record.checkpoint.as_ref())?;
            record.setup.verify(&nm)?;
            println!("interruptedRemovalRestoredCommittedBridge=passed");
        }
        record.setup.restore(&nm, if record.removal.is_some() { None } else { record.checkpoint.as_ref() })?;
        std::fs::remove_file(path)?;
        println!("nativeNetworkJournalRecovery=passed");
        return Ok(());
    }
    if !matches!(args[1].as_str(), "--commit" | "--cancel" | "--interrupt" | "--interrupt-commit" | "--stale-mtu" | "--external-edit" | "--committed-edit" | "--reject-generated" | "--remove-cancel" | "--remove-keep" | "--remove-interrupt" | "--remove-external-edit" | "--remove-interrupt-commit") { return Err("unknown lifecycle test mode".into()); }
    let interrupt = if args[1] == "--interrupt" { Some(args.get(3).ok_or("missing step")?.parse::<usize>()?) } else { None };
    if interrupt.is_some_and(|step| step > 5) { return Err("step must be 0 through 5".into()); }
    let identity = NetworkIdentity::new("enp0s1", "einklife0", "b015a698-6c07-4795-a98d-1ff70044e932", "ca04bcbb-2b90-4349-a0eb-9b73ab5158fc")?;
    if args[1] == "--reject-generated" {
        let active = nm.active_profile("enp0s1")?;
        let before = nm.runtime("enp0s1")?;
        let error = NetworkSetup::prepare(&nm, identity).expect_err("generated profiles must be rejected");
        if error.code != "EUNSUPPORTED" || !error.message.contains("temporary or generated") { return Err(error.into()); }
        if !eink_bridge::linux_profiles::same_configuration(&active.profile, &nm.active_profile("enp0s1")?.profile) { return Err("rejected plan changed the profile".into()); }
        before.verify_preserved(&nm.runtime("enp0s1")?)?;
        println!("generatedProfileRejectedWithoutMutation=passed");
        return Ok(());
    }
    let setup = NetworkSetup::prepare(&nm, identity)?;
    if args[1] == "--stale-mtu" {
        let (_, original_mtu) = nm.link_identity("enp0s1")?;
        let altered_mtu = if original_mtu == 1400 { 1450 } else { 1400 };
        let set = |mtu: u32| -> Result<(), Box<dyn std::error::Error>> {
            if !std::process::Command::new("/usr/sbin/ip").args(["link", "set", "dev", "enp0s1", "mtu", &mtu.to_string()]).status()?.success() { return Err("MTU fixture command failed".into()); }
            Ok(())
        };
        set(altered_mtu)?;
        let result = setup.begin(&nm);
        set(original_mtu)?;
        match result {
            Ok(checkpoint) => { checkpoint.disarm(&nm)?; return Err("stale MTU plan was accepted".into()); },
            Err(error) if error.code == "ECONFLICT" => (),
            Err(error) => return Err(error.into()),
        }
        println!("staleMtuPlanRejected=passed");
        return Ok(());
    }
    let mut record = Record { setup, checkpoint: None, fixture_change: None, removal: None };
    let mut file = OpenOptions::new().write(true).create_new(true).mode(0o600).open(path)?;
    serde_json::to_writer(&mut file, &record)?;
    file.sync_all()?;
    record.checkpoint = Some(record.setup.begin(&nm)?);
    persist(&record)?;
    if interrupt == Some(0) { std::process::exit(71); }
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        for (index, step) in NetworkSetup::steps().into_iter().enumerate() {
            record.setup.apply(&nm, step)?;
            if interrupt == Some(index + 1) { std::process::exit(71); }
        }
        record.setup.verify(&nm)?;
        let checkpoint = record.checkpoint.as_ref().unwrap();
        checkpoint.confirm_window(&nm)?;
        if matches!(args[1].as_str(), "--remove-cancel" | "--remove-keep" | "--remove-interrupt" | "--remove-external-edit" | "--remove-interrupt-commit") {
            let stop = if args[1] == "--remove-interrupt" { Some(args.get(3).ok_or("missing removal step")?.parse::<usize>()?) } else { None };
            if stop.is_some_and(|step| step > 4) { return Err("removal step must be 0 through 4".into()); }
            record.setup.commit(&nm, checkpoint)?;
            persist(&record)?;
            record.removal = Some(NetworkRemoval::prepare(&nm, &record.setup)?);
            record.checkpoint = None;
            persist(&record)?;
            record.checkpoint = Some(record.removal.as_ref().unwrap().begin(&nm)?);
            persist(&record)?;
            if stop == Some(0) { std::process::exit(71); }
            let removal = record.removal.as_ref().unwrap();
            for (index, step) in NetworkRemoval::steps().into_iter().enumerate() {
                removal.apply(&nm, step)?;
                if stop == Some(index + 1) { std::process::exit(71); }
            }
            removal.verify(&nm)?;
            let checkpoint = record.checkpoint.as_ref().unwrap();
            if matches!(args[1].as_str(), "--remove-keep" | "--remove-interrupt-commit") {
                removal.commit(&nm, checkpoint)?;
                if args[1] == "--remove-interrupt-commit" { std::process::exit(71); }
                println!("nativeRemovalKept=passed");
            } else {
                if args[1] == "--remove-external-edit" {
                    let before = nm.active_profile("enp0s1")?.profile;
                    let mut edited = before.clone();
                    edited.get_mut("connection").unwrap().insert("autoconnect-priority".into(), 77i32.into());
                    record.fixture_change = Some(eink_bridge::linux_transaction::ProfileChange::new(Some(before), Some(edited.clone()))?);
                    persist(&record)?;
                    record.fixture_change.as_ref().unwrap().apply(&nm)?;
                    let error = removal.rollback(&nm, Some(checkpoint)).expect_err("removal recovery must preserve external edits");
                    if error.code != "ECONFLICT" || checkpoint.exists(&nm)?
                        || !eink_bridge::linux_profiles::same_configuration(&edited, &nm.active_profile("enp0s1")?.profile) { return Err("removal recovery overwrote an external edit or left its timer active".into()); }
                    undo_fixture(&record)?;
                    record.fixture_change = None;
                    persist(&record)?;
                    println!("removalExternalEditPreservedAndTimerDisarmed=passed");
                }
                removal.rollback(&nm, Some(checkpoint))?;
                record.setup.verify(&nm)?;
                println!("cancelledRemovalRestoredCommittedBridge=passed");
                record.setup.restore(&nm, None)?;
            }
            return Ok(());
        }
        if matches!(args[1].as_str(), "--external-edit" | "--committed-edit") {
            let committed_edit = args[1] == "--committed-edit";
            if committed_edit { record.setup.commit(&nm, checkpoint)?; persist(&record)?; }
            let before = if committed_edit { record.setup.migration().bridge.clone() } else { record.setup.migration().original_disabled.clone() };
            let mut edited = before.clone();
            if committed_edit { edited.get_mut("connection").unwrap().insert("autoconnect".into(), false.into()); }
            else { edited.get_mut("connection").unwrap().insert("autoconnect-priority".into(), 77i32.into()); }
            if eink_bridge::linux_profiles::same_configuration(&before, &edited) { return Err("external-edit fixture already present".into()); }
            record.fixture_change = Some(eink_bridge::linux_transaction::ProfileChange::new(Some(before), Some(edited.clone()))?);
            persist(&record)?;
            record.fixture_change.as_ref().unwrap().apply(&nm)?;
            let error = record.setup.restore(&nm, Some(checkpoint)).expect_err("an external edit must refuse restoration");
            if error.code != "ECONFLICT" || checkpoint.exists(&nm)? { return Err("restoration did not preserve the edit and disarm its timer".into()); }
            let id = <&str>::try_from(&edited["connection"]["uuid"])?;
            let current = nm.find_profile(id)?.ok_or("edited profile disappeared")?.1;
            if !eink_bridge::linux_profiles::same_configuration(&edited, &current) { return Err("external edit was overwritten".into()); }
            undo_fixture(&record)?;
            record.fixture_change = None;
            persist(&record)?;
            record.setup.restore(&nm, None)?;
            println!("nativeExternalEditPreservedAndTimerDisarmed=passed committed={committed_edit}");
            return Ok(());
        }
        if matches!(args[1].as_str(), "--commit" | "--interrupt-commit") {
            record.setup.commit(&nm, checkpoint)?;
            if args[1] == "--interrupt-commit" { std::process::exit(71); }
            persist(&record)?;
            // Use a freshly deserialized object after native commit. The
            // checkpoint has gone; restoration must use durable profile data.
            record = serde_json::from_slice(&std::fs::read(path)?)?;
            record.setup.restore(&nm, None)?;
            println!("nativeKeepThenRemoveWithoutCheckpoint=passed");
        } else {
            record.setup.restore(&nm, Some(checkpoint))?;
            println!("nativeCancelRestoresNetwork=passed");
        }
        Ok(())
    })();
    if result.is_err() {
        undo_fixture(&record)?;
        if let Some(removal) = &record.removal { removal.rollback(&nm, record.checkpoint.as_ref())?; }
        record.setup.restore(&nm, if record.removal.is_some() { None } else { record.checkpoint.as_ref() })?;
    }
    std::fs::remove_file(path)?;
    result
}
#[cfg(not(target_os = "linux"))]
fn main() { panic!("Linux only"); }
