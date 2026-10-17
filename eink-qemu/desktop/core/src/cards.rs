//! Exclusive use of SD card images among every eink-qemu process of this
//! user (desktop windows and command-line runs): lock files named by a
//! card's path and by its file identity, in one directory per user.

use crate::session::AppError;
use std::path::{Path, PathBuf};
use std::sync::Mutex as StdMutex;

/// Locks that give this process exclusive use of a host file. Held, never read.
pub struct Reservation {
    #[allow(dead_code)]
    pub(crate) path: std::fs::File,
    /// The current identity's key and lock; replaced when the file is.
    pub(crate) identity: Option<(String, std::fs::File)>,
}

/// The open SD card and the reservation that makes this process its only user.
pub struct Card {
    pub path: PathBuf,
    pub lock: StdMutex<Reservation>,
}

/// The lock directory the desktop app uses (its data directory's `locks`),
/// so a card has one owner across the app and the command-line tool. The
/// path follows Tauri's app_data_dir for the app's identifier.
pub fn default_locks_dir() -> PathBuf {
    const ID: &str = "net.serialx.eink-qemu";
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let data = if cfg!(target_os = "macos") {
        home.map(|h| h.join("Library").join("Application Support"))
    } else if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).filter(|p| p.is_absolute())
            .or_else(|| home.map(|h| h.join(".local").join("share")))
    };
    data.map(|d| d.join(ID)).unwrap_or_else(|| std::env::temp_dir().join("eink-qemu")).join("locks")
}

/// A stable name for a card path, also before the file exists.
pub fn card_key(path: &Path) -> PathBuf {
    if let Ok(path) = dunce::canonicalize(path) {
        return path;
    }
    match (path.parent().and_then(|p| dunce::canonicalize(p).ok()), path.file_name()) {
        (Some(parent), Some(name)) => parent.join(name),
        _ => path.to_path_buf(),
    }
}

/// The lock file for one key (a path or a file identity).
pub fn lock_file(locks: &Path, key: &str) -> Result<std::fs::File, AppError> {
    std::fs::create_dir_all(locks)?;
    // FNV-1a: a fixed hash, so every build names a key's lock file alike.
    let hash = key.bytes().fold(0xcbf2_9ce4_8422_2325u64, |hash, byte| (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3));
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(locks.join(format!("{hash:016x}.lock")))?;
    Ok(file)
}

/// A file's identity, so hard links to one card share one owner.
pub fn identity_key(path: &Path) -> Option<String> {
    crate::host::file_identity(path).map(|(device, file)| format!("id:{device}:{file}"))
}

/// Exclusive use of a host file among all eink-qemu instances of this user:
/// a lock on its path (also before it exists) and, when it exists, on its
/// identity. Cards hold one while open; writes to a host file hold one for
/// the whole write, so they can never land on an open card.
pub fn reserve(locks: &Path, path: &Path) -> Result<Reservation, AppError> {
    let mut reservation = reserve_path(locks, path)?;
    reservation.refresh(locks, path)?;
    Ok(reservation)
}

/// Only the path lock: for a hard link to the card this instance already
/// owns, whose identity lock it holds.
pub fn reserve_path(locks: &Path, path: &Path) -> Result<Reservation, AppError> {
    let mut key = format!("path:{}", card_key(path).display());
    if cfg!(windows) {
        // NTFS names ignore case, so C:\Card.img and c:\card.img are one card.
        key = key.to_lowercase();
    }
    let file = try_lock(locks, &key, path)?;
    Ok(Reservation { path: file, identity: None })
}

pub fn try_lock(locks: &Path, key: &str, path: &Path) -> Result<std::fs::File, AppError> {
    let file = lock_file(locks, key)?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(std::fs::TryLockError::WouldBlock) => Err(AppError::new(
            "EBUSY",
            format!("{} is an SD card open in eink-qemu.", path.display()),
        )),
        Err(std::fs::TryLockError::Error(error)) => Err(error.into()),
    }
}

impl Reservation {
    /// Lock the file's current identity (after it was created or replaced),
    /// releasing the identity of a file it replaced.
    pub fn refresh(&mut self, locks: &Path, path: &Path) -> Result<(), AppError> {
        let Some(id) = identity_key(path) else { return Ok(()) };
        if self.identity.as_ref().is_some_and(|(held, _)| *held == id) {
            return Ok(());
        }
        let file = try_lock(locks, &id, path)?;
        self.identity = Some((id, file));
        Ok(())
    }
}

/// Check that `path` looks like an SD image and return its canonical form.
pub fn validate_card(path: &Path) -> Result<PathBuf, AppError> {
    let path = dunce::canonicalize(path)
        .map_err(|_| AppError::new("ENOENT", format!("{} does not exist.", path.display())))?;
    let meta = std::fs::metadata(&path)?;
    if !meta.is_file() {
        return Err(AppError::new("EINVAL", "Choose an SD image file."));
    }
    if meta.len() == 0 || meta.len() % 512 != 0 {
        return Err(AppError::new("EINVAL", "SD image must contain a whole number of 512-byte sectors."));
    }
    Ok(path)
}

pub fn open_card(locks: &Path, path: &Path) -> Result<Card, AppError> {
    let path = validate_card(path)?;
    let lock = reserve(locks, &path)?;
    Ok(Card { path, lock: lock.into() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reserved_card_cannot_be_reserved_again() {
        let dir = tempfile::tempdir().unwrap();
        let locks = dir.path().join("locks");
        let card = dir.path().join("card.img");
        std::fs::write(&card, vec![0u8; 1024]).unwrap();
        let first = reserve(&locks, &card).unwrap();
        assert_eq!(reserve(&locks, &card).err().unwrap().code, "EBUSY");
        drop(first);
        assert!(reserve(&locks, &card).is_ok());
    }

    #[test]
    fn hard_links_share_one_owner() {
        let dir = tempfile::tempdir().unwrap();
        let locks = dir.path().join("locks");
        let card = dir.path().join("card.img");
        let alias = dir.path().join("alias.img");
        std::fs::write(&card, vec![0u8; 1024]).unwrap();
        std::fs::hard_link(&card, &alias).unwrap();
        let _owner = reserve(&locks, &card).unwrap();
        assert_eq!(reserve(&locks, &alias).err().unwrap().code, "EBUSY");
    }

    #[test]
    fn a_replaced_card_releases_its_old_identity() {
        let dir = tempfile::tempdir().unwrap();
        let locks = dir.path().join("locks");
        let card = dir.path().join("card.img");
        let old_alias = dir.path().join("old.img");
        std::fs::write(&card, vec![0u8; 1024]).unwrap();
        std::fs::hard_link(&card, &old_alias).unwrap();
        let mut owner = reserve(&locks, &card).unwrap();
        // Replace the card with a new file, as a blank card does.
        let fresh = dir.path().join("fresh.tmp");
        std::fs::write(&fresh, vec![1u8; 1024]).unwrap();
        std::fs::rename(&fresh, &card).unwrap();
        owner.refresh(&locks, &card).unwrap();
        assert!(reserve(&locks, &old_alias).is_ok(), "the replaced file is no longer the card");
        assert_eq!(reserve(&locks, &card).err().unwrap().code, "EBUSY");
    }

    #[test]
    fn a_path_is_reserved_before_it_exists() {
        let dir = tempfile::tempdir().unwrap();
        let locks = dir.path().join("locks");
        let card = dir.path().join("new.img");
        let _creator = reserve(&locks, &card).unwrap();
        assert_eq!(reserve(&locks, &card).err().unwrap().code, "EBUSY");
    }
}
