//! Exact, idempotent profile transitions used by apply and durable restoration.
//! A missing or edited original profile is a conflict, never permission to
//! recreate somebody else's configuration from an old snapshot.
use crate::{linux_network_manager::NetworkManager, linux_profiles::{Profile, same_configuration}, model::BridgeError};
use serde::{Deserialize, Serialize};
fn conflict() -> BridgeError { BridgeError::new("ECONFLICT", "A network profile changed outside the app. Its changes have been preserved.") }
fn identity(profile: &Profile) -> Result<&str, BridgeError> {
    let id = profile.get("connection").and_then(|c| c.get("uuid")).and_then(|v| <&str>::try_from(v).ok()).ok_or_else(conflict)?;
    if id.len() != 36 || !id.bytes().enumerate().all(|(i, b)| if [8, 13, 18, 23].contains(&i) { b == b'-' } else { b.is_ascii_hexdigit() }) { return Err(conflict()); }
    Ok(id)
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileAction { Unchanged, Create, Update, Delete }
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileChange { before: Option<Profile>, after: Option<Profile> }
impl ProfileChange {
    pub fn new(before: Option<Profile>, after: Option<Profile>) -> Result<Self, BridgeError> {
        let change = Self { before, after };
        change.identity()?;
        Ok(change)
    }
    pub fn identity(&self) -> Result<&str, BridgeError> {
        let before = self.before.as_ref().map(identity).transpose()?;
        let after = self.after.as_ref().map(identity).transpose()?;
        if before.is_some() && after.is_some() && before != after { return Err(conflict()); }
        before.or(after).ok_or_else(conflict)
    }
    pub fn reverse(&self) -> Self { Self { before: self.after.clone(), after: self.before.clone() } }
    pub fn action(&self, current: Option<&Profile>) -> Result<ProfileAction, BridgeError> {
        self.identity()?;
        let same = |expected: Option<&Profile>| match (expected, current) {
            (None, None) => true,
            (Some(expected), Some(current)) => same_configuration(expected, current),
            _ => false,
        };
        if same(self.after.as_ref()) { return Ok(ProfileAction::Unchanged); }
        if !same(self.before.as_ref()) { return Err(conflict()); }
        Ok(match (&self.before, &self.after) {
            (None, Some(_)) => ProfileAction::Create,
            (Some(_), Some(_)) => ProfileAction::Update,
            (Some(_), None) => ProfileAction::Delete,
            (None, None) => return Err(conflict()),
        })
    }
    pub fn check(&self, nm: &NetworkManager) -> Result<(), BridgeError> {
        let current = nm.find_profile(self.identity()?)?;
        self.action(current.as_ref().map(|(_, profile)| profile)).map(|_| ())
    }
    pub fn apply(&self, nm: &NetworkManager) -> Result<(), BridgeError> {
        let current = nm.find_profile(self.identity()?)?;
        match self.action(current.as_ref().map(|(_, profile)| profile))? {
            ProfileAction::Unchanged => Ok(()),
            ProfileAction::Create => nm.add_profile(self.after.as_ref().unwrap()).map(|_| ()),
            ProfileAction::Update => nm.update_profile(self.before.as_ref().unwrap(), self.after.as_ref().unwrap()),
            ProfileAction::Delete => nm.delete_profile(self.before.as_ref().unwrap()),
        }
    }
}

/// A checkpoint path is meaningful only within its creating host/bus-owner
/// lifetime. A new service may allocate the same path for an unrelated request.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Checkpoint { epoch: String, path: String }
impl Checkpoint {
    pub fn record(epoch: String, path: String) -> Result<Self, BridgeError> {
        let suffix = path.strip_prefix("/org/freedesktop/NetworkManager/Checkpoint/").ok_or_else(conflict)?;
        if epoch.is_empty() || epoch.len() > 128 || suffix.is_empty() || !suffix.bytes().all(|b| b.is_ascii_digit()) { return Err(conflict()); }
        Ok(Self { epoch, path })
    }
    pub fn belongs_to(&self, epoch: &str) -> bool { self.epoch == epoch }
    pub fn create(nm: &NetworkManager, device: &zbus::zvariant::OwnedObjectPath) -> Result<Self, BridgeError> {
        let epoch = nm.epoch()?;
        let path = nm.checkpoint_in_epoch(&epoch, device, 300)?;
        if nm.epoch()? != epoch { return Err(BridgeError::new("ESTALE", "NetworkManager restarted during bridge setup. Review the connection again.")); }
        Self::record(epoch, path.to_string())
    }
    fn path(&self) -> Result<zbus::zvariant::OwnedObjectPath, BridgeError> {
        Self::record(self.epoch.clone(), self.path.clone())?;
        self.path.clone().try_into().map_err(|_| conflict())
    }
    pub fn exists(&self, nm: &NetworkManager) -> Result<bool, BridgeError> {
        Ok(self.belongs_to(&nm.epoch()?) && nm.checkpoints()?.contains(&self.path()?))
    }
    pub fn rollback(&self, nm: &NetworkManager) -> Result<(), BridgeError> {
        if self.exists(nm)? { nm.rollback_checkpoint_in_epoch(&self.epoch, &self.path()?)?; }
        self.disarm(nm)
    }
    pub fn disarm(&self, nm: &NetworkManager) -> Result<(), BridgeError> {
        if self.exists(nm)? { nm.commit_checkpoint_in_epoch(&self.epoch, &self.path()?)?; }
        Ok(())
    }
    pub fn confirm_window(&self, nm: &NetworkManager) -> Result<(), BridgeError> {
        if !self.exists(nm)? { return Err(BridgeError::new("ESTALE", "NetworkManager restored or replaced this setup checkpoint.")); }
        nm.extend_checkpoint_in_epoch(&self.epoch, &self.path()?, 130)
    }
}
