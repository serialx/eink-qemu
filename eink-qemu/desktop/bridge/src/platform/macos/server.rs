//! Long-lived root service. Recovery outlives individual desktop connections.
use crate::{coordinator::Coordinator, macos_dispatch::Dispatcher, macos_platform::MacPlatform,
    macos_service::{NativeService, JOURNAL_ROOT, SOCKET_ROOT, validate_protected_file}, model::*,
    recovery::RecoveryWorker, transport, protocol::{HelperReply, ReplyKind, RequestKind}};
use std::{fs::{self, OpenOptions}, os::unix::{fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt}, net::{UnixListener, UnixStream}},
    path::Path, sync::{Arc, Mutex, atomic::{AtomicUsize, Ordering}}, time::{Duration, SystemTime, UNIX_EPOCH}};

fn io(error: impl std::fmt::Display) -> BridgeError { BridgeError::new("EIPC", error.to_string()) }
pub fn now() -> u64 { SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() }
fn start_recovery() -> Result<RecoveryWorker, BridgeError> {
    RecoveryWorker::start(Coordinator::open(Path::new(JOURNAL_ROOT), "service", MacPlatform::for_recovery()?)?, Duration::from_secs(1), now)
}
fn health(worker: &Mutex<Result<RecoveryWorker, BridgeError>>) -> Result<(), BridgeError> {
    match &*worker.lock().unwrap() { Ok(worker) => worker.health(), Err(error) => Err(error.clone()) }
}

pub fn serve() -> Result<(), BridgeError> {
    let native = NativeService::new()?;
    native.prepare_helper_directories()?;
    let lock_path = Path::new(JOURNAL_ROOT).join("service.lock");
    let lock = OpenOptions::new().read(true).write(true).create(true).mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC).open(&lock_path).map_err(io)?;
    validate_protected_file(&lock_path)?;
    lock.try_lock().map_err(|_| BridgeError::new("EBUSY", "The bridge helper is already running."))?;
    // Keep diagnostics and explicit restoration available after a recovery
    // conflict. No new mutation or transport acquisition is permitted until an
    // owner retries restoration and the independent watchdog starts again.
    let recovery = Arc::new(Mutex::new(start_recovery()));
    let socket = Path::new(SOCKET_ROOT).join("bridge.sock");
    match fs::symlink_metadata(&socket) {
        Ok(meta) => {
            if meta.uid() != 0 || !meta.file_type().is_socket() { return Err(BridgeError::new("EPERM", "Unsafe bridge helper endpoint.")); }
            match UnixStream::connect(&socket) {
                Ok(_) => return Err(BridgeError::new("EBUSY", "A bridge helper already owns the endpoint.")),
                Err(error) if error.raw_os_error() == Some(libc::ECONNREFUSED) => fs::remove_file(&socket).map_err(io)?,
                Err(error) => return Err(io(error)),
            }
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        Err(error) => return Err(io(error)),
    }
    let listener = UnixListener::bind(&socket).map_err(io)?;
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o666)).map_err(io)?;
    listener.set_nonblocking(true).map_err(io)?;
    let active = Arc::new(AtomicUsize::new(0));
    loop {
        match listener.accept() {
            Ok((mut stream, _)) => {
                // Bound slow/incomplete clients. Never hold the watchdog behind
                // socket reads, authorization checks or platform command waits.
                if active.load(Ordering::Acquire) >= 16 { continue; }
                let caller = match transport::unix::caller(&stream) { Ok(caller) => caller, Err(_) => continue };
                let mut dispatcher = match Dispatcher::new(Path::new(JOURNAL_ROOT), &caller) { Ok(dispatcher) => dispatcher, Err(_) => continue };
                stream.set_read_timeout(Some(Duration::from_secs(5))).map_err(io)?;
                stream.set_write_timeout(Some(Duration::from_secs(5))).map_err(io)?;
                active.fetch_add(1, Ordering::AcqRel);
                let count = active.clone(); let worker = recovery.clone();
                let spawn = std::thread::Builder::new().name("bridge-request".into()).spawn(move || {
                    struct Active(Arc<AtomicUsize>);
                    impl Drop for Active { fn drop(&mut self) { self.0.fetch_sub(1, Ordering::AcqRel); } }
                    let _active = Active(count);
                    let request = transport::read_request(&mut stream)?;
                    if let Err(error) = health(&worker) {
                        if !matches!(request.request, RequestKind::Inspect | RequestKind::Recover | RequestKind::Cancel { .. }) {
                            return transport::write_reply(&mut stream, &HelperReply::new(ReplyKind::Error { error }));
                        }
                    }
                    let retry = matches!(request.request, RequestKind::Cancel { .. });
                    let (reply, lease) = dispatcher.request(request, now());
                    if retry && matches!(&reply.reply, ReplyKind::Operation { operation: Some(operation) } if operation.state == OperationState::Cancelled)
                        && health(&worker).is_err() {
                        *worker.lock().unwrap() = start_recovery();
                    }
                    transport::write_reply(&mut stream, &reply)?;
                    if let Some(lease) = lease { transport::unix::send_lease_file(&stream, lease.file())?; }
                    Ok::<(), BridgeError>(())
                });
                if let Err(error) = spawn { active.fetch_sub(1, Ordering::AcqRel); return Err(io(error)); }
            },
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(50)),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => (),
            Err(error) => return Err(io(error)),
        }
    }
}
