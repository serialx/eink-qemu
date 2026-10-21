use eink_bridge::{coordinator::{Coordinator, PlatformOps, Prepared, Resource}, model::*, recovery::RecoveryWorker};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering}, mpsc};
use std::time::Duration;

// The live watchdog uses the same nonblocking transaction lock. A fixture
// request may legitimately lose a tick's race, especially on an emulated host.
fn when_unlocked<T>(mut request: impl FnMut() -> Result<T, BridgeError>) -> T {
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    loop {
        match request() {
            Ok(value) => return value,
            Err(error) if error.code == "EBUSY" && std::time::Instant::now() < deadline => std::thread::sleep(Duration::from_millis(1)),
            Err(error) => panic!("fixture request failed: {error}"),
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct Snapshot;
#[derive(Clone)]
struct Host { restored: mpsc::Sender<()>, mutations: Arc<AtomicUsize>, fail_restore: Arc<AtomicBool> }
impl PlatformOps for Host {
    type Snapshot = Snapshot;
    type Mutation = ();
    fn prepare(&mut self, request: PlanRequest, id: OperationId, now: u64, _: Option<&Resource<Snapshot>>) -> Result<Prepared<Snapshot, ()>, BridgeError> {
        Ok(Prepared { plan: BridgePlan { id, request, fingerprint: "nic".into(), changes: vec![], requires_authorization: true, may_disconnect: true, expires_at: now + 300 }, snapshot: Snapshot, mutations: vec![()] })
    }
    fn fingerprint(&mut self, _: &PlanRequest, _: &Self::Snapshot) -> Result<String, BridgeError> { Ok("nic".into()) }
    fn apply(&mut self, _: &()) -> Result<(), BridgeError> { self.mutations.fetch_add(1, Ordering::SeqCst); Ok(()) }
    fn verify(&mut self, _: &Snapshot) -> Result<(), BridgeError> { Ok(()) }
    fn commit(&mut self, _: &Snapshot) -> Result<(), BridgeError> { Ok(()) }
    fn rollback(&mut self, _: &Snapshot) -> Result<(), BridgeError> { self.restored.send(()).unwrap(); if self.fail_restore.load(Ordering::SeqCst) { Err(BridgeError::new("EIO", "Host refused restoration")) } else { Ok(()) } }
}

#[test]
fn service_watchdog_restores_after_the_request_handler_disappears() {
    let dir = tempfile::tempdir().unwrap();
    let (tx, restored) = mpsc::channel();
    let host = Host { restored: tx, mutations: Arc::new(AtomicUsize::new(0)), fail_restore: Arc::new(AtomicBool::new(false)) };
    let clock = Arc::new(AtomicU64::new(100));
    let worker_clock = clock.clone();
    let worker = RecoveryWorker::start(
        Coordinator::open(dir.path(), "service", host.clone()).unwrap(),
        Duration::from_millis(10), move || worker_clock.load(Ordering::SeqCst),
    ).unwrap();
    let mut request_handler = Coordinator::open(dir.path(), "user", host.clone()).unwrap();
    let plan = when_unlocked(|| request_handler.plan(PlanRequest::Configure { adapter_id: AdapterId::new("nic").unwrap() }, 100));
    when_unlocked(|| request_handler.apply(&plan.id, 101));
    drop(request_handler);
    clock.store(222, Ordering::SeqCst);
    restored.recv_timeout(Duration::from_secs(3)).expect("recovery must not need another client request");
    worker.stop().unwrap();
    assert_eq!(host.mutations.load(Ordering::SeqCst), 1);
    let mut coordinator = Coordinator::open(dir.path(), "user", host).unwrap();
    assert_eq!(coordinator.recover(223).unwrap().unwrap().state, OperationState::Cancelled);
}

#[test]
fn service_startup_reconciles_before_returning_a_worker() {
    let dir = tempfile::tempdir().unwrap();
    let (tx, restored) = mpsc::channel();
    let host = Host { restored: tx, mutations: Arc::new(AtomicUsize::new(0)), fail_restore: Arc::new(AtomicBool::new(false)) };
    let mut request_handler = Coordinator::open(dir.path(), "user", host.clone()).unwrap();
    let plan = when_unlocked(|| request_handler.plan(PlanRequest::Configure { adapter_id: AdapterId::new("nic").unwrap() }, 100));
    when_unlocked(|| request_handler.apply(&plan.id, 101));
    drop(request_handler);
    let worker = RecoveryWorker::start(Coordinator::open(dir.path(), "service", host).unwrap(), Duration::from_secs(1), || 102).unwrap();
    restored.try_recv().expect("startup recovery must finish before clients are accepted");
    worker.stop().unwrap();
}

#[test]
fn failed_restoration_stops_the_watchdog_without_repeating_mutations() {
    let dir = tempfile::tempdir().unwrap();
    let (tx, restored) = mpsc::channel();
    let host = Host { restored: tx, mutations: Arc::new(AtomicUsize::new(0)), fail_restore: Arc::new(AtomicBool::new(true)) };
    let clock = Arc::new(AtomicU64::new(100));
    let worker_clock = clock.clone();
    let worker = RecoveryWorker::start(Coordinator::open(dir.path(), "service", host.clone()).unwrap(), Duration::from_millis(10), move || worker_clock.load(Ordering::SeqCst)).unwrap();
    let mut request_handler = Coordinator::open(dir.path(), "user", host.clone()).unwrap();
    let plan = when_unlocked(|| request_handler.plan(PlanRequest::Configure { adapter_id: AdapterId::new("nic").unwrap() }, 100));
    when_unlocked(|| request_handler.apply(&plan.id, 101));
    drop(request_handler);
    clock.store(222, Ordering::SeqCst);
    restored.recv_timeout(Duration::from_secs(3)).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while worker.health().is_ok() && std::time::Instant::now() < deadline { std::thread::yield_now(); }
    assert_eq!(worker.health().unwrap_err().code, "EROLLBACK");
    assert_eq!(worker.stop().unwrap_err().code, "EROLLBACK");
    assert!(restored.try_recv().is_err());
    let restarted = RecoveryWorker::start(Coordinator::open(dir.path(), "service", host.clone()).unwrap(), Duration::from_secs(1), || 223);
    assert!(matches!(restarted, Err(error) if error.code == "EROLLBACK"));
    assert!(restored.try_recv().is_err(), "service restart must not retry a recorded restoration conflict");
    let mut coordinator = Coordinator::open(dir.path(), "user", host.clone()).unwrap();
    assert_eq!(coordinator.plan(PlanRequest::Configure { adapter_id: AdapterId::new("other").unwrap() }, 223).unwrap_err().code, "ERECOVERY");
    host.fail_restore.store(false, Ordering::SeqCst);
    assert_eq!(coordinator.cancel(&plan.id).unwrap().state, OperationState::Cancelled);
    restored.try_recv().expect("an explicit retry may restore the protected snapshot");
}
