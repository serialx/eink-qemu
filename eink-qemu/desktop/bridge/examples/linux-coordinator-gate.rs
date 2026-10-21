//! Exercise the production coordinator and packet-helper probe in a disposable
//! VM. The root-owned journal remains available for explicit --recover.
#[cfg(target_os = "linux")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use eink_bridge::{coordinator::Coordinator, linux_platform::LinuxPlatform, model::*};
    use std::{path::Path, os::unix::fs::DirBuilderExt, time::{SystemTime, UNIX_EPOCH}};
    fn now() -> u64 { SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() }
    fn apply(c: &mut Coordinator<LinuxPlatform>, request: PlanRequest, keep: bool) -> Result<Operation, BridgeError> {
        let plan = c.plan(request, now())?;
        let operation = c.apply(&plan.id, now())?;
        if operation.state == OperationState::Verifying && keep { c.keep(&plan.id, now()) } else { Ok(operation) }
    }
    let args = std::env::args().collect::<Vec<_>>();
    if args.len() != 3 || args[2] != "eink-bridge-test" || unsafe { libc::geteuid() } != 0
        || std::fs::read_to_string("/etc/hostname")?.trim() != args[2] { return Err("requires exact disposable host and root".into()); }
    let root = Path::new("/var/tmp/eink-linux-coordinator-gate");
    match std::fs::DirBuilder::new().mode(0o700).create(root) {
        Ok(()) => (),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
        Err(error) => return Err(error.into()),
    }
    let mut c = Coordinator::open(root, "uid:1000", LinuxPlatform::for_user(1000)?)?;
    if args[1] == "--recover" {
        c.recover_startup()?;
        for resource in c.resources()? {
            let operation = apply(&mut c, PlanRequest::Remove { resource_id: resource.id }, true)?;
            if operation.state != OperationState::Ready { return Err(format!("cleanup failed: {operation:?}").into()); }
        }
        println!("coordinatorGateRecovered=passed");
        return Ok(());
    }
    if !matches!(args[1].as_str(), "--run" | "--existing") { return Err("unknown gate mode".into()); }
    if !c.resources()?.is_empty() { return Err("recover the previous coordinator gate first".into()); }
    let adapter = if args[1] == "--existing" { "eqlocaltest" } else { "enp0s1" };
    let external_before = if args[1] == "--existing" {
        Some(std::fs::read_dir("/sys/class/net/eqlocaltest/brif")?.map(|entry| entry.map(|e| e.file_name())).collect::<Result<Vec<_>, _>>()?)
    } else { None };
    let operation = apply(&mut c, PlanRequest::Configure { adapter_id: AdapterId::new(adapter)? }, true)?;
    if operation.state != OperationState::Ready { return Err(format!("configuration failed: {operation:?}").into()); }
    let resource = c.resources()?.pop().ok_or("resource missing after Keep")?;
    let (_, lease) = c.verify_resource(&resource.id)?;
    assert_eq!(c.plan(PlanRequest::Remove { resource_id: resource.id.clone() }, now()).err().ok_or("active lease allowed removal")?.code, "EBUSY");
    drop(lease);
    let repair = apply(&mut c, PlanRequest::Repair { resource_id: resource.id.clone() }, true)?;
    if repair.state != OperationState::Ready { return Err(format!("repair failed: {repair:?}").into()); }
    if args[1] == "--run" {
        let removal = apply(&mut c, PlanRequest::Remove { resource_id: resource.id.clone() }, false)?;
        assert_eq!(removal.state, OperationState::Verifying);
        assert_eq!(c.cancel(&removal.id)?.state, OperationState::Cancelled);
        let (_, lease) = c.verify_resource(&resource.id)?;
        drop(lease);
    }
    let removal = apply(&mut c, PlanRequest::Remove { resource_id: resource.id }, true)?;
    if removal.state != OperationState::Ready { return Err(format!("removal failed: {removal:?}").into()); }
    assert!(c.resources()?.is_empty());
    if let Some(before) = external_before {
        let after = std::fs::read_dir("/sys/class/net/eqlocaltest/brif")?.map(|entry| entry.map(|e| e.file_name())).collect::<Result<Vec<_>, _>>()?;
        assert_eq!(before, after);
        println!("existingBridgeReferenceRemovalPreservesMembership=passed");
    }
    println!("nativeCoordinatorConfigureRepairRemoveLeaseAndOrdinaryProbe=passed");
    Ok(())
}
#[cfg(not(target_os = "linux"))]
fn main() { panic!("Linux only"); }
