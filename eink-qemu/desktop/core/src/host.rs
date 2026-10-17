//! Host facts the standard library does not give on every platform.

use std::path::Path;

/// A file's identity: device and inode on Unix, volume serial and file
/// index on Windows. Hard links to one file share it.
pub fn file_identity(path: &Path) -> Option<(u64, u64)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let meta = std::fs::metadata(path).ok()?;
        Some((meta.dev(), meta.ino()))
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION};
        // No access rights: only the handle's metadata is read, so a file
        // open elsewhere (QEMU shares read and write only) still opens.
        let file = std::fs::OpenOptions::new().access_mode(0).open(path).ok()?;
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
            return None;
        }
        Some((u64::from(info.dwVolumeSerialNumber), (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow)))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        None
    }
}

/// Whether a process with this id exists. Errs towards true: a directory
/// whose owner might be alive is left alone.
pub fn process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        let Ok(pid) = libc::pid_t::try_from(pid) else { return true };
        let found = unsafe { libc::kill(pid, 0) } == 0;
        found || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_INVALID_PARAMETER, STILL_ACTIVE};
        use windows_sys::Win32::System::Threading::{GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle.is_null() {
                // No such process; any other error (access denied) means one exists.
                return GetLastError() != ERROR_INVALID_PARAMETER;
            }
            let mut code = 0u32;
            let alive = GetExitCodeProcess(handle, &mut code) == 0 || code == STILL_ACTIVE as u32;
            CloseHandle(handle);
            alive
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_process_is_alive_and_an_unused_id_is_not() {
        // No child is spawned: a child would briefly share the lock files
        // other tests release, so they would look still held.
        assert!(process_alive(std::process::id()));
        assert!(!process_alive(i32::MAX as u32 - 1));
    }

    #[test]
    fn hard_links_share_an_identity() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b, c) = (dir.path().join("a"), dir.path().join("b"), dir.path().join("c"));
        std::fs::write(&a, b"x").unwrap();
        std::fs::write(&c, b"x").unwrap();
        std::fs::hard_link(&a, &b).unwrap();
        assert!(file_identity(&a).is_some());
        assert_eq!(file_identity(&a), file_identity(&b));
        assert_ne!(file_identity(&a), file_identity(&c));
        assert_eq!(file_identity(&dir.path().join("missing")), None);
    }
}
