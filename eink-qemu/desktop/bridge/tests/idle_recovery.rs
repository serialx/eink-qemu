//! A background status read must not make an approved foreground request busy.
use eink_bridge::{coordinator::{Coordinator, PlatformOps, Prepared, Resource}, model::*};
use serde::{Deserialize, Deserializer, Serialize};
use std::{cell::RefCell, sync::{mpsc, Arc, atomic::{AtomicUsize, Ordering}}, time::Duration};

thread_local! {
    // Pause only the recovery thread's first snapshot read. No timing race or
    // production hook is needed to reproduce the lock held during that read.
    static PAUSE_READ: RefCell<Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>> = const { RefCell::new(None) };
}
#[derive(Clone, Serialize)]
struct Snapshot;
impl<'de> Deserialize<'de> for Snapshot {
    fn deserialize<D: Deserializer<'de>>(input: D) -> Result<Self, D::Error> {
        <()>::deserialize(input)?;
        PAUSE_READ.with(|gate| {
            if let Some((entered, resume)) = gate.borrow_mut().take() {
                entered.send(()).unwrap();
                resume.recv_timeout(Duration::from_secs(5)).unwrap();
            }
        });
        Ok(Self)
    }
}
#[derive(Clone, Default)]
struct Host { restored: Arc<AtomicUsize>, pause_for_reboot: bool }
impl PlatformOps for Host {
    type Snapshot = Snapshot;
    type Mutation = ();
    fn prepare(&mut self, request: PlanRequest, id: OperationId, now: u64, _: Option<&Resource<Snapshot>>) -> Result<Prepared<Snapshot, ()>, BridgeError> {
        Ok(Prepared { plan: BridgePlan { id, request, fingerprint: "nic".into(), changes: vec![], requires_authorization: true, may_disconnect: true, expires_at: now + 300 }, snapshot: Snapshot, mutations: vec![()] })
    }
    fn fingerprint(&mut self, _: &PlanRequest, _: &Snapshot) -> Result<String, BridgeError> { Ok("nic".into()) }
    fn apply(&mut self, _: &()) -> Result<(), BridgeError> { Ok(()) }
    fn awaiting_reboot(&self, _: &Snapshot) -> bool { self.pause_for_reboot }
    fn verify(&mut self, _: &Snapshot) -> Result<(), BridgeError> { Ok(()) }
    fn commit(&mut self, _: &Snapshot) -> Result<(), BridgeError> { Ok(()) }
    fn rollback(&mut self, _: &Snapshot) -> Result<(), BridgeError> { self.restored.fetch_add(1, Ordering::SeqCst); Ok(()) }
}
fn paused_read(mut recovery: Coordinator<Host>, now: u64) -> (mpsc::Sender<()>, std::thread::JoinHandle<Result<Option<Operation>, BridgeError>>) {
    let (entered, waiting) = mpsc::channel();
    let (resume, resumed) = mpsc::channel();
    let thread = std::thread::spawn(move || {
        PAUSE_READ.with(|gate| *gate.borrow_mut() = Some((entered, resumed)));
        recovery.recover(now)
    });
    waiting.recv_timeout(Duration::from_secs(5)).unwrap();
    (resume, thread)
}
#[test]
fn idle_recovery_does_not_refuse_approval_and_observes_new_work_on_the_next_tick() {
    let dir = tempfile::tempdir().unwrap();
    let host = Host::default();
    let mut client = Coordinator::open(dir.path(), "user", host.clone()).unwrap();
    let plan = client.plan(PlanRequest::Configure { adapter_id: AdapterId::new("nic").unwrap() }, 100).unwrap();
    let (resume, thread) = paused_read(Coordinator::open(dir.path(), "service", host.clone()).unwrap(), 101);
    let review = client.reviewed_plan(&plan.id, 101);
    let apply = review.as_ref().ok().map(|_| client.apply(&plan.id, 101));
    resume.send(()).unwrap();
    thread.join().unwrap().unwrap();
    review.expect("an idle watchdog read must not reject administrator approval");
    assert_eq!(apply.unwrap().unwrap().state, OperationState::Verifying);
    assert_eq!(client.operation().unwrap().unwrap().state, OperationState::Verifying);
    let mut recovery = Coordinator::open(dir.path(), "service", host.clone()).unwrap();
    assert_eq!(recovery.recover(222).unwrap().unwrap().state, OperationState::Cancelled);
    assert_eq!(host.restored.load(Ordering::SeqCst), 1);
}
#[test]
fn waiting_recovery_does_not_refuse_keep_or_undo_a_concurrent_confirmation() {
    let dir = tempfile::tempdir().unwrap();
    let host = Host::default();
    let mut client = Coordinator::open(dir.path(), "user", host.clone()).unwrap();
    let plan = client.plan(PlanRequest::Configure { adapter_id: AdapterId::new("nic").unwrap() }, 100).unwrap();
    client.apply(&plan.id, 101).unwrap();
    let (resume, thread) = paused_read(Coordinator::open(dir.path(), "service", host.clone()).unwrap(), 102);
    let kept = client.keep(&plan.id, 102);
    resume.send(()).unwrap();
    thread.join().unwrap().unwrap();
    assert_eq!(kept.expect("a waiting watchdog read must not reject Keep").state, OperationState::Ready);
    let mut recovery = Coordinator::open(dir.path(), "service", host.clone()).unwrap();
    assert_eq!(recovery.recover(222).unwrap().unwrap().state, OperationState::Ready);
    assert_eq!(client.resources().unwrap().len(), 1);
    assert_eq!(host.restored.load(Ordering::SeqCst), 0);
}
#[test]
fn expired_recovery_rereads_under_the_lock_before_restoring() {
    let dir = tempfile::tempdir().unwrap();
    let host = Host::default();
    let mut client = Coordinator::open(dir.path(), "user", host.clone()).unwrap();
    let plan = client.plan(PlanRequest::Configure { adapter_id: AdapterId::new("nic").unwrap() }, 100).unwrap();
    client.apply(&plan.id, 101).unwrap();
    let (resume, thread) = paused_read(Coordinator::open(dir.path(), "service", host.clone()).unwrap(), 222);
    // Model a Keep that began before the deadline, but finishes after the
    // watchdog read its old record. Restoration must revalidate that record.
    let kept = client.keep(&plan.id, 102);
    resume.send(()).unwrap();
    let recovered = thread.join().unwrap().unwrap().unwrap();
    assert_eq!(kept.unwrap().state, OperationState::Ready);
    assert_eq!(recovered.state, OperationState::Ready);
    assert_eq!(host.restored.load(Ordering::SeqCst), 0);
}
#[test]
fn cancellation_during_a_waiting_read_is_not_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let host = Host::default();
    let mut client = Coordinator::open(dir.path(), "user", host.clone()).unwrap();
    let plan = client.plan(PlanRequest::Configure { adapter_id: AdapterId::new("nic").unwrap() }, 100).unwrap();
    client.apply(&plan.id, 101).unwrap();
    let (resume, thread) = paused_read(Coordinator::open(dir.path(), "service", host.clone()).unwrap(), 102);
    let cancelled = client.cancel(&plan.id);
    resume.send(()).unwrap();
    let recovered = thread.join().unwrap().unwrap().unwrap();
    assert_eq!(cancelled.unwrap().state, OperationState::Cancelled);
    assert_eq!(recovered.state, OperationState::Cancelled);
    assert_eq!(host.restored.load(Ordering::SeqCst), 1);
}

