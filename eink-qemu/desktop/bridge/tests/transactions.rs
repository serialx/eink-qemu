use eink_bridge::{coordinator::{Coordinator, PlatformOps, Prepared}, model::*};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

#[derive(Clone, Serialize, Deserialize)]
struct Snapshot { value: i32 }
#[derive(Clone, Serialize, Deserialize)]
enum Mutation { Change(i32), Fail }
#[derive(Clone)]
struct Host { idle_transport: bool, confirmation_events: Arc<Mutex<Vec<&'static str>>>, confirmation_failure: Option<&'static str>, pause_in_verify: bool, pause_in_commit: bool, may_disconnect: bool, value: Arc<Mutex<i32>>, fingerprint: Arc<Mutex<String>>, fail: bool, rollback_fails: bool, commits: Arc<Mutex<usize>>, commit_fails: bool, crash_commit: bool, resumes: Arc<Mutex<usize>>, apply_delay: u64, apply_started: Option<std::sync::mpsc::Sender<()>>, apply_resume: Option<Arc<Mutex<std::sync::mpsc::Receiver<()>>>> }
impl Host { fn new() -> Self { Self { idle_transport: false, confirmation_events: Arc::new(Mutex::new(Vec::new())), confirmation_failure: None, pause_in_verify: false, pause_in_commit: false, may_disconnect: true, value: Arc::new(Mutex::new(10)), fingerprint: Arc::new(Mutex::new("original".into())), fail: false, rollback_fails: false, commits: Arc::new(Mutex::new(0)), commit_fails: false, crash_commit: false, resumes: Arc::new(Mutex::new(0)), apply_delay: 0, apply_started: None, apply_resume: None } } }
impl Host {
    fn confirmation(&self, stage: &'static str) -> Result<(), BridgeError> {
        self.confirmation_events.lock().unwrap().push(stage);
        if self.confirmation_failure == Some(stage) { Err(BridgeError::new("ESTALE", "Native checkpoint expired")) } else { Ok(()) }
    }
    fn pause(&self) {
        if let Some(started) = &self.apply_started { started.send(()).unwrap(); }
        if let Some(resume) = &self.apply_resume { resume.lock().unwrap().recv_timeout(std::time::Duration::from_secs(5)).unwrap(); }
    }
}
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
        Ok(Prepared { plan: BridgePlan { id, request, fingerprint: self.fingerprint.lock().unwrap().clone(), changes: vec!["Change connection".into()], requires_authorization: true, may_disconnect: self.may_disconnect, expires_at: now + 300 }, snapshot, mutations })
    }
    fn fingerprint(&mut self, _: &PlanRequest, _: &Self::Snapshot) -> Result<String, BridgeError> { Ok(self.fingerprint.lock().unwrap().clone()) }
    fn apply(&mut self, mutation: &Mutation) -> Result<(), BridgeError> {
        if !self.pause_in_verify && !self.pause_in_commit { self.pause(); }
        std::thread::sleep(std::time::Duration::from_secs(self.apply_delay));
        match mutation { Mutation::Change(value) => { *self.value.lock().unwrap() = *value; Ok(()) }, Mutation::Fail => Err(BridgeError::new("EAPPLY", "Failed mutation")) }
    }
    fn verify(&mut self, _: &Snapshot) -> Result<(), BridgeError> {
        self.confirmation_events.lock().unwrap().push("verify");
        if self.pause_in_verify { self.pause(); } Ok(())
    }
    fn requires_idle_transport(&self, _: &Snapshot) -> bool { self.idle_transport }
    fn begin_confirmation(&mut self, _: &Snapshot) -> Result<(), BridgeError> { self.confirmation("confirm") }
    fn begin_keep(&mut self, _: &Snapshot) -> Result<(), BridgeError> { self.confirmation("keep") }
    fn commit(&mut self, _: &Snapshot) -> Result<(), BridgeError> {
        *self.commits.lock().unwrap() += 1;
        if self.pause_in_commit { self.pause(); }
        assert!(!self.crash_commit, "crash after native checkpoint commit");
        if self.commit_fails { Err(BridgeError::new("ECOMMIT", "Native checkpoint could not be committed")) } else { Ok(()) }
    }
    fn resume(&mut self, _: &Snapshot) -> Result<(), BridgeError> {
        *self.resumes.lock().unwrap() += 1;
        if self.fail { return Err(BridgeError::new("ECONFLICT", "Changed native configuration")); }
        *self.value.lock().unwrap() = 20; Ok(())
    }
    fn rollback(&mut self, snapshot: &Snapshot) -> Result<(), BridgeError> {
        if self.rollback_fails { return Err(BridgeError::new("EIO", "Host restoration failed")); }
        *self.value.lock().unwrap() = snapshot.value; Ok(())
    }
}
fn request() -> PlanRequest { PlanRequest::Configure { adapter_id: AdapterId::new("adapter").unwrap() } }

