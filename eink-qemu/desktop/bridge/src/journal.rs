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
        self.validate_file(&file, create)?;
        Ok(file)
    }
    fn validate_file(&self, file: &File, create: bool) -> Result<(), BridgeError> {
        #[cfg(not(unix))]
        let _ = create;
        let meta = file.metadata().map_err(io)?;
        if !meta.is_file() { return Err(io("Invalid bridge journal file.")); }
        #[cfg(target_os = "macos")]
        crate::transport::unix::validate_no_acl_writes(file).map_err(io)?;
        #[cfg(unix)] {
            use std::os::unix::fs::MetadataExt;
            // Atomic publication can unlink an old record after a reader has
            // opened it. Its private, owned inode remains a valid snapshot.
            // Locks/writers must still refer to one live link; extra links
            // remain forbidden for every handle.
            if meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0
                || meta.nlink() > 1 || (create && meta.nlink() != 1) {
                return Err(io("Bridge journal file has unsafe ownership or permissions."));
            }
        }
        #[cfg(windows)] {
            use std::os::windows::fs::MetadataExt;
            if meta.file_attributes() & 0x400 != 0 { return Err(io("Bridge journal cannot be a reparse point.")); }
            crate::windows_security::validate_private_handle(file, &self.principal)?;
        }
        Ok(())
    }
    pub fn lock(&self) -> Result<File, BridgeError> {
        let file = self.open_file("operation.lock", true)?;
        file.try_lock().map_err(|_| BridgeError::new("EBUSY", "Another bridge operation is running."))?;
        Ok(file)
    }
    /// Only short journal reads/writes belong under this fence, never native
    /// calls. Cancel releases it before attempting the main mutation lock.
    pub fn cancellation_lock(&self) -> Result<File, BridgeError> {
        let file = self.open_file("cancellation.lock", true)?;
        file.lock().map_err(io)?;
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
        self.read_named("operation.json")
    }
    pub fn read_cancellation<T: DeserializeOwned>(&self) -> Result<Option<T>, BridgeError> {
        self.read_named("cancellation.json")
    }
    fn read_named<T: DeserializeOwned>(&self, name: &str) -> Result<Option<T>, BridgeError> {
        match fs::symlink_metadata(self.root.join(name)) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(io(e)),
            Ok(_) => (),
        }
        let file = self.open_file(name, false)?;
        if file.metadata().map_err(io)?.len() > 4 * 1024 * 1024 { return Err(io("Bridge journal is too large.")); }
        let mut text = String::new(); file.take(4 * 1024 * 1024 + 1).read_to_string(&mut text).map_err(io)?;
        Ok(Some(serde_json::from_str(&text).map_err(io)?))
    }
    pub fn write<T: Serialize>(&self, value: &T) -> Result<(), BridgeError> {
        self.write_named("operation.json", value)
    }
    pub fn write_cancellation<T: Serialize>(&self, value: &T) -> Result<(), BridgeError> {
        self.write_named("cancellation.json", value)
    }
    fn write_named<T: Serialize>(&self, name: &str, value: &T) -> Result<(), BridgeError> {
        let mut temp = tempfile::NamedTempFile::new_in(&self.root).map_err(io)?;
        #[cfg(windows)]
        crate::windows_security::validate_private_handle(temp.as_file(), &self.principal)?;
        serde_json::to_writer(&mut temp, value).map_err(io)?;
        temp.flush().map_err(io)?; temp.as_file().sync_all().map_err(io)?;
        #[cfg(windows)] {
            // Native status readers can briefly prevent replacement even when
            // their handles permit delete sharing. Keep the flushed candidate
            // intact and retry only transient Windows access/sharing errors;
            // never truncate the current durable recovery record.
            let until = std::time::Instant::now() + std::time::Duration::from_secs(1);
            loop {
                match temp.persist(self.root.join(name)) {
                    Ok(_) => break,
                    Err(error) if matches!(error.error.raw_os_error(), Some(5 | 32 | 33))
                        && std::time::Instant::now() < until => {
                        temp = error.file;
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                    Err(error) => return Err(io(error)),
                }
            }
        }
        #[cfg(not(windows))]
        temp.persist(self.root.join(name)).map_err(io)?;
        #[cfg(unix)] File::open(&self.root).and_then(|f| f.sync_all()).map_err(io)?;
        Ok(())
    }
}