#[test]
fn reboot_wait_does_not_refuse_review_and_cancellation_is_still_observed() {
    let dir = tempfile::tempdir().unwrap();
    let host = Host { pause_for_reboot: true, ..Host::default() };
    let mut client = Coordinator::open(dir.path(), "user", host.clone()).unwrap();
    let plan = client.plan(PlanRequest::Configure { adapter_id: AdapterId::new("nic").unwrap() }, 100).unwrap();
    assert!(client.apply(&plan.id, 101).unwrap().reboot_required);
    let (resume, thread) = paused_read(Coordinator::open(dir.path(), "service", host.clone()).unwrap(), 102);
    let reviewed = client.reviewed_plan(&plan.id, 102);
    let cancelled = client.cancel(&plan.id);
    resume.send(()).unwrap();
    let recovered = thread.join().unwrap().unwrap().unwrap();
    reviewed.expect("a reboot-waiting watchdog must not reject the owner's review");
    assert_eq!(cancelled.unwrap().state, OperationState::Cancelled);
    assert_eq!(recovered.state, OperationState::Cancelled);
    assert_eq!(host.restored.load(Ordering::SeqCst), 1);
}

#[test]
fn reboot_wait_needs_no_transaction_lock() {
    let dir = tempfile::tempdir().unwrap();
    let host = Host { pause_for_reboot: true, ..Host::default() };
    let mut client = Coordinator::open(dir.path(), "user", host.clone()).unwrap();
    let plan = client.plan(PlanRequest::Configure { adapter_id: AdapterId::new("nic").unwrap() }, 100).unwrap();
    client.apply(&plan.id, 101).unwrap();
    let lock = std::fs::OpenOptions::new().read(true).write(true).open(dir.path().join("operation.lock")).unwrap();
    lock.lock().unwrap();
    let mut recovery = Coordinator::open(dir.path(), "service", host.clone()).unwrap();
    assert!(recovery.recover(102).expect("waiting for a reboot must only read status").unwrap().reboot_required);
    assert_eq!(host.restored.load(Ordering::SeqCst), 0);
}