#[test]
fn native_approval_uses_only_the_current_owners_unexpired_review() {
    let root = tempfile::tempdir().unwrap(); let host = Host::new();
    let mut owner = Coordinator::open(root.path(), "owner", host.clone()).unwrap();
    let first = owner.plan(request(), 100).unwrap();
    assert_eq!(owner.reviewed_plan(&first.id, 101).unwrap(), (first.clone(), 1));
    let other = Coordinator::open(root.path(), "other", host).unwrap();
    assert_eq!(other.reviewed_plan(&first.id, 101).unwrap_err().code, "EPERM");
    assert_eq!(owner.reviewed_plan(&first.id, 400).unwrap_err().code, "ESTALE");
    let next = owner.plan(request(), 401).unwrap();
    assert_eq!(owner.reviewed_plan(&first.id, 402).unwrap_err().code, "ESTALE");
    owner.apply(&next.id, 402).unwrap();
    assert_eq!(owner.reviewed_plan(&next.id, 403).unwrap_err().code, "ESTALE");
    owner.cancel(&next.id).unwrap();
    assert_eq!(owner.reviewed_plan(&next.id, 404).unwrap_err().code, "ESTALE");
}

#[test]
fn polling_is_read_only_and_hides_another_users_operation() {
    let dir = tempfile::tempdir().unwrap(); let host = Host::new();
    let mut owner = Coordinator::open(dir.path(), "user-a", host.clone()).unwrap();
    let plan = owner.plan(request(), 100).unwrap();
    let operation = owner.apply(&plan.id, 101).unwrap();
    assert_eq!(owner.operation().unwrap(), Some(operation.clone()));
    let other = Coordinator::open(dir.path(), "user-b", host.clone()).unwrap();
    assert_eq!(other.operation().unwrap(), None);
    assert_eq!(*host.value.lock().unwrap(), 20, "polling must not roll back a pending operation");
    assert_eq!(owner.operation().unwrap(), Some(operation));
}

#[test]
fn progress_remains_readable_while_native_mutation_holds_the_lock() {
    let root = tempfile::tempdir().unwrap(); let mut host = Host::new();
    let (started, entered) = std::sync::mpsc::channel();
    let (release, resume) = std::sync::mpsc::channel();
    host.apply_started = Some(started); host.apply_resume = Some(Arc::new(Mutex::new(resume)));
    let mut owner = Coordinator::open(root.path(), "owner", host.clone()).unwrap();
    let plan = owner.plan(request(), 100).unwrap(); let id = plan.id.clone();
    let mut observer = Coordinator::open(root.path(), "owner", host.clone()).unwrap();
    let stranger = Coordinator::open(root.path(), "other", host).unwrap();
    let worker = std::thread::spawn(move || owner.apply(&id, 101));
    entered.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
    let progress = observer.operation();
    let hidden = stranger.operation();
    let resources = observer.resources();
    let hidden_resources = stranger.resources();
    let competing = observer.apply(&plan.id, 101);
    release.send(()).unwrap();
    let completed = worker.join().unwrap().unwrap();
    let progress = progress.unwrap().unwrap();
    assert_eq!(progress.state, OperationState::Applying);
    assert_eq!(progress.id, plan.id);
    assert_eq!(hidden.unwrap(), None);
    assert!(resources.unwrap().is_empty());
    assert!(hidden_resources.unwrap().is_empty());
    assert_eq!(competing.unwrap_err().code, "EBUSY");
    assert!(completed.revision > progress.revision);
    assert_eq!(completed.state, OperationState::Verifying);
}

