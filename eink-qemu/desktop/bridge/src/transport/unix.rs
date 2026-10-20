use super::Caller;
use crate::model::BridgeError;
use std::{os::{fd::AsRawFd, unix::{fs::{FileTypeExt, MetadataExt}, net::UnixStream}}, path::Path, time::Duration};

fn permission() -> BridgeError { BridgeError::new("EPERM", "Cannot authenticate the bridge helper connection.") }
fn peer_uid(stream: &UnixStream) -> Result<u32, BridgeError> {
    #[cfg(target_os = "macos")] {
        let mut uid = 0; let mut gid = 0;
        if unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) } != 0 { return Err(permission()); }
        Ok(uid)
    }
    #[cfg(target_os = "linux")] {
        let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
        let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        if unsafe { libc::getsockopt(stream.as_raw_fd(), libc::SOL_SOCKET, libc::SO_PEERCRED,
            (&mut credentials as *mut libc::ucred).cast(), &mut length) } != 0
            || length as usize != std::mem::size_of::<libc::ucred>() { return Err(permission()); }
        Ok(credentials.uid)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    { let _ = stream; Err(permission()) }
}
pub fn caller(stream: &UnixStream) -> Result<Caller, BridgeError> {
    Ok(Caller { principal: format!("uid:{}", peer_uid(stream)?) })
}
pub fn verify_peer(stream: &UnixStream, expected_uid: u32) -> Result<(), BridgeError> {
    if peer_uid(stream)? != expected_uid { return Err(permission()); }
    Ok(())
}

/// App-side connection to the fixed, installer-owned service endpoint.
pub fn connect() -> Result<UnixStream, BridgeError> {
    #[cfg(target_os = "macos")]
    let path = Path::new("/var/run/eink-qemu/bridge.sock");
    #[cfg(not(target_os = "macos"))]
    let path = Path::new("/run/eink-qemu/bridge.sock");
    let meta = std::fs::symlink_metadata(path).map_err(|_| BridgeError::new("ENEEDSSETUP", "Install bridge support first."))?;
    if !meta.file_type().is_socket() || meta.uid() != 0 { return Err(permission()); }
    // Canonical system aliases such as macOS /var are allowed. Every actual
    // ancestor must remain installer-controlled.
    let parent = std::fs::canonicalize(path.parent().ok_or_else(permission)?).map_err(|_| permission())?;
    for directory in parent.ancestors() {
        let meta = std::fs::metadata(directory).map_err(|_| permission())?;
        if !meta.is_dir() || meta.uid() != 0 || meta.mode() & 0o022 != 0 { return Err(permission()); }
    }
    let stream = UnixStream::connect(path).map_err(|e| BridgeError::new("EIPC", e.to_string()))?;
    verify_peer(&stream, 0)?;
    stream.set_read_timeout(Some(Duration::from_secs(30))).map_err(|e| BridgeError::new("EIPC", e.to_string()))?;
    stream.set_write_timeout(Some(Duration::from_secs(30))).map_err(|e| BridgeError::new("EIPC", e.to_string()))?;
    Ok(stream)
}
