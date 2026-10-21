//! Disposable-VM acceptance for the production coordinator plus macOS backend.
#[cfg(target_os = "macos")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use eink_bridge::{coordinator::Coordinator, macos_service::*, macos_platform::MacPlatform, model::*};
    use std::{ffi::CStr, path::Path, os::unix::fs::DirBuilderExt};
    let args = std::env::args().collect::<Vec<_>>();
    if args.len() != 4 || !matches!(args[1].as_str(), "--exercise" | "--crash-apply") { return Err("requires --exercise exact-hostname pinned-payload".into()); }
    let mut name = [0i8; 256];
    if unsafe { libc::gethostname(name.as_mut_ptr(), name.len()) } != 0 || unsafe { CStr::from_ptr(name.as_ptr()) }.to_str()? != args[2] { return Err("wrong disposable host".into()); }
    let model = std::process::Command::new("/usr/sbin/sysctl").args(["-n", "hw.model"]).output()?;
    if !String::from_utf8(model.stdout)?.starts_with("VirtualMac") { return Err("requires a clean disposable VirtualMac".into()); }
    let native = NativeService::new()?;
    let user = LocalUser::new(501, 20)?;
    if args[1] == "--crash-apply" {
        let root = Path::new(INSTALL_ROOT).join("gate-journal");
        let mut coordinator = Coordinator::open(&root, "uid:501", CrashAfterApply(MacPlatform::for_user(user)?))?;
        coordinator.apply(&OperationId::new(&args[3])?, 101)?;
        return Err("fault injection did not terminate".into());
    }
    if Path::new(INSTALL_ROOT).exists() { return Err("preexisting test resources".into()); }
    native.install_payload(&std::fs::read(&args[3])?)?;
    let root = Path::new(INSTALL_ROOT).join("gate-journal");
    std::fs::DirBuilder::new().mode(0o700).create(&root)?;
    let mut coordinator = Coordinator::open(&root, "uid:501", MacPlatform::for_user(user)?)?;
    let plan = coordinator.plan(PlanRequest::Configure { adapter_id: AdapterId::new("en0")? }, 100)?;
    let crash_id = plan.id.clone();
    drop(coordinator);
    let dead = std::process::Command::new(std::env::current_exe()?).args(["--crash-apply", &args[2], crash_id.as_str()]).status()?;
    assert_eq!(dead.code(), Some(42));
    let mut recovery = Coordinator::open(&root, "service", MacPlatform::for_recovery()?)?;
    assert_eq!(recovery.recover_startup()?.unwrap().state, OperationState::Cancelled);
    let failed_service = ServiceSpec::new(user, ResourceId::new(crash_id.as_str())?, &eink_bridge::inspect()?.adapters.into_iter().find(|a| a.id.as_str() == "en0").unwrap())?;
    assert!(!failed_service.plist_path().exists() && !failed_service.socket().exists());
    println!("coordinatorRecoveryAfterProcessDeath=passed");
    drop(recovery);
    let mut coordinator = Coordinator::open(&root, "uid:501", MacPlatform::for_user(user)?)?;
    let plan = coordinator.plan(PlanRequest::Configure { adapter_id: AdapterId::new("en0")? }, 100)?;
    // A separate helper lifetime must resolve its planned service entirely from
    // the protected snapshot, without an in-memory adapter/resource cache.
    drop(coordinator);
    let mut coordinator = Coordinator::open(&root, "uid:501", MacPlatform::for_user(user)?)?;
    assert_eq!(coordinator.apply(&plan.id, 101)?.state, OperationState::Ready);
    let resource = coordinator.resources()?.pop().ok_or("missing installed resource")?;
    println!("coordinatorConfigureAfterRestart=passed");
    let repair = coordinator.plan(PlanRequest::Repair { resource_id: resource.id.clone() }, 102)?;
    drop(coordinator);
    let mut coordinator = Coordinator::open(&root, "uid:501", MacPlatform::for_user(user)?)?;
    assert_eq!(coordinator.apply(&repair.id, 103)?.state, OperationState::Ready);
    println!("coordinatorRepairAfterRestart=passed");
    let lease = coordinator.lease(&resource.id)?;
    assert_eq!(coordinator.plan(PlanRequest::Remove { resource_id: resource.id.clone() }, 104).unwrap_err().code, "EBUSY");
    drop(lease);
    let remove = coordinator.plan(PlanRequest::Remove { resource_id: resource.id }, 105)?;
    assert_eq!(coordinator.apply(&remove.id, 106)?.state, OperationState::Ready);
    assert!(coordinator.resources()?.is_empty());
    println!("coordinatorLeaseAndRemoval=passed");
    drop(coordinator);
    std::fs::remove_dir_all(&root)?;
    std::fs::remove_file(Path::new(INSTALL_ROOT).join("socket_vmnet"))?;
    std::fs::remove_dir(INSTALL_ROOT)?;
    let _ = std::fs::remove_dir(SOCKET_ROOT);
    Ok(())
}
#[cfg(not(target_os = "macos"))]
fn main() { panic!("macOS only"); }

#[cfg(target_os = "macos")]
struct CrashAfterApply(eink_bridge::macos_platform::MacPlatform);
#[cfg(target_os = "macos")]
impl eink_bridge::coordinator::PlatformOps for CrashAfterApply {
    type Snapshot = eink_bridge::macos_platform::Snapshot;
    type Mutation = eink_bridge::macos_platform::Mutation;
    fn prepare(&mut self, request: eink_bridge::model::PlanRequest, id: eink_bridge::model::OperationId, now: u64, existing: Option<&eink_bridge::coordinator::Resource<Self::Snapshot>>) -> Result<eink_bridge::coordinator::Prepared<Self::Snapshot, Self::Mutation>, eink_bridge::model::BridgeError> { self.0.prepare(request, id, now, existing) }
    fn fingerprint(&mut self, request: &eink_bridge::model::PlanRequest, snapshot: &Self::Snapshot) -> Result<String, eink_bridge::model::BridgeError> { self.0.fingerprint(request, snapshot) }
    fn apply(&mut self, mutation: &Self::Mutation) -> Result<(), eink_bridge::model::BridgeError> { self.0.apply(mutation)?; std::process::exit(42) }
    fn verify(&mut self, snapshot: &Self::Snapshot) -> Result<(), eink_bridge::model::BridgeError> { self.0.verify(snapshot) }
    fn commit(&mut self, snapshot: &Self::Snapshot) -> Result<(), eink_bridge::model::BridgeError> { self.0.commit(snapshot) }
    fn rollback(&mut self, snapshot: &Self::Snapshot) -> Result<(), eink_bridge::model::BridgeError> { self.0.rollback(snapshot) }
}
