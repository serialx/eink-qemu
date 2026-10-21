//! Atomic records in a caller-supplied, installer-controlled directory.
//! The privileged entry point must use its fixed installation path, never a
//! path from IPC. Ownership here is filesystem ownership, not the IPC caller.
use crate::model::BridgeError;
use serde::{Serialize, de::DeserializeOwned};
use std::{fs::{self, File, OpenOptions}, io::{Read, Write}, path::{Path, PathBuf}};

pub(crate) struct Journal {
    root: PathBuf,
    #[cfg(windows)]
    principal: String,
}
fn io(error: impl std::fmt::Display) -> BridgeError { BridgeError::new("EJOURNAL", error.to_string()) }
impl Journal {
    pub fn open(root: &Path) -> Result<Self, BridgeError> {
        let meta = fs::symlink_metadata(root).map_err(io)?;
        if !meta.is_dir() || meta.file_type().is_symlink() { return Err(io("Bridge journal must be a real directory.")); }
        #[cfg(unix)] {
            use std::os::unix::fs::MetadataExt;
            if meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o022 != 0 {
                return Err(io("Bridge journal directory has unsafe ownership or permissions."));
            }
        }
        #[cfg(target_os = "macos")]
        crate::transport::unix::validate_no_acl_writes(&File::open(root).map_err(io)?).map_err(io)?;
        #[cfg(windows)]
        let principal = {
            use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
            for ancestor in root.ancestors() {
                if fs::symlink_metadata(ancestor).map_err(io)?.file_attributes() & 0x400 != 0 { return Err(io("Bridge journal ancestors cannot be reparse points.")); }
            }
            let directory = OpenOptions::new().read(true).custom_flags(0x02200000).open(root).map_err(io)?;
            let principal = crate::windows_security::current_sid()?;
            crate::windows_security::validate_handle(&directory, Some(&principal))?;
            principal
        };
        Ok(Self { root: fs::canonicalize(root).map_err(io)?, #[cfg(windows)] principal })
    }
    fn open_file(&self, name: &str, create: bool) -> Result<File, BridgeError> {
        let mut options = OpenOptions::new(); options.read(true).write(create).create(create);
        #[cfg(unix)] {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        }
        #[cfg(windows)] {
            use std::os::windows::fs::OpenOptionsExt;
            options.custom_flags(0x00200000); // FILE_FLAG_OPEN_REPARSE_POINT
        }
        let file = options.open(self.root.join(name)).map_err(io)?;
        let meta = file.metadata().map_err(io)?;
        if !meta.is_file() { return Err(io("Invalid bridge journal file.")); }
        #[cfg(target_os = "macos")]
        crate::transport::unix::validate_no_acl_writes(&file).map_err(io)?;
        #[cfg(unix)] {
            use std::os::unix::fs::MetadataExt;
            if meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 || meta.nlink() != 1 {
                return Err(io("Bridge journal file has unsafe ownership or permissions."));
            }
        }
        #[cfg(windows)] {
            use std::os::windows::fs::MetadataExt;
            if meta.file_attributes() & 0x400 != 0 { return Err(io("Bridge journal cannot be a reparse point.")); }
            crate::windows_security::validate_private_handle(&file, &self.principal)?;
        }
        Ok(file)
    }
    pub fn lock(&self) -> Result<File, BridgeError> {
        let file = self.open_file("operation.lock", true)?;
        file.try_lock().map_err(|_| BridgeError::new("EBUSY", "Another bridge operation is running."))?;
        Ok(file)
    }
    pub fn resource_lock(&self, id: &crate::model::ResourceId, shared: bool) -> Result<File, BridgeError> {
        let name = format!("resource-{}.lock", id.as_str());
        #[cfg(windows)] {
            use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
            // Windows byte-range locks belong to a process; duplicating their
            // file handle does not retain them after that process exits. File
            // sharing restrictions instead live with the open handle and its
            // duplicates, including the emulator's inherited handle.
            if shared {
                match fs::symlink_metadata(self.root.join(&name)) {
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => { drop(self.open_file(&name, true)?); },
                    Err(error) => return Err(io(error)),
                    Ok(_) => (),
                }
            }
            let file = OpenOptions::new().read(true).write(!shared).create(!shared)
                .share_mode(if shared { 1 } else { 0 }) // FILE_SHARE_READ; an exclusive writer blocks new readers.
                .custom_flags(0x00200000) // FILE_FLAG_OPEN_REPARSE_POINT
                .open(self.root.join(name)).map_err(|error| {
                    if error.raw_os_error() == Some(32) { BridgeError::new("EBUSY", "This bridge is in use. Stop its emulators first.") }
                    else { io(error) }
                })?;
            let meta = file.metadata().map_err(io)?;
            if !meta.is_file() || meta.file_attributes() & 0x400 != 0 || meta.len() != 0 { return Err(io("Invalid bridge lease file.")); }
            crate::windows_security::validate_private_handle(&file, &self.principal)?;
            return Ok(file);
        }
        #[cfg(not(windows))] {
        let created = self.open_file(&name, true)?;
        // The ordinary client may retain this descriptor across helper restarts.
        // Give it only read access to the empty lock file, never a root writer.
        let file = if shared { drop(created); self.open_file(&name, false)? } else { created };
        let result = if shared { file.try_lock_shared() } else { file.try_lock() };
        result.map_err(|_| BridgeError::new("EBUSY", "This bridge is in use. Stop its emulators first."))?;
        Ok(file)
        }
    }
    pub fn read<T: DeserializeOwned>(&self) -> Result<Option<T>, BridgeError> {
        match fs::symlink_metadata(self.root.join("operation.json")) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(io(e)),
            Ok(_) => (),
        }
        let file = self.open_file("operation.json", false)?;
        if file.metadata().map_err(io)?.len() > 4 * 1024 * 1024 { return Err(io("Bridge journal is too large.")); }
        let mut text = String::new(); file.take(4 * 1024 * 1024 + 1).read_to_string(&mut text).map_err(io)?;
        Ok(Some(serde_json::from_str(&text).map_err(io)?))
    }
    pub fn write<T: Serialize>(&self, value: &T) -> Result<(), BridgeError> {
        let mut temp = tempfile::NamedTempFile::new_in(&self.root).map_err(io)?;
        #[cfg(windows)]
        crate::windows_security::validate_private_handle(temp.as_file(), &self.principal)?;
        serde_json::to_writer(&mut temp, value).map_err(io)?;
        temp.flush().map_err(io)?; temp.as_file().sync_all().map_err(io)?;
        temp.persist(self.root.join("operation.json")).map_err(io)?;
        #[cfg(unix)] File::open(&self.root).and_then(|f| f.sync_all()).map_err(io)?;
        Ok(())
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;
    use std::{io::{BufRead, BufReader}, process::{Command, Stdio}, time::Duration};

    #[test]
    fn lease_survives_handle_transfer_and_releases_after_last_process() {
        if std::env::var_os("EINK_BRIDGE_TEST_LEASE_CHILD").is_some() {
            // The standard input handle is the read-only lease duplicate.
            println!("lease-child-ready");
            std::io::stdout().flush().unwrap();
            std::thread::sleep(Duration::from_secs(3));
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let journal = Journal::open(root.path()).unwrap();
        let id = crate::model::ResourceId::new("11223344556677889900aabbccddeeff").unwrap();
        let lease = journal.resource_lock(&id, true).unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "journal::windows_tests::lease_survives_handle_transfer_and_releases_after_last_process", "--nocapture"])
            .env("EINK_BRIDGE_TEST_LEASE_CHILD", "1")
            .stdin(Stdio::from(lease)).stdout(Stdio::piped()).spawn().unwrap();
        // The temporary Command and original lease have both been dropped.
        let mut output = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        loop {
            assert_ne!(output.read_line(&mut line).unwrap(), 0, "child did not retain the lease");
            if line.contains("lease-child-ready") { break; }
            line.clear();
        }
        let second = journal.resource_lock(&id, true).unwrap();
        drop(second);
        assert_eq!(journal.resource_lock(&id, false).unwrap_err().code, "EBUSY");
        assert!(child.wait().unwrap().success());
        let writer = journal.resource_lock(&id, false).unwrap();
        assert_eq!(journal.resource_lock(&id, true).unwrap_err().code, "EBUSY");
        drop(writer);
        assert!(journal.resource_lock(&id, true).is_ok());
    }
}