#[test]
fn cancellation_during_native_apply_restores_before_automatic_commit() {
    for disruptive in [false, true] {
        let root = tempfile::tempdir().unwrap(); let mut host = Host::new();
        host.may_disconnect = disruptive;
        let (started, entered) = std::sync::mpsc::channel();
        let (release, resume) = std::sync::mpsc::channel();
        host.apply_started = Some(started); host.apply_resume = Some(Arc::new(Mutex::new(resume)));
        let mut owner = Coordinator::open(root.path(), "owner", host.clone()).unwrap();
        let plan = owner.plan(request(), 100).unwrap(); let id = plan.id.clone();
        let mut cancellation = Coordinator::open(root.path(), "owner", host.clone()).unwrap();
        let worker = std::thread::spawn(move || owner.apply(&id, 101));
        entered.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        let durable: serde_json::Value = serde_json::from_slice(&std::fs::read(root.path().join("operation.json")).unwrap()).unwrap();
        assert_eq!(durable["version"], 4, "older helpers cannot ignore a queued cancellation");
        let mut stranger = Coordinator::open(root.path(), "other", host.clone()).unwrap();
        assert_eq!(stranger.cancel(&plan.id).unwrap_err().code, "EPERM");
        assert!(!root.path().join("cancellation.json").exists());
        let queued = cancellation.cancel(&plan.id);
        // The client may disappear after queuing cancellation. No second
        // Cancel request is needed to make the helper restore the snapshot.
        drop(cancellation);
        release.send(()).unwrap();
        let completed = worker.join().unwrap().unwrap();
        assert_eq!(queued.unwrap_err().code, "EBUSY");
        assert_eq!(completed.state, OperationState::Cancelled);
        assert_eq!(*host.value.lock().unwrap(), 10);
        assert_eq!(*host.commits.lock().unwrap(), 0);
        let mut reopened = Coordinator::open(root.path(), "owner", Host::new()).unwrap();
        assert!(reopened.resources().unwrap().is_empty());
        let next = reopened.plan(request(), 200).unwrap();
        assert_eq!(reopened.apply(&next.id, 201).unwrap().state, OperationState::Verifying, "old cancellation cannot cancel a new plan");
    }
}

#[test]
fn cancellation_is_accepted_during_verification_but_not_after_finalization_starts() {
    for finalizing in [false, true] {
        let root = tempfile::tempdir().unwrap(); let mut host = Host::new();
        host.may_disconnect = false;
        host.pause_in_verify = !finalizing; host.pause_in_commit = finalizing;
        let (started, entered) = std::sync::mpsc::channel();
        let (release, resume) = std::sync::mpsc::channel();
        host.apply_started = Some(started); host.apply_resume = Some(Arc::new(Mutex::new(resume)));
        let mut owner = Coordinator::open(root.path(), "owner", host.clone()).unwrap();
        let plan = owner.plan(request(), 100).unwrap(); let id = plan.id.clone();
        let mut cancellation = Coordinator::open(root.path(), "owner", host.clone()).unwrap();
        let worker = std::thread::spawn(move || owner.apply(&id, 101));
        entered.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        let progress = cancellation.operation().unwrap().unwrap();
        let result = cancellation.cancel(&plan.id);
        release.send(()).unwrap();
        let completed = worker.join().unwrap().unwrap();
        if finalizing {
            assert_eq!(progress.state, OperationState::Finalizing);
            assert_eq!(result.unwrap_err().code, "ESTALE");
            assert_eq!(completed.state, OperationState::Ready);
            assert!(!root.path().join("cancellation.json").exists());
        } else {
            assert_eq!(result.unwrap_err().code, "EBUSY");
            assert_eq!(completed.state, OperationState::Cancelled);
            assert_eq!(*host.value.lock().unwrap(), 10);
            assert_eq!(*host.commits.lock().unwrap(), 0);
        }
    }
}

