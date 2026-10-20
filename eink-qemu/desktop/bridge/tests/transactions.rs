use eink_bridge::{coordinator::{Coordinator, PlatformOps, Prepared}, model::*};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

#[derive(Clone, Serialize, Deserialize)]
struct Snapshot { value: i32 }
#[derive(Clone, Serialize, Deserialize)]
enum Mutation { Change(i32), Fail }
#[derive(Clone)]
struct Host { value: Arc<Mutex<i32>>, fingerprint: Arc<Mutex<String>>, fail: bool, rollback_fails: bool }
impl Host { fn new() -> Self { Self { value: Arc::new(Mutex::new(10)), fingerprint: Arc::new(Mutex::new("original".into())), fail: false, rollback_fails: false } } }
impl PlatformOps for Host {
    type Snapshot = Snapshot;
    type Mutation = Mutation;
    fn prepare(&mut self, request: PlanRequest, id: OperationId, now: u64, existing: Option<&eink_bridge::coordinator::Resource<Snapshot>>) -> Result<Prepared<Snapshot, Mutation>, BridgeError> {
        let snapshot = Snapshot { value: *self.value.lock().unwrap() };
        let target = if matches!(request, PlanRequest::Remove { .. }) {
            existing.unwrap().original.value
        } else { 20 };
        let mut mutations = vec![Mutation::Change(target)];
        if self.fail { mutations.push(Mutation::Fail); }
        Ok(Prepared { plan: BridgePlan { id, request, fingerprint: self.fingerprint.lock().unwrap().clone(), changes: vec!["Change connection".into()], requires_authorization: true, may_disconnect: true, expires_at: now + 300 }, snapshot, mutations })
    }
    fn fingerprint(&mut self, _: &PlanRequest) -> Result<String, BridgeError> { Ok(self.fingerprint.lock().unwrap().clone()) }
    fn apply(&mut self, mutation: &Mutation) -> Result<(), BridgeError> {
        match mutation { Mutation::Change(value) => { *self.value.lock().unwrap() = *value; Ok(()) }, Mutation::Fail => Err(BridgeError::new("EAPPLY", "Failed mutation")) }
    }
    fn verify(&mut self, _: &Snapshot) -> Result<(), BridgeError> { Ok(()) }
    fn rollback(&mut self, snapshot: &Snapshot) -> Result<(), BridgeError> {
        if self.rollback_fails { return Err(BridgeError::new("EIO", "Host restoration failed")); }
        *self.value.lock().unwrap() = snapshot.value; Ok(())
    }
}
fn request() -> PlanRequest { PlanRequest::Configure { adapter_id: AdapterId::new("adapter").unwrap() } }

#[test]
fn a_failed_mutation_restores_the_original_host() {
    let dir = tempfile::tempdir().unwrap(); let mut host = Host::new(); host.fail = true;
    let mut coordinator = Coordinator::open(dir.path(), "user-a", host.clone()).unwrap();
    let plan = coordinator.plan(request(), 100).unwrap();
    let operation = coordinator.apply(&plan.id, 101).unwrap();
    assert_eq!(operation.state, OperationState::Failed);
    assert_eq!(*host.value.lock().unwrap(), 10);
    assert_eq!(operation.error.unwrap().code, "EAPPLY");
}

#[test]
fn timeout_after_reopening_recovers_before_accepting_more_changes() {
    let dir = tempfile::tempdir().unwrap(); let host = Host::new();
    let mut coordinator = Coordinator::open(dir.path(), "user-a", host.clone()).unwrap();
    let plan = coordinator.plan(request(), 100).unwrap();
    let op = coordinator.apply(&plan.id, 101).unwrap();
    assert_eq!(op.state, OperationState::Verifying);
    assert_eq!(op.confirmation_deadline, Some(221));
    assert_eq!(*host.value.lock().unwrap(), 20);
    drop(coordinator);
    let mut restored = Coordinator::open(dir.path(), "user-a", host.clone()).unwrap();
    let op = restored.recover(222).unwrap().unwrap();
    assert_eq!(op.state, OperationState::Cancelled);
    assert_eq!(*host.value.lock().unwrap(), 10);
    assert!(restored.keep(&plan.id, 223).is_err());
}

