//! Irreversible device deletion starts only after confirmation is durable.
use eink_bridge::{coordinator::{Coordinator, PlatformOps, Prepared, Resource}, model::*};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

#[derive(Clone, Serialize, Deserialize)]
struct Snapshot { remove: bool }
#[derive(Clone)]
struct Host { device: Arc<Mutex<bool>>, cleanups: Arc<Mutex<usize>>, rollbacks: Arc<Mutex<usize>>, crash: bool, fail: bool }
impl Host { fn new() -> Self { Self { device: Arc::new(Mutex::new(true)), cleanups: Arc::new(Mutex::new(0)), rollbacks: Arc::new(Mutex::new(0)), crash: false, fail: false } } }
impl PlatformOps for Host {
    type Snapshot = Snapshot;
    type Mutation = ();
    fn prepare(&mut self, request: PlanRequest, id: OperationId, now: u64, _: Option<&Resource<Snapshot>>) -> Result<Prepared<Snapshot, ()>, BridgeError> {
        let remove = matches!(request, PlanRequest::Remove { .. });
        Ok(Prepared { plan: BridgePlan { id, request, fingerprint: "device".into(), changes: vec!["Change bridge membership".into()], requires_authorization: true, may_disconnect: true, expires_at: now + 300 },
            snapshot: Snapshot { remove }, mutations: vec![()] })
    }
    fn fingerprint(&mut self, _: &PlanRequest, _: &Snapshot) -> Result<String, BridgeError> { Ok("device".into()) }
    fn apply(&mut self, _: &()) -> Result<(), BridgeError> { Ok(()) }
    fn verify(&mut self, _: &Snapshot) -> Result<(), BridgeError> { Ok(()) }
    fn commit(&mut self, _: &Snapshot) -> Result<(), BridgeError> { Ok(()) }
    fn rollback(&mut self, _: &Snapshot) -> Result<(), BridgeError> {
        *self.rollbacks.lock().unwrap() += 1;
        assert!(*self.device.lock().unwrap(), "a deleted device identity cannot be restored");
        Ok(())
    }
    fn needs_committed_cleanup(&self, snapshot: &Snapshot) -> bool { snapshot.remove }
    fn cleanup_committed(&mut self, _: &Snapshot) -> Result<(), BridgeError> {
        *self.cleanups.lock().unwrap() += 1;
        if self.fail { return Err(BridgeError { code: "ECONFLICT".into(), message: "Device was edited by its administrator".into(), detail: Some("Bridge membership no longer matches".into()) }); }
        *self.device.lock().unwrap() = false;
        assert!(!self.crash, "process stopped after irreversible device deletion");
        Ok(())
    }
}
fn setup(path: &std::path::Path, host: Host) -> (Coordinator<Host>, ResourceId) {
    let mut coordinator = Coordinator::open(path, "owner", host).unwrap();
    let plan = coordinator.plan(PlanRequest::Configure { adapter_id: AdapterId::new("ethernet").unwrap() }, 100).unwrap();
    coordinator.apply(&plan.id, 101).unwrap(); coordinator.keep(&plan.id, 102).unwrap();
    (coordinator, ResourceId::new(plan.id.as_str()).unwrap())
}
#[test]
fn cancelling_before_confirmation_preserves_the_device() {
    let root = tempfile::tempdir().unwrap(); let host = Host::new();
    let (mut coordinator, id) = setup(root.path(), host.clone());
    let plan = coordinator.plan(PlanRequest::Remove { resource_id: id }, 103).unwrap();
    coordinator.apply(&plan.id, 104).unwrap();
    assert_eq!(coordinator.cancel(&plan.id).unwrap().state, OperationState::Cancelled);
    assert!(*host.device.lock().unwrap()); assert_eq!(*host.cleanups.lock().unwrap(), 0);
    assert_eq!(coordinator.resources().unwrap().len(), 1);
}
#[test]
fn a_crash_after_deletion_finishes_cleanup_without_rollback_or_a_new_lease() {
    let root = tempfile::tempdir().unwrap(); let mut host = Host::new(); host.crash = true;
    let (mut coordinator, id) = setup(root.path(), host.clone());
    let plan = coordinator.plan(PlanRequest::Remove { resource_id: id.clone() }, 103).unwrap();
    coordinator.apply(&plan.id, 104).unwrap();
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| coordinator.keep(&plan.id, 105))).is_err());
    assert!(!*host.device.lock().unwrap());
    let durable: serde_json::Value = serde_json::from_reader(std::fs::File::open(root.path().join("operation.json")).unwrap()).unwrap();
    assert_eq!(durable["version"], 2, "older helpers must refuse an irreversible pending cleanup");
    assert_eq!(durable["operation"]["state"], "finalizing");
    assert!(matches!(coordinator.lease(&id), Err(error) if error.code == "EBUSY"));
    assert!(matches!(coordinator.verify_resource(&id), Err(error) if error.code == "EBUSY"));
    assert_eq!(coordinator.plan(PlanRequest::Remove { resource_id: id }, 106).unwrap_err().code, "ERECOVERY");
    drop(coordinator);
    host.crash = false;
    let mut recovered = Coordinator::open(root.path(), "service", host.clone()).unwrap();
    assert_eq!(recovered.recover_startup().unwrap().unwrap().state, OperationState::Ready);
    assert_eq!(*host.rollbacks.lock().unwrap(), 0);
    let owner = Coordinator::open(root.path(), "owner", host.clone()).unwrap();
    assert!(owner.resources().unwrap().is_empty());
    recovered.recover_startup().unwrap();
    assert_eq!(*host.cleanups.lock().unwrap(), 2, "completed cleanup must not run again");
}
#[test]
fn cleanup_conflicts_are_retained_until_an_explicit_owner_retry() {
    let root = tempfile::tempdir().unwrap(); let mut host = Host::new(); host.fail = true;
    let (mut coordinator, id) = setup(root.path(), host.clone());
    let plan = coordinator.plan(PlanRequest::Remove { resource_id: id }, 103).unwrap();
    coordinator.apply(&plan.id, 104).unwrap();
    let failed = coordinator.keep(&plan.id, 105).unwrap();
    assert_eq!(failed.state, OperationState::Failed);
    let error = failed.error.unwrap();
    assert_eq!(error.code, "ECLEANUP");
    assert_eq!(error.detail.as_deref(), Some("ECONFLICT: Device was edited by its administrator\nBridge membership no longer matches"));
    coordinator.recover(999).unwrap(); coordinator.recover_startup().unwrap();
    assert_eq!(*host.cleanups.lock().unwrap(), 1, "watchdog and restart must preserve a known conflict");
    let service = Coordinator::open(root.path(), "service", host.clone()).unwrap();
    assert!(matches!(eink_bridge::recovery::RecoveryWorker::start(service, std::time::Duration::from_millis(50), || 999),
        Err(error) if error.code == "ECLEANUP"));
    let mut other = Coordinator::open(root.path(), "another-user", host.clone()).unwrap();
    assert_eq!(other.cancel(&plan.id).unwrap_err().code, "EPERM");
    host.fail = false;
    let mut owner = Coordinator::open(root.path(), "owner", host.clone()).unwrap();
    assert_eq!(owner.cancel(&plan.id).unwrap().state, OperationState::Ready, "retry completes confirmed removal; it cannot undo it");
    assert!(owner.resources().unwrap().is_empty());
    assert_eq!(*host.rollbacks.lock().unwrap(), 0);
}