#[test]
fn verified_resource_holds_a_lease_and_rejects_other_users() {
    let dir = tempfile::tempdir().unwrap(); let host = Host::new();
    let mut owner = Coordinator::open(dir.path(), "user-a", host.clone()).unwrap();
    let plan = owner.plan(request(), 100).unwrap();
    owner.apply(&plan.id, 101).unwrap(); owner.keep(&plan.id, 102).unwrap();
    let id = ResourceId::new(plan.id.as_str()).unwrap();
    let (resource, lease) = owner.verify_resource(&id).unwrap();
    assert_eq!(resource.id, id);
    let mut other = Coordinator::open(dir.path(), "user-b", host.clone()).unwrap();
    assert!(matches!(other.verify_resource(&id), Err(error) if error.code == "EPERM"));
    assert_eq!(owner.plan(PlanRequest::Remove { resource_id: id.clone() }, 103).unwrap_err().code, "EBUSY");
    drop(lease);
    owner.plan(PlanRequest::Remove { resource_id: id }, 104).unwrap();
}

#[test]
fn transport_that_requires_idle_verification_reports_in_use_without_claiming_damage_or_readiness() {
    let root = tempfile::tempdir().unwrap(); let mut host = Host::new(); host.idle_transport = true;
    let mut owner = Coordinator::open(root.path(), "owner", host.clone()).unwrap();
    let plan = owner.plan(request(), 100).unwrap();
    owner.apply(&plan.id, 101).unwrap(); owner.keep(&plan.id, 102).unwrap();
    let id = ResourceId::new(plan.id.as_str()).unwrap();
    let (_, lease) = owner.verify_resource(&id).unwrap();
    host.confirmation_events.lock().unwrap().clear();
    let mut observer = Coordinator::open(root.path(), "owner", host.clone()).unwrap();
    assert!(matches!(observer.verify_resource(&id), Err(error) if error.code == "EINUSE"));
    assert!(host.confirmation_events.lock().unwrap().is_empty(), "do not inspect temporary topology while a transport is live");
    let mut stranger = Coordinator::open(root.path(), "other", host.clone()).unwrap();
    assert!(matches!(stranger.verify_resource(&id), Err(error) if error.code == "EPERM"));
    assert_eq!(owner.plan(PlanRequest::Remove { resource_id: id.clone() }, 103).unwrap_err().code, "EBUSY");
    drop(lease);
    assert!(observer.verify_resource(&id).is_ok());
    assert_eq!(*host.confirmation_events.lock().unwrap(), ["verify"], "strict native checks resume when idle");
}

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
fn helper_startup_restores_unconfirmed_changes_even_before_the_deadline() {
    let dir = tempfile::tempdir().unwrap(); let host = Host::new();
    let mut coordinator = Coordinator::open(dir.path(), "user-a", host.clone()).unwrap();
    let plan = coordinator.plan(request(), 100).unwrap();
    coordinator.apply(&plan.id, 101).unwrap();
    // A live helper's timer leaves the user time to confirm.
    assert_eq!(coordinator.recover(102).unwrap().unwrap().state, OperationState::Verifying);
    drop(coordinator);
    // Restart/reboot invalidates that confirmation window. Startup recovery
    // must not depend on the previous wall clock or the original caller.
    let mut restarted = Coordinator::open(dir.path(), "service", host.clone()).unwrap();
    let restored = restarted.recover_startup().unwrap().unwrap();
    assert_eq!(restored.state, OperationState::Cancelled);
    assert_eq!(*host.value.lock().unwrap(), 10);
    assert_eq!(restarted.recover_startup().unwrap().unwrap().revision, restored.revision);
    let mut original = Coordinator::open(dir.path(), "user-a", host.clone()).unwrap();
    assert_eq!(original.keep(&plan.id, 103).unwrap_err().code, "ESTALE");
    let next = original.plan(request(), 104).unwrap();
    original.apply(&next.id, 105).unwrap();
    original.keep(&next.id, 106).unwrap();
    drop(original);
    assert_eq!(restarted.recover_startup().unwrap().unwrap().state, OperationState::Ready);
    assert_eq!(*host.value.lock().unwrap(), 20);
}