#[test]
fn replacing_adapter_or_expiring_plan_performs_no_mutation() {
    let dir = tempfile::tempdir().unwrap(); let host = Host::new();
    let mut coordinator = Coordinator::open(dir.path(), "user-a", host.clone()).unwrap();
    let plan = coordinator.plan(request(), 100).unwrap();
    *host.fingerprint.lock().unwrap() = "replacement".into();
    assert_eq!(coordinator.apply(&plan.id, 101).unwrap_err().code, "ESTALE");
    assert_eq!(*host.value.lock().unwrap(), 10);
    assert_eq!(coordinator.apply(&plan.id, 401).unwrap_err().code, "ESTALE");
}

#[test]
fn a_second_process_cannot_overlap_or_cancel_another_users_operation() {
    let dir = tempfile::tempdir().unwrap(); let host = Host::new();
    let mut first = Coordinator::open(dir.path(), "user-a", host.clone()).unwrap();
    let plan = first.plan(request(), 100).unwrap(); first.apply(&plan.id, 101).unwrap();
    let mut second = Coordinator::open(dir.path(), "user-b", host.clone()).unwrap();
    assert_eq!(second.plan(request(), 102).unwrap_err().code, "EBUSY");
    assert_eq!(second.cancel(&plan.id).unwrap_err().code, "EPERM");
    assert_eq!(second.keep(&plan.id, 102).unwrap_err().code, "EPERM");
    assert_eq!(*host.value.lock().unwrap(), 20);
    assert_eq!(first.keep(&plan.id, 102).unwrap().state, OperationState::Ready);
}

#[test]
fn failed_restoration_is_retained_for_repair_instead_of_becoming_ready() {
    let dir = tempfile::tempdir().unwrap(); let mut host = Host::new(); host.fail = true; host.rollback_fails = true;
    let mut coordinator = Coordinator::open(dir.path(), "user-a", host.clone()).unwrap();
    let plan = coordinator.plan(request(), 100).unwrap();
    let op = coordinator.apply(&plan.id, 101).unwrap();
    assert_eq!(op.error.unwrap().code, "EROLLBACK");
    assert_eq!(coordinator.plan(request(), 102).unwrap_err().code, "ERECOVERY");
    drop(coordinator);
    host.rollback_fails = false;
    let mut recovered = Coordinator::open(dir.path(), "user-a", host.clone()).unwrap();
    assert_eq!(recovered.recover(102).unwrap().unwrap().state, OperationState::Cancelled);
    assert_eq!(*host.value.lock().unwrap(), 10);
}

#[test]
fn installed_resource_survives_later_plans_and_enforces_ownership() {
    let dir = tempfile::tempdir().unwrap(); let host = Host::new();
    let mut first = Coordinator::open(dir.path(), "user-a", host.clone()).unwrap();
    let plan = first.plan(request(), 100).unwrap();
    first.apply(&plan.id, 101).unwrap(); first.keep(&plan.id, 102).unwrap();
    let resource = first.resources().unwrap().pop().unwrap();
    drop(first);
    let mut second = Coordinator::open(dir.path(), "user-b", host.clone()).unwrap();
    assert!(second.resources().unwrap().is_empty());
    assert_eq!(second.plan(PlanRequest::Remove { resource_id: resource.id.clone() }, 103).unwrap_err().code, "EPERM");
    let other = second.plan(PlanRequest::Configure { adapter_id: AdapterId::new("other").unwrap() }, 104).unwrap();
    second.cancel(&other.id).unwrap();
    let mut first = Coordinator::open(dir.path(), "user-a", host).unwrap();
    assert_eq!(first.resources().unwrap()[0].id, resource.id);
    assert_eq!(first.plan(request(), 105).unwrap_err().code, "ECONFLICT");
}

#[test]
fn active_transport_lease_prevents_removal_until_released() {
    let dir = tempfile::tempdir().unwrap(); let host = Host::new();
    let mut coordinator = Coordinator::open(dir.path(), "user-a", host.clone()).unwrap();
    let plan = coordinator.plan(request(), 100).unwrap();
    coordinator.apply(&plan.id, 101).unwrap(); coordinator.keep(&plan.id, 102).unwrap();
    let resource = coordinator.resources().unwrap().pop().unwrap();
    let removal = coordinator.plan(PlanRequest::Remove { resource_id: resource.id.clone() }, 103).unwrap();
    let lease = coordinator.lease(&resource.id).unwrap();
    assert_eq!(coordinator.apply(&removal.id, 104).unwrap_err().code, "EBUSY");
    drop(lease);
    coordinator.apply(&removal.id, 105).unwrap();
    assert!(coordinator.lease(&resource.id).is_err());
    coordinator.keep(&removal.id, 106).unwrap();
    assert!(coordinator.resources().unwrap().is_empty());
    assert_eq!(*host.value.lock().unwrap(), 10);
}

