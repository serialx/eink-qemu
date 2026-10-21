//! Disposable host gate: checkpoint rollback must not delete profiles created
//! independently after the checkpoint. No profile is activated by this test.
#[cfg(target_os = "linux")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use eink_bridge::{linux_network_manager::NetworkManager, linux_profiles::{migrate, same_configuration}, model::BridgeError};
    use zbus::zvariant::OwnedValue;
    let args = std::env::args().collect::<Vec<_>>();
    if args.len() != 3 || unsafe { libc::geteuid() } != 0 { return Err("requires root, exact disposable hostname, and Ethernet interface".into()); }
    let mut name = [0 as libc::c_char; 256];
    if unsafe { libc::gethostname(name.as_mut_ptr(), name.len()) } != 0 || unsafe { std::ffi::CStr::from_ptr(name.as_ptr()) }.to_str()? != args[1] || args[1] != "eink-bridge-test" { return Err("wrong disposable host".into()); }
    let nm = NetworkManager::connect()?;
    let original = nm.active_profile(&args[2])?;
    let identity = "bb957d43-cbb8-4e72-994e-3b427472b621";
    if nm.find_profile(identity)?.is_some() { return Err("test profile already exists".into()); }
    let mut profile = migrate(&original.profile, &args[2], "einkcheck0", identity, "2329eacf-c0e8-49ca-aa17-04a29624e23d")?.bridge;
    profile.get_mut("connection").unwrap().insert("autoconnect".into(), OwnedValue::from(false));
    let checkpoint = nm.checkpoint(&original.device, 60)?;
    let result = (|| -> Result<(), BridgeError> {
        nm.add_profile(&profile)?;
        nm.rollback_checkpoint(&checkpoint)?;
        let saved = nm.find_profile(identity)?.ok_or_else(|| BridgeError::new("EDATALOSS", "Rollback deleted an unrelated profile created after the checkpoint."))?;
        if !same_configuration(&profile, &saved.1) { return Err(BridgeError::new("ECONFLICT", "Rollback modified the unrelated profile.")); }
        let active = nm.active_profile(&args[2])?;
        if !same_configuration(&original.profile, &active.profile) { return Err(BridgeError::new("EROLLBACK", "Original connection was not preserved.")); }
        Ok(())
    })();
    // Only the exact profile created by this test may be removed.
    nm.delete_profile(&profile)?;
    if nm.checkpoints()?.contains(&checkpoint) { nm.commit_checkpoint(&checkpoint)?; }
    result?;
    println!("unrelatedProfileSurvivesCheckpointRollback=passed");
    println!("originalActiveProfilePreserved=passed");
    Ok(())
}
#[cfg(not(target_os = "linux"))]
fn main() { panic!("Linux only"); }