#[cfg(all(test, unix))]
mod unix_tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;

    #[test]
    fn an_atomic_replacement_does_not_invalidate_an_open_read_snapshot() {
        let root = tempfile::tempdir().unwrap();
        let journal = Journal::open(root.path()).unwrap();
        journal.write(&41u64).unwrap();
        // Reproduce replacement between open() and validation of that handle.
        let mut previous = File::open(root.path().join("operation.json")).unwrap();
        journal.write(&42u64).unwrap();
        assert_eq!(previous.metadata().unwrap().nlink(), 0);
        journal.validate_file(&previous, false).expect("an immutable old read snapshot remains safe");
        let mut text = String::new();
        previous.read_to_string(&mut text).unwrap();
        assert_eq!(text, "41");
        assert_eq!(journal.read::<u64>().unwrap(), Some(42));
        // Writable lock handles must still name a single live inode.
        assert!(journal.validate_file(&previous, true).is_err());
        fs::hard_link(root.path().join("operation.json"), root.path().join("alias.json")).unwrap();
        assert!(journal.read::<u64>().is_err(), "hard-linked records remain forbidden");
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;
    use std::{io::{BufRead, BufReader}, process::{Command, Stdio}, time::Duration};

    #[test]
    fn transaction_lock_excludes_other_handles_in_the_same_process() {
        let root = tempfile::tempdir().unwrap();
        let journal = Journal::open(root.path()).unwrap();
        let held = journal.lock().unwrap();
        let other = Journal::open(root.path()).unwrap();
        assert_eq!(other.lock().unwrap_err().code, "EBUSY");
        std::thread::scope(|scope| {
            scope.spawn(|| assert_eq!(other.lock().unwrap_err().code, "EBUSY")).join().unwrap();
        });
        drop(held);
        assert!(other.lock().is_ok());
    }

    #[test]
    fn concurrent_status_reads_do_not_interrupt_atomic_publication() {
        use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
        let root = tempfile::tempdir().unwrap();
        let writer = Journal::open(root.path()).unwrap();
        writer.write(&0u64).unwrap();
        let done = Arc::new(AtomicBool::new(false));
        std::thread::scope(|scope| {
            let finished = done.clone();
            let reader = Journal::open(root.path()).unwrap();
            let reads = scope.spawn(move || {
                while !finished.load(Ordering::Acquire) {
                    assert!(reader.read::<u64>().unwrap().unwrap() <= 100);
                }
            });
            let writes = (1..=100u64).try_for_each(|value| writer.write(&value));
            done.store(true, Ordering::Release);
            reads.join().unwrap();
            writes.unwrap();
        });
        assert_eq!(writer.read::<u64>().unwrap(), Some(100));
    }

    #[test]
    fn publication_retries_until_a_conflicting_reader_closes() {
        use std::os::windows::fs::OpenOptionsExt;
        let root = tempfile::tempdir().unwrap();
        let journal = Journal::open(root.path()).unwrap();
        journal.write(&1u64).unwrap();
        let held = OpenOptions::new().read(true).share_mode(1)
            .open(root.path().join("operation.json")).unwrap();
        std::thread::scope(|scope| {
            scope.spawn(move || {
                std::thread::sleep(Duration::from_millis(100));
                drop(held);
            });
            journal.write(&2u64).unwrap();
        });
        assert_eq!(journal.read::<u64>().unwrap(), Some(2));
    }

    #[test]
    fn persistent_publication_conflict_preserves_the_recovery_record() {
        use std::os::windows::fs::OpenOptionsExt;
        let root = tempfile::tempdir().unwrap();
        let journal = Journal::open(root.path()).unwrap();
        journal.write_cancellation(&1u64).unwrap();
        let held = OpenOptions::new().read(true).share_mode(1)
            .open(root.path().join("cancellation.json")).unwrap();
        assert_eq!(journal.write_cancellation(&2u64).unwrap_err().code, "EJOURNAL");
        assert_eq!(journal.read_cancellation::<u64>().unwrap(), Some(1));
        drop(held);
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1, "failed candidate was not removed");
        journal.write_cancellation(&3u64).unwrap();
        assert_eq!(journal.read_cancellation::<u64>().unwrap(), Some(3));
    }

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
