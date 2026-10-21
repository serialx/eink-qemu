//! Service-owned watchdog. Keep this alive for the privileged service lifetime,
//! independently of request connections and the desktop UI. The service must
//! refuse mutations if `health` reports failure and arrange restart at boot.
use crate::{coordinator::{Coordinator, PlatformOps}, model::{BridgeError, Operation, OperationState}};
use std::{sync::{Arc, Mutex, mpsc}, thread::{self, JoinHandle}, time::Duration};

pub struct RecoveryWorker {
    stop: Option<mpsc::Sender<()>>,
    thread: Option<JoinHandle<Result<(), BridgeError>>>,
    failure: Arc<Mutex<Option<BridgeError>>>,
}
fn restored(operation: Option<Operation>) -> Result<(), BridgeError> {
    if let Some(operation) = operation {
        if operation.state == OperationState::Failed {
            if let Some(error) = operation.error.filter(|e| e.code == "EROLLBACK") { return Err(error); }
        }
    }
    Ok(())
}
impl RecoveryWorker {
    /// Startup reconciliation completes synchronously before the caller can
    /// open its IPC listener. Platform calls must have bounded native timeouts.
    pub fn start<P, C>(mut coordinator: Coordinator<P>, interval: Duration, clock: C) -> Result<Self, BridgeError>
    where P: PlatformOps + Send + 'static, C: Fn() -> u64 + Send + 'static {
        if interval.is_zero() || interval > Duration::from_secs(1) {
            return Err(BridgeError::new("EINVAL", "Bridge recovery must run at least once per second."));
        }
        restored(coordinator.recover_startup()?)?;
        let (stop, receiver) = mpsc::channel();
        let failure = Arc::new(Mutex::new(None));
        let worker_failure = failure.clone();
        let thread = thread::Builder::new().name("bridge-recovery".into()).spawn(move || {
            let result = loop {
                match receiver.recv_timeout(interval) {
                    Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break Ok(()),
                    Err(mpsc::RecvTimeoutError::Timeout) => (),
                }
                match coordinator.recover(clock()) {
                    Ok(operation) => if let Err(error) = restored(operation) { break Err(error); },
                    // A request currently holds the transaction lock. Its
                    // durable intent remains available for the next tick.
                    Err(error) if error.code == "EBUSY" => (),
                    Err(error) => break Err(error),
                }
            };
            if let Err(error) = &result { *worker_failure.lock().unwrap() = Some(error.clone()); }
            result
        }).map_err(|e| BridgeError::new("ERECOVERY", format!("Cannot start bridge recovery: {e}")))?;
        Ok(Self { stop: Some(stop), thread: Some(thread), failure })
    }
    pub fn health(&self) -> Result<(), BridgeError> {
        if let Some(error) = self.failure.lock().unwrap().clone() { return Err(error); }
        if self.thread.as_ref().is_some_and(JoinHandle::is_finished) {
            return Err(BridgeError::new("ERECOVERY", "Bridge recovery worker stopped unexpectedly."));
        }
        Ok(())
    }
    pub fn stop(mut self) -> Result<(), BridgeError> { self.join() }
    fn join(&mut self) -> Result<(), BridgeError> {
        if let Some(stop) = self.stop.take() { let _ = stop.send(()); }
        match self.thread.take() {
            Some(thread) => thread.join().map_err(|_| BridgeError::new("ERECOVERY", "Bridge recovery worker failed."))?,
            None => Ok(()),
        }
    }
}
impl Drop for RecoveryWorker { fn drop(&mut self) { let _ = self.join(); } }
