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

#[cfg(test)]
mod storage_tests {
    use super::*;
    #[test]
    fn generated_and_unsaved_profiles_cannot_be_converted() {
        let persistent = "/etc/NetworkManager/system-connections/wired.nmconnection";
        assert!(durable_profile(persistent, 0, false).is_ok());
        for filename in ["", "/run/NetworkManager/system-connections/netplan-eth0.nmconnection",
            "/etc/NetworkManager/system-connections/netplan-eth0.nmconnection",
            "/usr/lib/NetworkManager/system-connections/vendor.nmconnection", "/etc/sysconfig/network-scripts/ifcfg-eth0"] {
            assert_eq!(durable_profile(filename, 0, false).unwrap_err().code, "EUNSUPPORTED");
        }
        for flags in [1, 2, 4, 8, 16] { assert!(durable_profile(persistent, flags, false).is_err()); }
        assert!(durable_profile(persistent, 0, true).is_err());
    }
}
pub struct ActiveProfile { pub device: OwnedObjectPath, pub path: OwnedObjectPath, pub profile: Profile }
pub struct NetworkManager { connection: Connection }

#[cfg(test)]
mod startup_tests {
    use super::*;
    use std::{io::{BufRead, BufReader}, process::{Child, Command, Stdio},
        sync::{Arc, atomic::{AtomicBool, Ordering}, mpsc}, time::Instant};

    struct Bus(Child);
    impl Drop for Bus { fn drop(&mut self) { let _ = self.0.kill(); let _ = self.0.wait(); } }
    struct Starting(Arc<AtomicBool>);
    #[zbus::interface(name = "org.freedesktop.NetworkManager")]
    impl Starting {
        #[zbus(property)]
        fn startup(&self) -> bool { self.0.load(Ordering::Acquire) }
    }
    fn fixture() -> (Bus, Connection, NetworkManager, Arc<AtomicBool>) {
        // Private bus only: no test changes the host's NetworkManager.
        let mut bus = Bus(Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .stdout(Stdio::piped()).spawn().unwrap());
        let mut address = String::new();
        BufReader::new(bus.0.stdout.take().unwrap()).read_line(&mut address).unwrap();
        let starting = Arc::new(AtomicBool::new(true));
        let service = Builder::address(address.trim()).unwrap().name(SERVICE).unwrap()
            .serve_at(ROOT, Starting(starting.clone())).unwrap().build().unwrap();
        let connection = Builder::address(address.trim()).unwrap()
            .method_timeout(Duration::from_secs(1)).build().unwrap();
        (bus, service, NetworkManager { connection }, starting)
    }
    #[test]
    fn recovery_waits_until_network_manager_finishes_startup() {
        let (_bus, _service, nm, starting) = fixture();
        let (send, receive) = mpsc::channel();
        let worker = std::thread::spawn(move || send.send(nm.wait_initialized(Duration::from_secs(2))).unwrap());
        assert!(matches!(receive.recv_timeout(Duration::from_millis(150)), Err(mpsc::RecvTimeoutError::Timeout)),
            "recovery must not begin while devices and profiles are still loading");
        starting.store(false, Ordering::Release);
        receive.recv_timeout(Duration::from_secs(2)).unwrap().unwrap();
        worker.join().unwrap();
    }
    #[test]
    fn network_manager_startup_wait_is_bounded() {
        let (_bus, _service, nm, _starting) = fixture();
        let start = Instant::now();
        assert_eq!(nm.wait_initialized(Duration::from_millis(150)).unwrap_err().code, "ENETWORKMANAGER");
        assert!(start.elapsed() < Duration::from_secs(2));
    }
    #[test]
    fn missing_network_manager_does_not_allow_recovery() {
        let (_bus, service, nm, _starting) = fixture();
        service.release_name(SERVICE).unwrap();
        assert_eq!(nm.wait_initialized(Duration::from_millis(150)).unwrap_err().code, "ENETWORKMANAGER");
    }
}

