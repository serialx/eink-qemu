//! Read-only diagnostics for the disposable Windows recovery VM.
#[cfg(not(windows))]
fn main() { panic!("Requires the disposable Windows VM"); }
#[cfg(windows)]
fn main() { if let Err(error) = run() { eprintln!("{error:?}"); std::process::exit(1); } }
#[cfg(windows)]
fn run() -> Result<(), Box<dyn std::error::Error>> {
    use eink_bridge::{windows_device, windows_network_bridge, windows_profile,
        windows_saved, windows_security, windows_topology::Topology};
    use std::{fs::OpenOptions, io::Write, os::windows::fs::OpenOptionsExt, time::Instant};
    windows_security::require_system()?;
    if std::env::var("COMPUTERNAME").as_deref() != Ok("WIN-SUNSKHPNVMO") {
        return Err("Wrong disposable host".into());
    }
    let file = OpenOptions::new().read(true).share_mode(7)
        .open(r"C:\ProgramData\eink-qemu\bridge\operation.json")?;
    windows_security::validate_handle(&file, None)?;
    let record: serde_json::Value = serde_json::from_reader(file)?;
    let snapshot = &record["prepared"]["snapshot"];
    let physical = serde_json::from_value::<windows_device::DeviceIdentity>(snapshot["physical"].clone())?;
    let bridge = serde_json::from_value::<windows_device::DeviceIdentity>(snapshot["bridge"]["device"].clone())?;
    macro_rules! inspect {
        ($name:expr, $expr:expr) => {{
            println!("starting={}", $name); std::io::stdout().flush()?;
            let start = Instant::now(); let result = $expr;
            println!("finished={} elapsedMs={} result={result:?}", $name, start.elapsed().as_millis());
            std::io::stdout().flush()?;
        }};
    }
    inspect!("devices", windows_device::enumerate());
    inspect!("topology", Topology::read());
    for (name, device) in [("physical", physical), ("bridge", bridge)] {
        inspect!(format!("{name}-saved"), windows_saved::Saved::read(&device));
        inspect!(format!("{name}-profile"), windows_profile::Snapshot::read(&device).map(|profile| {
            println!("profileJson:{name}={}", serde_json::to_string(&profile).expect("serializable native profile"));
            profile
        }));
        if name == "bridge" { inspect!("bridge-address", windows_network_bridge::address(&device)); }
    }
    Ok(())
}
