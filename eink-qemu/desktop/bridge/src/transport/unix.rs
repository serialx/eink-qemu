use super::Caller;
use crate::model::BridgeError;
use std::{os::{fd::AsRawFd, unix::{fs::{FileTypeExt, MetadataExt}, net::UnixStream}}, path::Path, time::Duration};

fn permission() -> BridgeError { BridgeError::new("EPERM", "Cannot authenticate the bridge helper connection.") }
fn trusted_directory(path: &Path, uid: u32, gid: u32, mode: u32) -> bool {
    if uid != 0 || mode & 0o002 != 0 { return false; }
    if mode & 0o020 == 0 { return true; }
    // macOS ships this one system directory as root:daemon 0775. App
    // directories never receive this exception. Kernel peer authentication
    // remains mandatory, including if a system daemon races a rename.
    #[cfg(target_os = "macos")]
    if path == Path::new("/private/var/run") && gid == 1 && mode & 0o777 == 0o775 { return true; }
    let _ = (path, gid);
    false
}
pub fn validate_system_directory(path: &Path) -> Result<(), BridgeError> {
    for prefix in path.ancestors() {
        let link = std::fs::symlink_metadata(prefix).map_err(|_| permission())?;
        if link.uid() != 0 { return Err(permission()); }
    }
    let real = std::fs::canonicalize(path).map_err(|_| permission())?;
    for prefix in real.ancestors() {
        let meta = std::fs::metadata(prefix).map_err(|_| permission())?;
        if !meta.is_dir() || !trusted_directory(prefix, meta.uid(), meta.gid(), meta.mode()) { return Err(permission()); }
        #[cfg(target_os = "macos")]
        validate_no_acl_writes(&std::fs::File::open(prefix).map_err(|_| permission())?)?;
    }
    Ok(())
}

/// POSIX mode bits do not include macOS extended ACL permissions. Reject any
/// allow-entry granting mutation, including inherited grants. Deny entries and
/// traversal/read grants cannot widen write access and are retained.
#[cfg(target_os = "macos")]
pub fn validate_no_acl_writes(file: &std::fs::File) -> Result<(), BridgeError> {
    use std::ffi::c_void;
    extern "C" {
        fn acl_get_fd_np(fd: i32, kind: i32) -> *mut c_void;
        fn acl_free(acl: *mut c_void) -> i32;
        fn acl_valid(acl: *mut c_void) -> i32;
        fn acl_get_entry(acl: *mut c_void, index: i32, entry: *mut *mut c_void) -> i32;
        fn acl_get_tag_type(entry: *mut c_void, tag: *mut i32) -> i32;
        fn acl_get_permset_mask_np(entry: *mut c_void, mask: *mut u64) -> i32;
    }
    struct Acl(*mut c_void);
    impl Drop for Acl { fn drop(&mut self) { unsafe { acl_free(self.0); } } }
    let acl = unsafe { acl_get_fd_np(file.as_raw_fd(), 0x100) }; // ACL_TYPE_EXTENDED
    if acl.is_null() {
        // Darwin reports ENOENT for an open inode with no extended ACL.
        // The fd is already valid; this is not a missing pathname check.
        return if std::io::Error::last_os_error().raw_os_error() == Some(libc::ENOENT) { Ok(()) } else { Err(permission()) };
    }
    let acl = Acl(acl);
    if unsafe { acl_valid(acl.0) } != 0 { return Err(permission()); }
    // WRITE_DATA, DELETE, APPEND_DATA, DELETE_CHILD, WRITE_ATTRIBUTES,
    // WRITE_EXTATTRIBUTES, WRITE_SECURITY, CHANGE_OWNER (sys/acl.h).
    const MUTATION: u64 = (1 << 2) | (1 << 4) | (1 << 5) | (1 << 6) | (1 << 8) | (1 << 10) | (1 << 12) | (1 << 13);
    for index in 0..=169 { // Darwin limits an ACL to 169 entries.
        let mut entry = std::ptr::null_mut();
        if unsafe { acl_get_entry(acl.0, index, &mut entry) } != 0 {
            return if std::io::Error::last_os_error().raw_os_error() == Some(libc::EINVAL) { Ok(()) } else { Err(permission()) };
        }
        let mut tag = 0; let mut mask = 0;
        if unsafe { acl_get_tag_type(entry, &mut tag) } != 0 || unsafe { acl_get_permset_mask_np(entry, &mut mask) } != 0 {
            return Err(permission());
        }
        if tag == 1 && mask & MUTATION != 0 { return Err(permission()); } // ACL_EXTENDED_ALLOW
    }
    Err(permission())
}
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