#[test]
fn queued_cancellation_survives_client_exit_and_is_consumed_before_the_keep_deadline() {
    for restart in [false, true] {
        let root = tempfile::tempdir().unwrap(); let host = Host::new();
        let mut owner = Coordinator::open(root.path(), "owner", host.clone()).unwrap();
        let plan = owner.plan(request(), 100).unwrap();
        owner.apply(&plan.id, 101).unwrap();
        let lock = std::fs::OpenOptions::new().read(true).write(true).open(root.path().join("operation.lock")).unwrap();
        lock.lock().unwrap();
        assert_eq!(owner.cancel(&plan.id).unwrap_err().code, "EBUSY");
        drop(owner); drop(lock);
        let mut service = Coordinator::open(root.path(), "service", host.clone()).unwrap();
        let operation = if restart { service.recover_startup() } else { service.recover(102) }.unwrap().unwrap();
        assert_eq!(operation.state, OperationState::Cancelled);
        assert_eq!(*host.value.lock().unwrap(), 10);
        assert_eq!(*host.commits.lock().unwrap(), 0);
    }
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
    assert_eq!(recovered.recover(102).unwrap().unwrap().state, OperationState::Failed);
    assert_eq!(*host.value.lock().unwrap(), 20, "a recorded restoration conflict requires an explicit retry");
    assert_eq!(recovered.cancel(&plan.id).unwrap().state, OperationState::Cancelled);
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
        fn fingerprint(&mut self, request: &PlanRequest, snapshot: &Self::Snapshot) -> Result<String, BridgeError> { self.host.fingerprint(request, snapshot) }
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
        fn commit(&mut self, snapshot: &Snapshot) -> Result<(), BridgeError> { self.host.commit(snapshot) }
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

#[test]
fn keep_commits_the_native_checkpoint_once_and_commit_failure_restores() {
    for fail in [false, true] {
        let dir = tempfile::tempdir().unwrap(); let mut host = Host::new(); host.commit_fails = fail;
        let mut coordinator = Coordinator::open(dir.path(), "user-a", host.clone()).unwrap();
        let plan = coordinator.plan(request(), 100).unwrap();
        coordinator.apply(&plan.id, 101).unwrap();
        assert_eq!(*host.commits.lock().unwrap(), 0, "Apply must leave native rollback armed");
        let operation = coordinator.keep(&plan.id, 102).unwrap();
        assert_eq!(*host.commits.lock().unwrap(), 1);
        if fail {
            assert_eq!(operation.state, OperationState::Failed);
            assert_eq!(operation.error.unwrap().code, "ECOMMIT");
            assert_eq!(*host.value.lock().unwrap(), 10);
            assert!(coordinator.resources().unwrap().is_empty());
        } else {
            assert_eq!(operation.state, OperationState::Ready);
            assert_eq!(*host.value.lock().unwrap(), 20);
            assert_eq!(coordinator.resources().unwrap().len(), 1);
        }
        assert!(coordinator.keep(&plan.id, 103).is_err());
        assert_eq!(*host.commits.lock().unwrap(), 1);
    }
}

#[test]
fn crash_after_native_commit_does_not_discard_the_recovery_snapshot() {
    let dir = tempfile::tempdir().unwrap(); let mut host = Host::new(); host.crash_commit = true;
    let mut coordinator = Coordinator::open(dir.path(), "user-a", host.clone()).unwrap();
    let plan = coordinator.plan(request(), 100).unwrap();
    coordinator.apply(&plan.id, 101).unwrap();
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| coordinator.keep(&plan.id, 102))).is_err());
    assert_eq!(*host.commits.lock().unwrap(), 1);
    assert_eq!(*host.value.lock().unwrap(), 20);
    drop(coordinator);
    let mut restarted = Coordinator::open(dir.path(), "service", host.clone()).unwrap();
    assert_eq!(restarted.recover_startup().unwrap().unwrap().state, OperationState::Cancelled);
    assert_eq!(*host.value.lock().unwrap(), 10);
    assert!(restarted.resources().unwrap().is_empty());
}

