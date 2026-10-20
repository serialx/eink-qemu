//! Atomic records in a caller-supplied, installer-controlled directory.
//! The privileged entry point must use its fixed installation path, never a
//! path from IPC. Ownership here is filesystem ownership, not the IPC caller.
use crate::model::BridgeError;
use serde::{Serialize, de::DeserializeOwned};
use std::{fs::{self, File, OpenOptions}, io::{Read, Write}, path::{Path, PathBuf}};

pub(crate) struct Journal { root: PathBuf }
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
        Ok(Self { root: fs::canonicalize(root).map_err(io)? })
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
        #[cfg(unix)] {
            use std::os::unix::fs::MetadataExt;
            if meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 || meta.nlink() != 1 {
                return Err(io("Bridge journal file has unsafe ownership or permissions."));
            }
        }
        #[cfg(windows)] {
            use std::os::windows::fs::MetadataExt;
            if meta.file_attributes() & 0x400 != 0 { return Err(io("Bridge journal cannot be a reparse point.")); }
        }
        Ok(file)
    }
    pub fn lock(&self) -> Result<File, BridgeError> {
        let file = self.open_file("operation.lock", true)?;
        file.try_lock().map_err(|_| BridgeError::new("EBUSY", "Another bridge operation is running."))?;
        Ok(file)
    }
    pub fn resource_lock(&self, id: &crate::model::ResourceId, shared: bool) -> Result<File, BridgeError> {
        let file = self.open_file(&format!("resource-{}.lock", id.as_str()), true)?;
        let result = if shared { file.try_lock_shared() } else { file.try_lock() };
        result.map_err(|_| BridgeError::new("EBUSY", "This bridge is in use. Stop its emulators first."))?;
        Ok(file)
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
        serde_json::to_writer(&mut temp, value).map_err(io)?;
        temp.flush().map_err(io)?; temp.as_file().sync_all().map_err(io)?;
        temp.persist(self.root.join("operation.json")).map_err(io)?;
        #[cfg(unix)] File::open(&self.root).and_then(|f| f.sync_all()).map_err(io)?;
        Ok(())
    }
}
