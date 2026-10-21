//! Native NetworkManager D-Bus access with bounded calls. Paths come only from
//! the system bus; profile identities and expected content come from the journal.
use crate::{linux_profiles::{Profile, Settings, same_configuration}, model::BridgeError};
use std::time::Duration;
use zbus::{blocking::{Connection, Proxy, connection::Builder}, zvariant::OwnedObjectPath};
const SERVICE: &str = "org.freedesktop.NetworkManager";
const ROOT: &str = "/org/freedesktop/NetworkManager";
const SETTINGS: &str = "/org/freedesktop/NetworkManager/Settings";
fn error(value: impl std::fmt::Display) -> BridgeError { BridgeError { code: "ENETWORKMANAGER".into(), message: "NetworkManager could not complete the bridge operation.".into(), detail: Some(value.to_string()) } }
fn conflict() -> BridgeError { BridgeError::new("ECONFLICT", "This network profile changed outside the app. Its changes have been preserved.") }
fn root() -> Result<(), BridgeError> { if unsafe { libc::geteuid() } != 0 { Err(BridgeError::new("EAUTH", "The authorized bridge service must perform network changes.")) } else { Ok(()) } }
fn uuid(profile: &Profile) -> Result<&str, BridgeError> {
    profile.get("connection").and_then(|c| c.get("uuid")).and_then(|v| <&str>::try_from(v).ok()).ok_or_else(|| BridgeError::new("EPROFILE", "The network profile has no stable identity."))
}
pub struct ActiveProfile { pub device: OwnedObjectPath, pub path: OwnedObjectPath, pub profile: Profile }
pub struct NetworkManager { connection: Connection }
impl NetworkManager {
    pub fn connect() -> Result<Self, BridgeError> {
        Ok(Self { connection: Builder::system().map_err(error)?.method_timeout(Duration::from_secs(15)).build().map_err(error)? })
    }
    fn proxy(&self, path: &str, interface: &str) -> Result<Proxy<'_>, BridgeError> {
        Proxy::new(&self.connection, SERVICE, path.to_owned(), interface.to_owned()).map_err(error)
    }
    fn manager(&self) -> Result<Proxy<'_>, BridgeError> { self.proxy(ROOT, SERVICE) }
    fn profile_proxy(&self, path: &OwnedObjectPath) -> Result<Proxy<'_>, BridgeError> { self.proxy(path.as_str(), "org.freedesktop.NetworkManager.Settings.Connection") }
    pub fn read_profile(&self, path: &OwnedObjectPath) -> Result<Profile, BridgeError> {
        self.profile_proxy(path)?.call::<_, _, Settings>("GetSettings", &()).map(Profile).map_err(error)
    }
    pub fn active_profile(&self, interface: &str) -> Result<ActiveProfile, BridgeError> {
        let device: OwnedObjectPath = self.manager()?.call("GetDeviceByIpIface", &(interface,)).map_err(error)?;
        let proxy = self.proxy(device.as_str(), "org.freedesktop.NetworkManager.Device")?;
        if proxy.get_property::<u32>("DeviceType").map_err(error)? != 1 || !proxy.get_property::<bool>("Managed").map_err(error)? {
            return Err(BridgeError::new("EUNSUPPORTED", "Select an Ethernet connection managed by NetworkManager."));
        }
        let active: OwnedObjectPath = proxy.get_property("ActiveConnection").map_err(error)?;
        if active.as_str() == "/" { return Err(BridgeError::new("EDISCONNECTED", "Connect this Ethernet adapter before bridge setup.")); }
        let proxy = self.proxy(active.as_str(), "org.freedesktop.NetworkManager.Connection.Active")?;
        let path: OwnedObjectPath = proxy.get_property("Connection").map_err(error)?;
        let profile = self.read_profile(&path)?;
        Ok(ActiveProfile { device, path, profile })
    }
    pub fn find_profile(&self, identity: &str) -> Result<Option<(OwnedObjectPath, Profile)>, BridgeError> {
        let paths: Vec<OwnedObjectPath> = self.proxy(SETTINGS, "org.freedesktop.NetworkManager.Settings")?.call("ListConnections", &()).map_err(error)?;
        let mut found = None;
        for path in paths {
            let profile = self.read_profile(&path)?;
            if uuid(&profile)? == identity {
                if found.is_some() { return Err(conflict()); }
                found = Some((path, profile));
            }
        }
        Ok(found)
    }
    pub fn add_profile(&self, profile: &Profile) -> Result<OwnedObjectPath, BridgeError> {
        root()?;
        if self.find_profile(uuid(profile)?)?.is_some() { return Err(conflict()); }
        self.proxy(SETTINGS, "org.freedesktop.NetworkManager.Settings")?.call("AddConnection", &(&profile.0,)).map_err(error)
    }
    pub fn update_profile(&self, expected: &Profile, replacement: &Profile) -> Result<(), BridgeError> {
        root()?;
        if uuid(expected)? != uuid(replacement)? { return Err(conflict()); }
        let (path, current) = self.find_profile(uuid(expected)?)?.ok_or_else(conflict)?;
        if !same_configuration(expected, &current) { return Err(conflict()); }
        self.profile_proxy(&path)?.call("Update", &(&replacement.0,)).map_err(error)
    }
    pub fn delete_profile(&self, expected: &Profile) -> Result<(), BridgeError> {
        root()?;
        let Some((path, current)) = self.find_profile(uuid(expected)?)? else { return Ok(()); };
        if !same_configuration(expected, &current) { return Err(conflict()); }
        self.profile_proxy(&path)?.call("Delete", &()).map_err(error)
    }
    pub fn checkpoint(&self, device: &OwnedObjectPath, seconds: u32) -> Result<OwnedObjectPath, BridgeError> {
        root()?;
        // DELETE_NEW_CONNECTIONS also deletes unrelated profiles added by
        // other clients. Restore the device here; the durable journal removes
        // only the exact app-owned profiles, with content conflict checks.
        self.manager()?.call("CheckpointCreate", &(vec![device], seconds, 0u32)).map_err(error)
    }
    pub fn checkpoints(&self) -> Result<Vec<OwnedObjectPath>, BridgeError> { self.manager()?.get_property("Checkpoints").map_err(error) }
    pub fn rollback_checkpoint(&self, path: &OwnedObjectPath) -> Result<(), BridgeError> {
        root()?;
        let result: std::collections::HashMap<String, u32> = self.manager()?.call("CheckpointRollback", &(path,)).map_err(error)?;
        if result.is_empty() || result.values().any(|status| *status != 0) { return Err(BridgeError::new("EROLLBACK", "NetworkManager could not fully restore the previous connection.")); }
        Ok(())
    }
    pub fn commit_checkpoint(&self, path: &OwnedObjectPath) -> Result<(), BridgeError> { root()?; self.manager()?.call("CheckpointDestroy", &(path,)).map_err(error) }
    pub fn extend_checkpoint(&self, path: &OwnedObjectPath, seconds: u32) -> Result<(), BridgeError> { root()?; self.manager()?.call("CheckpointAdjustRollbackTimeout", &(path, seconds)).map_err(error) }
    pub fn activate(&self, path: &OwnedObjectPath, device: &OwnedObjectPath) -> Result<OwnedObjectPath, BridgeError> {
        root()?;
        let any = OwnedObjectPath::try_from("/").unwrap();
        self.manager()?.call("ActivateConnection", &(path, device, &any)).map_err(error)
    }
    pub fn wait_active(&self, active: &OwnedObjectPath) -> Result<(), BridgeError> {
        let proxy = self.proxy(active.as_str(), "org.freedesktop.NetworkManager.Connection.Active")?;
        let deadline = std::time::Instant::now() + Duration::from_secs(40);
        loop {
            match proxy.get_property::<u32>("State").map_err(error)? {
                2 => return Ok(()),
                3 | 4 => return Err(BridgeError::new("ECONNECTION", "The bridge connection could not be activated.")),
                _ if std::time::Instant::now() >= deadline => return Err(BridgeError::new("ETIMEOUT", "The bridge connection did not become ready in time.")),
                _ => std::thread::sleep(Duration::from_millis(100)),
            }
        }
    }
}
