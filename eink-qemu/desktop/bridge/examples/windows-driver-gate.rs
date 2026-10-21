//! Opt-in native device lifecycle test. No physical adapter membership changes.
#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use eink_bridge::{model::ResourceId, windows_device, windows_driver::{self, VerifiedPackage}, windows_security, windows_verify};
    use std::{fs::OpenOptions, io::Write, path::PathBuf};
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 4 || args[1] != "--exercise" || args[2] != std::env::var("COMPUTERNAME")?
        || std::env::var("EINK_BRIDGE_DISPOSABLE_WINDOWS_TESTS").as_deref() != Ok("1") {
        return Err("Use --exercise <disposable-hostname> <protected-fixture-directory> with the native test opt-in.".into());
    }
    windows_security::require_system().map_err(|e| e.message)?;
    let fixture = PathBuf::from(&args[3]);
    let root = OpenOptions::new().read(true).custom_flags(0x02200000).open(&fixture)?;
    use std::os::windows::fs::OpenOptionsExt;
    windows_security::validate_handle(&root, None).map_err(|e| e.message)?;
    let record = fixture.join("pending-device.json");
    if record.exists() { return Err("A native device test needs recovery; inspect pending-device.json before retrying.".into()); }
    let package = VerifiedPackage::open(&fixture.join("driver")).map_err(|e| e.message)?;
    println!("pinnedPackageAndCatalog=verified");
    assert!(OpenOptions::new().write(true).open(fixture.join("driver/tap0901.sys")).is_err());
    println!("packageReplacementDuringInstall=blocked");
    assert!(VerifiedPackage::open(&fixture.join("corrupt-driver")).is_err());
    println!("corruptPackage=rejected");
    let before = windows_device::enumerate().map_err(|e| e.message)?;
    let mut random = [0u8; 16]; getrandom::fill(&mut random)?;
    let resource = ResourceId::new(random.iter().map(|byte| format!("{byte:02x}")).collect::<String>()).map_err(|e| e.message)?;
    let mut file = OpenOptions::new().create_new(true).write(true).open(&record)?;
    serde_json::to_writer(&mut file, &resource)?; file.flush()?; file.sync_all()?; drop(file);
    println!("pendingInstance={}", windows_driver::instance_id(&resource));
    let result = (|| -> Result<(), String> {
        let installed = windows_driver::create(&package, &resource).map_err(|e| e.message)?;
        println!("created={installed:?}");
        if installed.reboot_required { return Err("Native installation requested a reboot; not a successful immediate lifecycle.".into()); }
        let identity = installed.identity.ok_or("Missing installed identity")?;
        assert_eq!(windows_driver::create(&package, &resource).unwrap_err().code, "ECONFLICT");
        windows_verify::probe(&identity.guid).map_err(|e| e.message)?;
        let test_path = fixture.join("windows-transport-tests.exe");
        let test_file = OpenOptions::new().read(true).share_mode(1).custom_flags(0x00200000)
            .open(&test_path).map_err(|e| e.to_string())?;
        windows_security::validate_handle(&test_file, None).map_err(|e| e.message)?;
        let status = std::process::Command::new(test_path)
            .args(["ordinary_user_cannot_forge_server_and_receives_read_only_lease", "--include-ignored", "--test-threads=1"])
            .env("EINK_BRIDGE_TEST_TAP_GUID", &identity.guid).status().map_err(|e| e.to_string())?;
        if !status.success() { return Err("The new adapter's standard-user access check failed.".into()); }
        println!("dedicatedTapOrdinaryUserOpen=passed");
        let mut stale = identity.clone(); stale.guid = "{00000000-0000-0000-0000-000000000000}".into();
        assert_eq!(windows_driver::remove(&resource, Some(&stale)).unwrap_err().code, "ECONFLICT");
        assert!(!windows_driver::remove(&resource, Some(&identity)).map_err(|e| e.message)?);
        assert!(!windows_driver::remove(&resource, None).map_err(|e| e.message)?);
        Ok(())
    })();
    if let Err(error) = &result {
        eprintln!("nativeGateFailure={error}");
        if let Err(cleanup) = windows_driver::remove(&resource, None) { return Err(format!("{error}; recovery failed: {}. Retained {}", cleanup.message, record.display()).into()); }
    }
    let after = windows_device::enumerate().map_err(|e| e.message)?;
    if before != after { return Err("Native device baseline differs after cleanup; record retained.".into()); }
    // Ensure the original shared driver remains usable after dedicated removal.
    for identity in after.values().filter(|device| device.is_tap()) { windows_verify::probe(&identity.guid).map_err(|e| e.message)?; }
    std::fs::remove_file(record)?;
    println!("originalDevicesAndSharedTap=preserved; pendingRecord=removed");
    result.map_err(Into::into)
}
#[cfg(not(windows))]
fn main() { eprintln!("Run only in an explicitly selected disposable Windows x64 VM."); }