#[cfg(target_os = "macos")]
#[test]
fn extended_acl_cannot_make_the_recovery_journal_writable() {
    let dir = tempfile::tempdir().unwrap();
    let status = std::process::Command::new("/bin/chmod").args(["+a", "group:staff allow add_file"]).arg(dir.path()).status().unwrap();
    assert!(status.success());
    assert!(Coordinator::open(dir.path(), "user-a", Host::new()).is_err());
}

#[test]
fn startup_resumes_committed_resources_but_skips_active_leases() {
    let dir = tempfile::tempdir().unwrap(); let host = Host::new();
    let mut owner = Coordinator::open(dir.path(), "user-a", host.clone()).unwrap();
    let plan = owner.plan(request(), 100).unwrap();
    owner.apply(&plan.id, 101).unwrap();
    assert_eq!(owner.resume_resources().unwrap_err().code, "ERECOVERY");
    owner.keep(&plan.id, 102).unwrap();
    let id = ResourceId::new(plan.id.as_str()).unwrap();
    let lease = owner.lease(&id).unwrap();
    let mut service = Coordinator::open(dir.path(), "service", host.clone()).unwrap();
    assert!(service.resume_resources().unwrap().is_empty());
    assert_eq!(*host.resumes.lock().unwrap(), 0);
    drop(lease);
    *host.value.lock().unwrap() = 0;
    assert!(service.resume_resources().unwrap().is_empty());
    assert_eq!(*host.value.lock().unwrap(), 20);
    assert_eq!(*host.resumes.lock().unwrap(), 1);
    let mut changed = host.clone(); changed.fail = true;
    let mut service = Coordinator::open(dir.path(), "service", changed).unwrap();
    let failures = service.resume_resources().unwrap();
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0].0, id);
    assert_eq!(failures[0].1.code, "ECONFLICT");
    assert_eq!(owner.resources().unwrap().len(), 1, "resume failures preserve ownership for explicit repair");
}

#[test]
fn confirmation_time_starts_after_slow_native_setup() {
    let dir = tempfile::tempdir().unwrap(); let mut host = Host::new(); host.apply_delay = 1;
    let mut owner = Coordinator::open(dir.path(), "user-a", host).unwrap();
    let plan = owner.plan(request(), 100).unwrap();
    let operation = owner.apply(&plan.id, 101).unwrap();
    assert!(operation.confirmation_deadline.unwrap() >= 222, "native setup must not consume the confirmation window");
}

#[test]
fn native_confirmation_window_follows_verification_and_keep_rearms_before_verifying() {
    let dir = tempfile::tempdir().unwrap(); let host = Host::new();
    let mut owner = Coordinator::open(dir.path(), "owner", host.clone()).unwrap();
    let plan = owner.plan(request(), 100).unwrap();
    owner.apply(&plan.id, 101).unwrap();
    assert_eq!(*host.confirmation_events.lock().unwrap(), ["verify", "confirm"]);
    owner.keep(&plan.id, 220).unwrap();
    assert_eq!(*host.confirmation_events.lock().unwrap(), ["verify", "confirm", "keep", "verify"]);
}

#[test]
fn expired_native_confirmation_rolls_back_and_late_keep_cannot_extend_it() {
    for failure in [Some("confirm"), Some("keep"), None] {
        let dir = tempfile::tempdir().unwrap(); let mut host = Host::new();
        host.confirmation_failure = failure;
        let mut owner = Coordinator::open(dir.path(), "owner", host.clone()).unwrap();
        let plan = owner.plan(request(), 100).unwrap();
        let applied = owner.apply(&plan.id, 101).unwrap();
        let operation = if failure == Some("confirm") { applied }
            else { owner.keep(&plan.id, if failure.is_some() { 220 } else { applied.confirmation_deadline.unwrap() }).unwrap() };
        assert_eq!(operation.state, if failure.is_some() { OperationState::Failed } else { OperationState::Cancelled });
        assert_eq!(*host.value.lock().unwrap(), 10);
        assert_eq!(*host.commits.lock().unwrap(), 0);
        if failure.is_none() { assert!(!host.confirmation_events.lock().unwrap().contains(&"keep")); }
    }
}