fn durable_profile(filename: &str, flags: u32, unsaved: bool) -> Result<(), BridgeError> {
    let path = std::path::Path::new(filename);
    if flags != 0 || unsaved || path.parent() != Some(std::path::Path::new("/etc/NetworkManager/system-connections"))
        || path.file_name().is_none()
        || path.file_name().and_then(|name| name.to_str()).is_some_and(|name| name.starts_with("netplan-")) {
        return Err(BridgeError::new("EUNSUPPORTED", "This connection is temporary or generated by another network manager. Select an existing bridge or a persistent NetworkManager connection."));
    }
    Ok(())
}
impl NetworkManager {
    /// Acquiring the bus name makes NetworkManager's systemd unit active before
    /// its devices and profiles are loaded. Recovery must not interpret that
    /// transient absence as a permanent rollback conflict. Existing unmanaged
    /// bridges never open this connection and do not need NetworkManager.
    pub fn wait_initialized(&self, timeout: Duration) -> Result<(), BridgeError> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            // Use a fresh proxy so every attempt reads current startup state.
            let result = self.manager().and_then(|proxy| proxy.get_property::<bool>("Startup").map_err(error));
            if matches!(result, Ok(false)) { return Ok(()); }
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return Err(result.err().unwrap_or_else(|| error("NetworkManager did not finish starting before bridge recovery.")));
            }
            std::thread::sleep(remaining.min(Duration::from_millis(100)));
        }
    }
    pub fn connect() -> Result<Self, BridgeError> {
        let manager = Self { connection: Builder::system().map_err(error)?.method_timeout(Duration::from_secs(15)).build().map_err(error)? };
        manager.wait_initialized(Duration::from_secs(60))?;
        Ok(manager)
    }
    fn proxy(&self, path: &str, interface: &str) -> Result<Proxy<'_>, BridgeError> {
        Proxy::new(&self.connection, SERVICE, path.to_owned(), interface.to_owned()).map_err(error)
    }
    fn manager(&self) -> Result<Proxy<'_>, BridgeError> { self.proxy(ROOT, SERVICE) }
    fn profile_proxy(&self, path: &OwnedObjectPath) -> Result<Proxy<'_>, BridgeError> { self.proxy(path.as_str(), "org.freedesktop.NetworkManager.Settings.Connection") }
    /// Generated /run profiles can be recreated at boot, undoing our saved
    /// autoconnect change. Only accept persistent, native keyfile profiles.
    pub fn require_durable_profile(&self, path: &OwnedObjectPath) -> Result<(), BridgeError> {
        let proxy = self.profile_proxy(path)?;
        durable_profile(&proxy.get_property::<String>("Filename").map_err(error)?,
            proxy.get_property("Flags").map_err(error)?, proxy.get_property("Unsaved").map_err(error)?)
    }
    pub fn device_path(&self, interface: &str) -> Result<OwnedObjectPath, BridgeError> {
        self.manager()?.call("GetDeviceByIpIface", &(interface,)).map_err(error)
    }
    /// Checkpoint object paths can be reused after NetworkManager or the host
    /// restarts. Bind stored paths to both lifetimes before using them again.
    pub fn epoch(&self) -> Result<String, BridgeError> {
        let bus = Proxy::new(&self.connection, "org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus").map_err(error)?;
        let owner: String = bus.call("GetNameOwner", &(SERVICE,)).map_err(error)?;
        let bus_id: String = bus.call("GetId", &()).map_err(error)?;
        let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").map_err(error)?;
        Ok(format!("{}|{bus_id}|{owner}", boot.trim()))
    }
    fn checkpoint_manager(&self, epoch: &str) -> Result<Proxy<'_>, BridgeError> {
        if self.epoch()? != epoch { return Err(BridgeError::new("ESTALE", "NetworkManager or the system bus restarted during bridge setup.")); }
        let owner = epoch.rsplit_once('|').map(|(_, owner)| owner).filter(|owner| owner.starts_with(':')).ok_or_else(conflict)?;
        // Target the unique owner, not the well-known name: an owner change
        // between the check and call must fail rather than use a recycled path.
        Proxy::new(&self.connection, owner.to_owned(), ROOT, SERVICE).map_err(error)
    }
    pub fn active_uuid(&self, interface: &str) -> Result<Option<String>, BridgeError> {
        Ok(self.selected_connection(interface)?.filter(|(_, _, state)| *state == 2).map(|(_, uuid, _)| uuid))
    }
    pub fn selected_uuid(&self, interface: &str) -> Result<Option<String>, BridgeError> {
        Ok(self.selected_connection(interface)?.map(|(_, uuid, _)| uuid))
    }
    fn selected_connection(&self, interface: &str) -> Result<Option<(OwnedObjectPath, String, u32)>, BridgeError> {
        let device: OwnedObjectPath = self.manager()?.call("GetDeviceByIpIface", &(interface,)).map_err(error)?;
        let device = self.proxy(device.as_str(), "org.freedesktop.NetworkManager.Device")?;
        let active: OwnedObjectPath = device.get_property("ActiveConnection").map_err(error)?;
        if active.as_str() == "/" { return Ok(None); }
        let proxy = self.proxy(active.as_str(), "org.freedesktop.NetworkManager.Connection.Active")?;
        let state = proxy.get_property::<u32>("State").map_err(error)?;
        if state == 4 { return Ok(None); }
        Ok(Some((active, proxy.get_property("Uuid").map_err(error)?, state)))
    }
    pub fn deactivate_selected(&self, interface: &str, expected_uuid: &str) -> Result<(), BridgeError> {
        root()?;
        let Some((active, uuid, _)) = self.selected_connection(interface)? else { return Ok(()); };
        if uuid != expected_uuid { return Err(conflict()); }
        self.manager()?.call("DeactivateConnection", &(&active,)).map_err(error)
    }
    pub fn link_identity(&self, interface: &str) -> Result<([u8; 6], u32), BridgeError> {
        let device: OwnedObjectPath = self.manager()?.call("GetDeviceByIpIface", &(interface,)).map_err(error)?;
        let proxy = self.proxy(device.as_str(), "org.freedesktop.NetworkManager.Device")?;
        let mac: String = proxy.get_property("HwAddress").map_err(error)?;
        let mac = mac.split(':').map(|part| u8::from_str_radix(part, 16).map_err(error)).collect::<Result<Vec<_>, _>>()?;
        Ok((mac.try_into().map_err(|_| error("Invalid Ethernet MAC"))?, proxy.get_property("Mtu").map_err(error)?))
    }
    pub fn wait_preserved(&self, interface: &str, expected: &crate::linux_verify::HostSnapshot) -> Result<(), BridgeError> {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            match expected.verify_preserved(&self.runtime(interface)?) {
                Ok(()) => return Ok(()),
                Err(error) if std::time::Instant::now() >= deadline => return Err(error),
                Err(_) => std::thread::sleep(Duration::from_millis(100)),
            }
        }
    }
    pub fn runtime(&self, interface: &str) -> Result<crate::linux_verify::HostSnapshot, BridgeError> {
        use crate::linux_verify::{FamilySnapshot, HostSnapshot};
        let device: OwnedObjectPath = self.manager()?.call("GetDeviceByIpIface", &(interface,)).map_err(error)?;
        let proxy = self.proxy(device.as_str(), "org.freedesktop.NetworkManager.Device")?;
        let family = |property: &str, interface: &str| -> Result<Option<FamilySnapshot>, BridgeError> {
            let path: OwnedObjectPath = proxy.get_property(property).map_err(error)?;
            if path.as_str() == "/" { return Ok(None); }
            let properties = self.proxy(path.as_str(), "org.freedesktop.DBus.Properties")?.call("GetAll", &(interface,)).map_err(error)?;
            FamilySnapshot::from_properties(properties).map(Some)
        };
        Ok(HostSnapshot { ipv4: family("Ip4Config", "org.freedesktop.NetworkManager.IP4Config")?,
            ipv6: family("Ip6Config", "org.freedesktop.NetworkManager.IP6Config")? })
    }
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
        self.checkpoint_in_epoch(&self.epoch()?, device, seconds)
    }
    pub fn checkpoint_in_epoch(&self, epoch: &str, device: &OwnedObjectPath, seconds: u32) -> Result<OwnedObjectPath, BridgeError> {
        root()?;
        // DELETE_NEW_CONNECTIONS also deletes unrelated profiles added by
        // other clients. Restore the device here; the durable journal removes
        // only the exact app-owned profiles, with content conflict checks.
        self.checkpoint_manager(epoch)?.call("CheckpointCreate", &(vec![device], seconds, 0u32)).map_err(error)
    }
    pub fn checkpoints(&self) -> Result<Vec<OwnedObjectPath>, BridgeError> { self.manager()?.get_property("Checkpoints").map_err(error) }
    pub fn rollback_checkpoint(&self, path: &OwnedObjectPath) -> Result<(), BridgeError> {
        self.rollback_checkpoint_in_epoch(&self.epoch()?, path)
    }
    pub fn rollback_checkpoint_in_epoch(&self, epoch: &str, path: &OwnedObjectPath) -> Result<(), BridgeError> {
        root()?;
        let result: std::collections::HashMap<String, u32> = self.checkpoint_manager(epoch)?.call("CheckpointRollback", &(path,)).map_err(error)?;
        if result.is_empty() || result.values().any(|status| *status != 0) { return Err(BridgeError::new("EROLLBACK", "NetworkManager could not fully restore the previous connection.")); }
        Ok(())
    }
    pub fn commit_checkpoint(&self, path: &OwnedObjectPath) -> Result<(), BridgeError> { self.commit_checkpoint_in_epoch(&self.epoch()?, path) }
    pub fn commit_checkpoint_in_epoch(&self, epoch: &str, path: &OwnedObjectPath) -> Result<(), BridgeError> { root()?; self.checkpoint_manager(epoch)?.call("CheckpointDestroy", &(path,)).map_err(error) }
    pub fn extend_checkpoint(&self, path: &OwnedObjectPath, seconds: u32) -> Result<(), BridgeError> { self.extend_checkpoint_in_epoch(&self.epoch()?, path, seconds) }
    pub fn extend_checkpoint_in_epoch(&self, epoch: &str, path: &OwnedObjectPath, seconds: u32) -> Result<(), BridgeError> { root()?; self.checkpoint_manager(epoch)?.call("CheckpointAdjustRollbackTimeout", &(path, seconds)).map_err(error) }
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