#[test]
fn repair_preserves_original_installation_state_for_removal() {
    let dir = tempfile::tempdir().unwrap(); let host = Host::new();
    let mut coordinator = Coordinator::open(dir.path(), "user-a", host.clone()).unwrap();
    let plan = coordinator.plan(request(), 100).unwrap();
    coordinator.apply(&plan.id, 101).unwrap(); coordinator.keep(&plan.id, 102).unwrap();
    let id = coordinator.resources().unwrap()[0].id.clone();
    let repair = coordinator.plan(PlanRequest::Repair { resource_id: id.clone() }, 103).unwrap();
    coordinator.apply(&repair.id, 104).unwrap(); coordinator.keep(&repair.id, 105).unwrap();
    let removal = coordinator.plan(PlanRequest::Remove { resource_id: id }, 106).unwrap();
    coordinator.apply(&removal.id, 107).unwrap();
    coordinator.cancel(&removal.id).unwrap();
    assert_eq!(*host.value.lock().unwrap(), 20);
    assert_eq!(coordinator.resources().unwrap()[0].original.value, 10);
}

#[test]
fn every_mutation_has_durable_intent_and_crash_recovery() {
    struct CrashingHost { host: Host, journal: std::path::PathBuf, crash_at: usize, step: usize }
    impl PlatformOps for CrashingHost {
        type Snapshot = Snapshot;
        type Mutation = Mutation;
        fn prepare(&mut self, request: PlanRequest, id: OperationId, now: u64, existing: Option<&eink_bridge::coordinator::Resource<Snapshot>>) -> Result<Prepared<Snapshot, Mutation>, BridgeError> {
            let mut prepared = self.host.prepare(request, id, now, existing)?;
            prepared.mutations = vec![Mutation::Change(20), Mutation::Change(30), Mutation::Change(40)];
            Ok(prepared)
        }
        fn fingerprint(&mut self, request: &PlanRequest) -> Result<String, BridgeError> { self.host.fingerprint(request) }
        fn apply(&mut self, mutation: &Mutation) -> Result<(), BridgeError> {
            let durable: serde_json::Value = serde_json::from_slice(&std::fs::read(&self.journal).unwrap()).unwrap();
            assert_eq!(durable["completed_mutations"], self.step);
            assert_eq!(durable["in_flight_mutation"], self.step);
            assert_eq!(durable["undo_required"], true);
            self.host.apply(mutation)?;
            if self.step == self.crash_at { panic!("simulated crash after external effect"); }
            self.step += 1;
            Ok(())
        }
        fn verify(&mut self, snapshot: &Snapshot) -> Result<(), BridgeError> { self.host.verify(snapshot) }
        fn rollback(&mut self, snapshot: &Snapshot) -> Result<(), BridgeError> { self.host.rollback(snapshot) }
    }
    for crash_at in 0..3 {
        let dir = tempfile::tempdir().unwrap();
        let host = Host::new();
        let journal = dir.path().join("operation.json");
        let mut coordinator = Coordinator::open(dir.path(), "user-a", CrashingHost {
            host: host.clone(), journal: journal.clone(), crash_at, step: 0,
        }).unwrap();
        let plan = coordinator.plan(request(), 100).unwrap();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| coordinator.apply(&plan.id, 101)));
        assert!(result.is_err());
        // Prove the intended failure occurred after the mutation, not at an
        // unrelated assertion before the external effect.
        assert_eq!(*host.value.lock().unwrap(), 20 + crash_at as i32 * 10);
        drop(coordinator);
        let mut restarted = Coordinator::open(dir.path(), "user-a", host.clone()).unwrap();
        assert_eq!(restarted.recover(102).unwrap().unwrap().state, OperationState::Cancelled);
        assert_eq!(*host.value.lock().unwrap(), 10);
    }
}