#[test]
fn native_checkpoint_identity_is_durable_before_network_mutation() {
    #[derive(Clone, Serialize, Deserialize)]
    struct CheckpointSnapshot { checkpoint: Option<String> }
    #[derive(Clone)]
    struct CheckpointHost { journal: std::path::PathBuf, restored: Arc<Mutex<bool>> }
    impl PlatformOps for CheckpointHost {
        type Snapshot = CheckpointSnapshot;
        type Mutation = u8;
        fn prepare(&mut self, request: PlanRequest, id: OperationId, now: u64, _: Option<&eink_bridge::coordinator::Resource<Self::Snapshot>>) -> Result<Prepared<Self::Snapshot, u8>, BridgeError> {
            Ok(Prepared { plan: BridgePlan { id, request, fingerprint: "nic".into(), changes: vec![], requires_authorization: true, may_disconnect: true, expires_at: now + 300 }, snapshot: CheckpointSnapshot { checkpoint: None }, mutations: vec![0, 1] })
        }
        fn fingerprint(&mut self, _: &PlanRequest, _: &Self::Snapshot) -> Result<String, BridgeError> { Ok("nic".into()) }
        fn apply(&mut self, _: &u8) -> Result<(), BridgeError> { panic!("native mutation must receive its durable context") }
        fn apply_recorded(&mut self, step: &u8, snapshot: &mut Self::Snapshot) -> Result<(), BridgeError> {
            if *step == 0 { snapshot.checkpoint = Some("/native/checkpoint/7".into()); return Ok(()); }
            let record: serde_json::Value = serde_json::from_slice(&std::fs::read(&self.journal).unwrap()).unwrap();
            assert_eq!(record["prepared"]["snapshot"]["checkpoint"], "/native/checkpoint/7");
            panic!("process crash at first network mutation");
        }
        fn verify(&mut self, _: &Self::Snapshot) -> Result<(), BridgeError> { Ok(()) }
        fn commit(&mut self, _: &Self::Snapshot) -> Result<(), BridgeError> { Ok(()) }
        fn rollback(&mut self, snapshot: &Self::Snapshot) -> Result<(), BridgeError> {
            assert_eq!(snapshot.checkpoint.as_deref(), Some("/native/checkpoint/7"));
            *self.restored.lock().unwrap() = true; Ok(())
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let host = CheckpointHost { journal: dir.path().join("operation.json"), restored: Arc::new(Mutex::new(false)) };
    let mut owner = Coordinator::open(dir.path(), "user", host.clone()).unwrap();
    let plan = owner.plan(request(), 100).unwrap();
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| owner.apply(&plan.id, 101))).is_err());
    drop(owner);
    let mut service = Coordinator::open(dir.path(), "service", host.clone()).unwrap();
    assert_eq!(service.recover_startup().unwrap().unwrap().state, OperationState::Cancelled);
    assert!(*host.restored.lock().unwrap());
}

#[test]
fn committed_native_context_is_saved_with_the_owned_resource() {
    #[derive(Clone, Serialize, Deserialize)]
    struct CommitSnapshot { committed: bool }
    struct CommitHost;
    impl PlatformOps for CommitHost {
        type Snapshot = CommitSnapshot;
        type Mutation = u8;
        fn prepare(&mut self, request: PlanRequest, id: OperationId, now: u64, _: Option<&eink_bridge::coordinator::Resource<Self::Snapshot>>) -> Result<Prepared<Self::Snapshot, u8>, BridgeError> {
            Ok(Prepared { plan: BridgePlan { id, request, fingerprint: "nic".into(), changes: vec![], requires_authorization: true, may_disconnect: true, expires_at: now + 300 }, snapshot: CommitSnapshot { committed: false }, mutations: vec![0] })
        }
        fn fingerprint(&mut self, _: &PlanRequest, _: &Self::Snapshot) -> Result<String, BridgeError> { Ok("nic".into()) }
        fn apply(&mut self, _: &u8) -> Result<(), BridgeError> { Ok(()) }
        fn verify(&mut self, _: &Self::Snapshot) -> Result<(), BridgeError> { Ok(()) }
        fn commit(&mut self, _: &Self::Snapshot) -> Result<(), BridgeError> { Ok(()) }
        fn commit_recorded(&mut self, snapshot: &mut Self::Snapshot) -> Result<(), BridgeError> {
            self.commit(snapshot)?;
            snapshot.committed = true;
            Ok(())
        }
        fn rollback(&mut self, _: &Self::Snapshot) -> Result<(), BridgeError> { Ok(()) }
    }
    let dir = tempfile::tempdir().unwrap();
    let mut owner = Coordinator::open(dir.path(), "user", CommitHost).unwrap();
    let plan = owner.plan(request(), 100).unwrap();
    owner.apply(&plan.id, 101).unwrap();
    assert!(owner.resources().unwrap().is_empty());
    owner.keep(&plan.id, 102).unwrap();
    drop(owner);
    let owner = Coordinator::open(dir.path(), "user", CommitHost).unwrap();
    assert!(owner.resources().unwrap()[0].original.committed, "a committed bridge must not accept temporary staging profiles after restart");
}

