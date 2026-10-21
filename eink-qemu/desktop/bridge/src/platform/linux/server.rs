//! Root service with bounded clients and a watchdog independent of the UI.
use crate::{coordinator::Coordinator, linux_dispatch::Dispatcher, linux_platform::LinuxPlatform,
    model::*, protocol::{HelperReply, ReplyKind, RequestKind}, recovery::RecoveryWorker, transport};
use std::{fs::{self, OpenOptions}, os::unix::{fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt}, net::{UnixListener, UnixStream}},
    path::Path, sync::{Arc, Mutex, atomic::{AtomicUsize, Ordering}}, time::{Duration, SystemTime, UNIX_EPOCH}};
pub const JOURNAL_ROOT: &str = "/var/lib/eink-qemu/bridge";
pub const SOCKET_ROOT: &str = "/run/eink-qemu";
fn io(error: impl std::fmt::Display) -> BridgeError { BridgeError::new("EIPC", error.to_string()) }
fn now() -> u64 { SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() }
fn notify_ready() -> Result<(), BridgeError> {
    use std::os::{linux::net::SocketAddrExt, unix::{ffi::OsStrExt, net::{SocketAddr, UnixDatagram}}};
    let Some(path) = std::env::var_os("NOTIFY_SOCKET") else { return Ok(()); };
    let bytes = path.as_bytes();
    let address = if bytes.first() == Some(&b'@') { SocketAddr::from_abstract_name(&bytes[1..]).map_err(io)? }
        else { SocketAddr::from_pathname(Path::new(&path)).map_err(io)? };
    let socket = UnixDatagram::unbound().map_err(io)?;
    socket.set_write_timeout(Some(Duration::from_secs(5))).map_err(io)?;
    socket.send_to_addr(b"READY=1\nSTATUS=Bridge setup service ready", &address).map_err(io)?;
    Ok(())
}
fn directory(path: &str, mode: u32) -> Result<(), BridgeError> {
    let path = Path::new(path);
    transport::unix::validate_system_directory(path.parent().ok_or_else(|| io("Invalid service directory"))?)?;
    match fs::DirBuilder::new().mode(mode).create(path) {
        Ok(()) => (),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
        Err(error) => return Err(io(error)),
    }
    for prefix in path.ancestors() {
        if fs::symlink_metadata(prefix).map_err(io)?.file_type().is_symlink() { return Err(io("Service directories cannot be symlinks.")); }
    }
    transport::unix::validate_system_directory(path)?;
    Ok(())
}
fn start_recovery() -> Result<RecoveryWorker, BridgeError> {
    RecoveryWorker::start(Coordinator::open(Path::new(JOURNAL_ROOT), "service", LinuxPlatform::for_recovery()?)?, Duration::from_secs(1), now)
}
fn health(worker: &Mutex<Result<RecoveryWorker, BridgeError>>) -> Result<(), BridgeError> {
    match &*worker.lock().unwrap() { Ok(worker) => worker.health(), Err(error) => Err(error.clone()) }
}
fn service_lock()->Result<fs::File,BridgeError>{
    for prefix in Path::new(JOURNAL_ROOT).ancestors(){
        if fs::symlink_metadata(prefix).map_err(io)?.file_type().is_symlink(){return Err(io("Service directories cannot be symlinks."));}
        transport::unix::validate_system_directory(prefix)?;
    }
    let lock = OpenOptions::new().read(true).write(true).create(true).mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC).open(Path::new(JOURNAL_ROOT).join("service.lock")).map_err(io)?;
    let meta = lock.metadata().map_err(io)?;
    if !meta.is_file() || meta.uid() != 0 || meta.mode() & 0o077 != 0 || meta.nlink() != 1 { return Err(io("Unsafe bridge service lock.")); }
    lock.try_lock().map_err(|_| BridgeError::new("EBUSY", "The bridge service is already running."))?;
    Ok(lock)
}
/// Fixed package-maintainer entry point. Stop the system service first. A
/// conflict or surviving emulator prevents package removal; preserve the
/// helper, policy and journal so the ordinary app can offer recovery.
pub fn uninstall()->Result<(),BridgeError>{
    if unsafe{libc::geteuid()}!=0{return Err(BridgeError::new("EPERM","Only the package manager can uninstall bridge support."));}
    match fs::symlink_metadata(JOURNAL_ROOT){
        Err(error) if error.kind()==std::io::ErrorKind::NotFound=>return Ok(()),
        Err(error)=>return Err(io(error)),
        Ok(_)=>(),
    }
    let _lock=service_lock()?;
    let mut coordinator=Coordinator::open(Path::new(JOURNAL_ROOT),"service",LinuxPlatform::for_recovery()?)?;
    coordinator.uninstall(|resource|{
        let uid=resource.original.grant().uid();
        if resource.owner!=format!("uid:{uid}"){return Err(BridgeError::new("EPERM","The recorded bridge owner does not match its access grant."));}
        LinuxPlatform::for_user(uid)
    },now)
}
pub fn serve() -> Result<(), BridgeError> {
    if unsafe { libc::geteuid() } != 0 { return Err(BridgeError::new("EPERM", "Start bridge support through its installed system service.")); }
    for (path, mode) in [("/var/lib/eink-qemu", 0o755), (JOURNAL_ROOT, 0o700), (SOCKET_ROOT, 0o755),
        ("/etc/eink-qemu", 0o755), ("/etc/eink-qemu/bridge", 0o755)] { directory(path, mode)?; }
    let _lock=service_lock()?;
    let recovery = Arc::new(Mutex::new(start_recovery()));
    let socket = Path::new(SOCKET_ROOT).join("bridge.sock");
    match fs::symlink_metadata(&socket) {
        Ok(meta) => {
            if meta.uid() != 0 || !meta.file_type().is_socket() { return Err(io("Unsafe bridge service endpoint.")); }
            match UnixStream::connect(&socket) {
                Ok(_) => return Err(BridgeError::new("EBUSY", "Another service owns the bridge endpoint.")),
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
    // systemctl start completes only once recovery and the IPC listener are
    // ready, so clients do not mistake an initializing helper for missing setup.
    notify_ready()?;
    let active = Arc::new(AtomicUsize::new(0));
    loop {
        match listener.accept() {
            Ok((mut stream, _)) => {
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
                    let (reply, lease) = dispatcher.request(request);
                    if retry && matches!(&reply.reply, ReplyKind::Operation { operation: Some(operation) } if matches!(operation.state, OperationState::Cancelled | OperationState::Ready))
                        && health(&worker).is_err() { *worker.lock().unwrap() = start_recovery(); }
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