/// Send only after the versioned lease reply, on its authenticated connection.
/// SCM_RIGHTS retains the same open file description and its shared OS lock.
pub fn send_lease_file(stream: &UnixStream, file: &std::fs::File) -> Result<(), BridgeError> {
    let mut marker = 1u8;
    let mut io = libc::iovec { iov_base: (&mut marker as *mut u8).cast(), iov_len: 1 };
    let mut control = [0usize; 8]; // Aligned for cmsghdr on Darwin and Linux.
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &mut io; message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = unsafe { libc::CMSG_SPACE(std::mem::size_of::<i32>() as _) } as _;
    unsafe {
        let header = libc::CMSG_FIRSTHDR(&message);
        (*header).cmsg_level = libc::SOL_SOCKET; (*header).cmsg_type = libc::SCM_RIGHTS;
        (*header).cmsg_len = libc::CMSG_LEN(std::mem::size_of::<i32>() as _) as _;
        std::ptr::write_unaligned(libc::CMSG_DATA(header).cast::<i32>(), file.as_raw_fd());
        if libc::sendmsg(stream.as_raw_fd(), &message, libc::MSG_NOSIGNAL) != 1 {
            return Err(BridgeError::new("EIPC", "Cannot transfer the bridge transport lease."));
        }
    }
    Ok(())
}

/// Call only after authenticating the root helper and matching its lease reply.
/// Close every received descriptor on malformed/truncated ancillary messages.
pub fn receive_lease_file(stream: &UnixStream) -> Result<std::fs::File, BridgeError> {
    use std::os::fd::FromRawFd;
    let invalid = || BridgeError::new("EPROTOCOL", "Invalid bridge transport lease.");
    let mut marker = 0u8;
    let mut io = libc::iovec { iov_base: (&mut marker as *mut u8).cast(), iov_len: 1 };
    let mut control = [0usize; 16];
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &mut io; message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast(); message.msg_controllen = std::mem::size_of_val(&control) as _;
    #[cfg(target_os = "linux")]
    let flags = libc::MSG_CMSG_CLOEXEC;
    #[cfg(not(target_os = "linux"))]
    let flags = 0;
    let received = unsafe { libc::recvmsg(stream.as_raw_fd(), &mut message, flags) };
    if received < 0 { return Err(invalid()); }
    let mut files = Vec::new();
    let mut malformed = false;
    unsafe {
        let mut header = libc::CMSG_FIRSTHDR(&message);
        while !header.is_null() {
            let base = libc::CMSG_LEN(0) as usize;
            let length = (*header).cmsg_len as usize;
            let end = message.msg_control as usize + message.msg_controllen as usize;
            if length < base || header as usize + length > end { malformed = true; break; }
            if (*header).cmsg_level == libc::SOL_SOCKET && (*header).cmsg_type == libc::SCM_RIGHTS {
                let size = std::mem::size_of::<i32>();
                if (length - base) % size != 0 { malformed = true; }
                for offset in (0..length - base).step_by(size) {
                    if offset + size > length - base { break; }
                    let fd = std::ptr::read_unaligned(libc::CMSG_DATA(header).add(offset).cast::<i32>());
                    let file = std::fs::File::from_raw_fd(fd);
                    if libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) < 0 { malformed = true; }
                    files.push(file);
                }
            } else { malformed = true; }
            header = libc::CMSG_NXTHDR(&message, header);
        }
    }
    if malformed || received != 1 || marker != 1 || message.msg_flags & libc::MSG_CTRUNC != 0 || files.len() != 1 { return Err(invalid()); }
    let file = files.pop().unwrap();
    let metadata = file.metadata().map_err(|_| invalid())?;
    if !metadata.is_file() || metadata.nlink() != 1 || metadata.len() != 0
        || unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) } & libc::O_ACCMODE != libc::O_RDONLY { return Err(invalid()); }
    Ok(file)
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
    validate_system_directory(path.parent().ok_or_else(permission)?)?;
    let stream = UnixStream::connect(path).map_err(|e| BridgeError::new("EIPC", e.to_string()))?;
    verify_peer(&stream, 0)?;
    stream.set_read_timeout(Some(Duration::from_secs(30))).map_err(|e| BridgeError::new("EIPC", e.to_string()))?;
    stream.set_write_timeout(Some(Duration::from_secs(30))).map_err(|e| BridgeError::new("EIPC", e.to_string()))?;
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn system_runtime_exception_cannot_extend_to_other_directories_or_groups() {
        assert!(!trusted_directory(Path::new("/private/var/run/app"), 0, 1, 0o775));
        assert!(!trusted_directory(Path::new("/private/var/run"), 0, 20, 0o775));
        assert!(!trusted_directory(Path::new("/private/var/run"), 501, 1, 0o775));
        assert!(!trusted_directory(Path::new("/private/var/run"), 0, 1, 0o777));
        #[cfg(target_os = "macos")]
        assert!(trusted_directory(Path::new("/private/var/run"), 0, 1, 0o775));
    }
}