#[test]
fn package_uninstall_removes_each_protected_owner_and_is_repeatable() {
    let root=tempfile::tempdir().unwrap();let a=Host::new();let b=Host::new();
    for (owner,host) in [("a",a.clone()),("b",b.clone())] {
        let mut c=Coordinator::open(root.path(),owner,host).unwrap();
        let p=c.plan(PlanRequest::Configure{adapter_id:AdapterId::new(owner).unwrap()},100).unwrap();
        c.apply(&p.id,101).unwrap();c.keep(&p.id,102).unwrap();
    }
    let mut service=Coordinator::open(root.path(),"service",a.clone()).unwrap();
    let mut visited=Vec::new();
    service.uninstall(|resource|{visited.push(resource.owner.clone());Ok(if resource.owner=="a"{a.clone()}else{b.clone()})},||200).unwrap();
    assert_eq!(visited,["a","b"]);assert_eq!(*a.value.lock().unwrap(),10);assert_eq!(*b.value.lock().unwrap(),10);
    service.uninstall(|_|panic!("No retained resources"),||201).unwrap();
    for owner in ["a","b"] {assert!(Coordinator::open(root.path(),owner,a.clone()).unwrap().resources().unwrap().is_empty());}
}

#[test]
fn package_uninstall_refuses_live_emulators_before_removing_any_resource() {
    let root=tempfile::tempdir().unwrap();let host=Host::new();let mut c=Coordinator::open(root.path(),"owner",host.clone()).unwrap();
    let p=c.plan(request(),100).unwrap();c.apply(&p.id,101).unwrap();c.keep(&p.id,102).unwrap();
    let id=ResourceId::new(p.id.as_str()).unwrap();let (_,lease)=c.verify_resource(&id).unwrap();
    let mut service=Coordinator::open(root.path(),"service",host.clone()).unwrap();
    assert_eq!(service.uninstall(|_|panic!("A lease must stop removal first"),||200).unwrap_err().code,"EBUSY");
    assert_eq!(*host.value.lock().unwrap(),20);assert_eq!(c.resources().unwrap().len(),1);
    drop(lease);service.uninstall(|_|Ok(host.clone()),||200).unwrap();
    assert_eq!(*host.value.lock().unwrap(),10);
}

#[test]
fn package_uninstall_retains_failed_removal_for_recovery() {
    let root=tempfile::tempdir().unwrap();let host=Host::new();let mut c=Coordinator::open(root.path(),"owner",host.clone()).unwrap();
    let p=c.plan(request(),100).unwrap();c.apply(&p.id,101).unwrap();c.keep(&p.id,102).unwrap();
    let mut failed=host.clone();failed.fail=true;
    let mut service=Coordinator::open(root.path(),"service",host.clone()).unwrap();
    assert_eq!(service.uninstall(|_|Ok(failed.clone()),||200).unwrap_err().code,"EAPPLY");
    assert_eq!(c.resources().unwrap().len(),1);assert_eq!(*host.value.lock().unwrap(),20);
    service.uninstall(|_|Ok(host.clone()),||201).unwrap();assert_eq!(*host.value.lock().unwrap(),10);
}
