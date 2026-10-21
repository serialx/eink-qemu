//! Fixed app-specific packet-helper policy. Changes capture exact prior bytes
//! so recovery never overwrites a policy edited outside the transaction.
use crate::model::BridgeError;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, fs::{self, File, OpenOptions}, io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt}, path::Path};
pub const HELPER: &str = "/usr/lib/eink-qemu/bridge/eink-qemu-bridge-helper";
pub const POLICY: &str = "/etc/eink-qemu/bridge/bridge.conf";
const MAX_POLICY: usize = 65536;
fn invalid() -> BridgeError { BridgeError::new("EPOLICY", "Bridge access policy is invalid. Preserve it and repair bridge support.") }
fn io(error: impl std::fmt::Display) -> BridgeError { BridgeError::new("EHELPER", error.to_string()) }
fn conflict() -> BridgeError { BridgeError::new("ECONFLICT", "Bridge access policy changed outside the app. Its changes have been preserved.") }

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Grant { uid: u32, bridge: String }
impl Grant {
    pub fn new(uid: u32, bridge: &str) -> Result<Self, BridgeError> {
        if uid == 0 || bridge.is_empty() || bridge == "all" || bridge.len() >= 16
            || !bridge.bytes().all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c)) { return Err(invalid()); }
        Ok(Self { uid, bridge: bridge.into() })
    }
    pub fn uid(&self) -> u32 { self.uid }
    pub fn bridge(&self) -> &str { &self.bridge }
}
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Policy(BTreeSet<Grant>);
impl Policy {
    pub fn parse(bytes: &[u8]) -> Result<Self, BridgeError> {
        if bytes.len() > MAX_POLICY || bytes.contains(&0) || (!bytes.is_empty() && !bytes.ends_with(b"\n")) { return Err(invalid()); }
        let text = std::str::from_utf8(bytes).map_err(|_| invalid())?;
        let mut policy = Self::default();
        for line in text.split_terminator('\n') {
            if line.len() > 127 { return Err(invalid()); }
            let line = line.trim_start_matches([' ', '\t']);
            if line.is_empty() || line.starts_with('#') { continue; }
            let parts = line.split_ascii_whitespace().collect::<Vec<_>>();
            if parts.len() != 3 || parts[0] != "allow" || !parts[1].bytes().all(|c| c.is_ascii_digit()) { return Err(invalid()); }
            policy.insert(Grant::new(parts[1].parse().map_err(|_| invalid())?, parts[2])?);
        }
        Ok(policy)
    }
    pub fn contains(&self, grant: &Grant) -> bool { self.0.contains(grant) }
    pub fn insert(&mut self, grant: Grant) { self.0.insert(grant); }
    pub fn remove(&mut self, grant: &Grant) { self.0.remove(grant); }
    pub fn encode(&self) -> Vec<u8> {
        let mut text = String::from("# eink-qemu bridge access: exact local user and interface\n");
        for grant in &self.0 { text.push_str(&format!("allow {} {}\n", grant.uid, grant.bridge)); }
        text.into_bytes()
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccessChange { before: Option<Vec<u8>>, after: Option<Vec<u8>> }
fn protected(path: &Path) -> Result<(), BridgeError> {
    let parent = path.parent().ok_or_else(invalid)?;
    crate::transport::unix::validate_system_directory(parent)?;
    for prefix in parent.ancestors() {
        if fs::symlink_metadata(prefix).map_err(io)?.file_type().is_symlink() { return Err(invalid()); }
    }
    Ok(())
}
fn read_policy(path: &Path, owner: u32) -> Result<Option<Vec<u8>>, BridgeError> {
    let file = match OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io(error)),
    };
    let meta = file.metadata().map_err(io)?;
    if !meta.is_file() || meta.uid() != owner || meta.mode() & 0o022 != 0 || meta.nlink() != 1 || meta.len() > MAX_POLICY as u64 { return Err(invalid()); }
    let mut bytes = Vec::new();
    file.take(MAX_POLICY as u64 + 1).read_to_end(&mut bytes).map_err(io)?;
    Policy::parse(&bytes)?;
    Ok(Some(bytes))
}
fn replace_policy(path: &Path, expected: &Option<Vec<u8>>, next: &Option<Vec<u8>>, owner: u32) -> Result<(), BridgeError> {
    let current = read_policy(path, owner)?;
    if &current == next { return Ok(()); }
    if &current != expected { return Err(conflict()); }
    if let Some(bytes) = next {
        Policy::parse(bytes)?;
        let mut temp = tempfile::NamedTempFile::new_in(path.parent().ok_or_else(invalid)?).map_err(io)?;
        temp.as_file().set_permissions(fs::Permissions::from_mode(0o644)).map_err(io)?;
        temp.write_all(bytes).map_err(io)?;
        temp.as_file().sync_all().map_err(io)?;
        temp.persist(path).map_err(io)?;
    } else if current.is_some() { fs::remove_file(path).map_err(io)?; }
    File::open(path.parent().ok_or_else(invalid)?).and_then(|file| file.sync_all()).map_err(io)
}
pub fn check_helper() -> Result<(), BridgeError> {
    let path = Path::new(HELPER);
    protected(path)?;
    let file = OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK).open(path).map_err(io)?;
    let meta = file.metadata().map_err(io)?;
    if !meta.is_file() || meta.uid() != 0 || meta.nlink() != 1 || meta.mode() & 0o7777 != 0o4755 { return Err(BridgeError::new("EHELPER", "Reinstall the packaged bridge helper before setup.")); }
    Ok(())
}
pub fn prepare(grant: &Grant, enable: bool) -> Result<AccessChange, BridgeError> {
    Grant::new(grant.uid, &grant.bridge)?;
    check_helper()?;
    protected(Path::new(POLICY))?;
    let before = read_policy(Path::new(POLICY), 0)?;
    let mut policy = Policy::parse(before.as_deref().unwrap_or_default())?;
    if enable { policy.insert(grant.clone()); } else { policy.remove(grant); }
    let after = Some(policy.encode());
    Policy::parse(after.as_deref().unwrap())?;
    Ok(AccessChange { before, after })
}
pub fn apply(change: &AccessChange) -> Result<(), BridgeError> {
    if unsafe { libc::geteuid() } != 0 { return Err(invalid()); }
    protected(Path::new(POLICY))?;
    replace_policy(Path::new(POLICY), &change.before, &change.after, 0)
}
pub fn restore(change: &AccessChange) -> Result<(), BridgeError> {
    if unsafe { libc::geteuid() } != 0 { return Err(invalid()); }
    protected(Path::new(POLICY))?;
    replace_policy(Path::new(POLICY), &change.after, &change.before, 0)
}
pub fn verify_grant(grant: &Grant) -> Result<(), BridgeError> {
    Grant::new(grant.uid, &grant.bridge)?;
    check_helper()?;
    protected(Path::new(POLICY))?;
    let bytes = read_policy(Path::new(POLICY), 0)?.ok_or_else(invalid)?;
    if !Policy::parse(&bytes)?.contains(grant) { return Err(BridgeError::new("EACCESS", "This user no longer has access to the selected bridge.")); }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn policy_restoration_is_idempotent_and_preserves_external_edits() {
        let temp = tempfile::tempdir().unwrap(); let path = temp.path().join("policy");
        let owner = unsafe { libc::geteuid() };
        let old = Some(b"allow 1001 br-existing\n".to_vec());
        let next = Some(b"allow 1000 ebr123\nallow 1001 br-existing\n".to_vec());
        replace_policy(&path, &None, &old, owner).unwrap();
        replace_policy(&path, &old, &next, owner).unwrap();
        replace_policy(&path, &old, &next, owner).unwrap();
        let edited = b"allow 1002 someone-else\n";
        fs::write(&path, edited).unwrap();
        assert_eq!(replace_policy(&path, &next, &old, owner).unwrap_err().code, "ECONFLICT");
        assert_eq!(fs::read(&path).unwrap(), edited);
        fs::write(&path, next.as_ref().unwrap()).unwrap();
        replace_policy(&path, &next, &old, owner).unwrap();
        replace_policy(&path, &next, &old, owner).unwrap();
        assert_eq!(read_policy(&path, owner).unwrap(), old);
        replace_policy(&path, &old, &None, owner).unwrap();
        assert!(!path.exists());
    }
}
