//! Journalable native network conversion. The coordinator records this complete
//! undo state before the first step and saves the checkpoint before continuing.
use crate::{linux_network_manager::NetworkManager, linux_profiles::{Profile, Migration, migrate, same_configuration},
    linux_transaction::{Checkpoint, ProfileChange}, linux_verify::HostSnapshot, model::{AdapterKind, BridgeError}};
use serde::{Deserialize, Serialize};
use std::{fs, path::Path};
use zbus::zvariant::OwnedObjectPath;
fn conflict() -> BridgeError { BridgeError::new("ECONFLICT", "The connection changed outside the app. Its changes have been preserved.") }
fn io(error: impl std::fmt::Display) -> BridgeError { BridgeError::new("ENETWORK", error.to_string()) }
fn uuid(profile: &Profile) -> Result<&str, BridgeError> { profile.get("connection").and_then(|s| s.get("uuid")).and_then(|v| <&str>::try_from(v).ok()).ok_or_else(conflict) }
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "IdentityFields")]
pub struct NetworkIdentity { adapter: String, bridge: String, bridge_uuid: String, port_uuid: String }
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IdentityFields { adapter: String, bridge: String, bridge_uuid: String, port_uuid: String }
impl TryFrom<IdentityFields> for NetworkIdentity {
    type Error = BridgeError;
    fn try_from(value: IdentityFields) -> Result<Self, BridgeError> { Self::new(&value.adapter, &value.bridge, &value.bridge_uuid, &value.port_uuid) }
}
impl NetworkIdentity {
    pub fn new(adapter: &str, bridge: &str, bridge_uuid: &str, port_uuid: &str) -> Result<Self, BridgeError> {
        let interface = |name: &str| !name.is_empty() && name.len() < 16 && name.bytes().all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b));
        let uuid = |id: &str| id.len() == 36 && id.bytes().enumerate().all(|(i, b)| if [8, 13, 18, 23].contains(&i) { b == b'-' } else { b.is_ascii_hexdigit() });
        if !interface(adapter) || !interface(bridge) || adapter == bridge || !uuid(bridge_uuid) || !uuid(port_uuid) || bridge_uuid == port_uuid { return Err(conflict()); }
        Ok(Self { adapter: adapter.into(), bridge: bridge.into(), bridge_uuid: bridge_uuid.into(), port_uuid: port_uuid.into() })
    }
    pub fn adapter(&self) -> &str { &self.adapter }
    pub fn bridge(&self) -> &str { &self.bridge }
    pub fn bridge_uuid(&self) -> &str { &self.bridge_uuid }
    pub fn port_uuid(&self) -> &str { &self.port_uuid }
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum NetworkStep { CreateBridge, CreatePort, DisableOriginal, ActivateBridge, ActivatePort }
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum RemovalStep { RestoreOriginal, ActivateOriginal, DeletePort, DeleteBridge }
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum RepairStep { RestoreBridge, RestorePort, DisableOriginal, ActivateBridge, ActivatePort }
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkRepair {
    setup: NetworkSetup,
    original: Profile,
    bridge: Option<Profile>,
    port: Option<Profile>,
    active: Option<String>,
    runtime: Option<HostSnapshot>,
}
impl NetworkRepair {
    pub fn prepare(nm: &NetworkManager, setup: &NetworkSetup) -> Result<Self, BridgeError> {
        if !setup.committed { return Err(conflict()); }
        setup.check_adapter()?;
        setup.check_members()?;
        let bridge = setup.known_owned(nm, &setup.migration.bridge)?;
        let port = setup.known_owned(nm, &setup.migration.port)?;
        if bridge.is_none() && Path::new("/sys/class/net").join(&setup.identity.bridge).exists() { return Err(conflict()); }
        let original = nm.find_profile(uuid(&setup.original)?)?.ok_or_else(conflict)?.1;
        ProfileChange::new(Some(setup.original.clone()), Some(setup.migration.original_disabled.clone()))?.check(nm)?;
        let active = nm.selected_uuid(&setup.identity.adapter)?;
        if active.as_deref().is_some_and(|id| id != uuid(&setup.original).unwrap_or("") && id != setup.identity.port_uuid) { return Err(conflict()); }
        let runtime = match active.as_deref() {
            Some(id) if id == setup.identity.port_uuid => {
                if bridge.is_none() || port.is_none() { return Err(conflict()); }
                Some(nm.runtime(&setup.identity.bridge)?)
            },
            Some(_) => Some(nm.runtime(&setup.identity.adapter)?),
            None => None,
        };
        Ok(Self { setup: setup.clone(), original, bridge, port, active, runtime })
    }
    fn changes(&self) -> Result<[ProfileChange; 3], BridgeError> {
        Ok([
            ProfileChange::new(Some(self.original.clone()), Some(self.setup.migration.original_disabled.clone()))?,
            ProfileChange::new(self.bridge.clone(), Some(self.setup.migration.bridge.clone()))?,
            ProfileChange::new(self.port.clone(), Some(self.setup.migration.port.clone()))?,
        ])
    }
    fn check_transition(&self, nm: &NetworkManager) -> Result<(), BridgeError> {
        self.setup.check_adapter()?;
        self.setup.check_members()?;
        for change in self.changes()? { change.check(nm)?; }
        let active = nm.selected_uuid(&self.setup.identity.adapter)?;
        if active.as_deref().is_some_and(|id| id != uuid(&self.setup.original).unwrap_or("") && id != self.setup.identity.port_uuid) { return Err(conflict()); }
        Ok(())
    }
    fn before_matches(&self, nm: &NetworkManager) -> Result<(), BridgeError> {
        for expected in [Some(&self.original), self.bridge.as_ref(), self.port.as_ref()].into_iter().zip([
            uuid(&self.original)?, self.setup.identity.bridge_uuid(), self.setup.identity.port_uuid(),
        ]) {
            let current = nm.find_profile(expected.1)?.map(|(_, profile)| profile);
            if !match (expected.0, current.as_ref()) { (None, None) => true, (Some(a), Some(b)) => same_configuration(a, b), _ => false } { return Err(conflict()); }
        }
        if nm.selected_uuid(&self.setup.identity.adapter)? != self.active { return Err(conflict()); }
        if let Some(runtime) = &self.runtime {
            let interface = if self.active.as_deref() == Some(&self.setup.identity.port_uuid) { &self.setup.identity.bridge } else { &self.setup.identity.adapter };
            nm.wait_preserved(interface, runtime)?;
        }
        Ok(())
    }
    pub fn validate_prepared(&self, nm: &NetworkManager) -> Result<(), BridgeError> {
        self.check_transition(nm)?;
        self.before_matches(nm)
    }
    pub fn begin(&self, nm: &NetworkManager) -> Result<Checkpoint, BridgeError> {
        self.validate_prepared(nm)?;
        Checkpoint::create(nm, &nm.device_path(&self.setup.identity.adapter)?)
    }
    pub fn steps() -> [RepairStep; 5] { [RepairStep::RestoreBridge, RepairStep::RestorePort, RepairStep::DisableOriginal, RepairStep::ActivateBridge, RepairStep::ActivatePort] }
    pub fn apply(&self, nm: &NetworkManager, step: RepairStep) -> Result<(), BridgeError> {
        self.check_transition(nm)?;
        let changes = self.changes()?;
        match step {
            RepairStep::RestoreBridge => changes[1].apply(nm),
            RepairStep::RestorePort => changes[2].apply(nm),
            RepairStep::DisableOriginal => changes[0].apply(nm),
            RepairStep::ActivateBridge => {
                let (path, _) = nm.find_profile(&self.setup.identity.bridge_uuid)?.ok_or_else(conflict)?;
                nm.activate(&path, &OwnedObjectPath::try_from("/").unwrap()).map(|_| ())
            },
            RepairStep::ActivatePort => {
                let (path, _) = nm.find_profile(&self.setup.identity.port_uuid)?.ok_or_else(conflict)?;
                let active = nm.activate(&path, &nm.device_path(&self.setup.identity.adapter)?)?;
                nm.wait_active(&active)
            },
        }
    }
    pub fn verify(&self, nm: &NetworkManager) -> Result<(), BridgeError> {
        self.setup.verify_active(nm, true)?;
        if let Some(runtime) = &self.runtime { nm.wait_preserved(&self.setup.identity.bridge, runtime)?; }
        Ok(())
    }
    pub fn commit(&self, nm: &NetworkManager, checkpoint: &Checkpoint) -> Result<(), BridgeError> { self.verify(nm)?; checkpoint.disarm(nm) }
    pub fn rollback(&self, nm: &NetworkManager, checkpoint: Option<&Checkpoint>) -> Result<(), BridgeError> {
        if let Err(error) = self.check_transition(nm) {
            if let Some(checkpoint) = checkpoint { checkpoint.disarm(nm)?; }
            return Err(error);
        }
        if let Some(checkpoint) = checkpoint { checkpoint.rollback(nm)?; }
        let changes = self.changes()?;
        changes[0].reverse().apply(nm)?;
        // Restore Ethernet before removing newly created port/bridge profiles.
        if self.active.as_deref() == Some(uuid(&self.original)?) {
            let (path, _) = nm.find_profile(uuid(&self.original)?)?.ok_or_else(conflict)?;
            let active = nm.activate(&path, &nm.device_path(&self.setup.identity.adapter)?)?;
            nm.wait_active(&active)?;
        }
        changes[2].reverse().apply(nm)?;
        changes[1].reverse().apply(nm)?;
        if self.active.as_deref() == Some(&self.setup.identity.port_uuid) {
            let (bridge, _) = nm.find_profile(&self.setup.identity.bridge_uuid)?.ok_or_else(conflict)?;
            let bridge_active = nm.activate(&bridge, &OwnedObjectPath::try_from("/").unwrap())?;
            let (port, _) = nm.find_profile(&self.setup.identity.port_uuid)?.ok_or_else(conflict)?;
            let active = nm.activate(&port, &nm.device_path(&self.setup.identity.adapter)?)?;
            nm.wait_active(&active)?;
            nm.wait_active(&bridge_active)?;
        } else if self.active.is_none() {
            if let Some(active) = nm.selected_uuid(&self.setup.identity.adapter)? {
                if active != uuid(&self.original)? && active != self.setup.identity.port_uuid { return Err(conflict()); }
                nm.deactivate_selected(&self.setup.identity.adapter, &active)?;
            }
            if Path::new("/sys/class/net").join(&self.setup.identity.bridge).exists() {
                nm.deactivate_selected(&self.setup.identity.bridge, &self.setup.identity.bridge_uuid)?;
            }
        }
        self.before_matches(nm)
    }
}
/// Removal has its own undo state: cancelling it must restore the previously
/// committed bridge, not the Ethernet state from before installation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkRemoval {
    setup: NetworkSetup,
    // Earlier journals assumed a fully working bridge. New removal plans also
    // retain missing profiles and inactive state so Cancel restores them exactly.
    #[serde(default)]
    prior: Option<NetworkRepair>,
}
impl NetworkRemoval {
    pub fn prepare(nm: &NetworkManager, setup: &NetworkSetup) -> Result<Self, BridgeError> {
        if !setup.committed { return Err(conflict()); }
        let prior = NetworkRepair::prepare(nm, setup)?;
        let mut setup = setup.clone();
        // DHCP may legitimately have renewed since installation. Capture the
        // currently working bridge's runtime for this operation's undo state.
        if let Some(runtime) = &prior.runtime { setup.before = runtime.clone(); }
        Ok(Self { setup, prior: Some(prior) })
    }
    pub fn validate_prepared(&self, nm: &NetworkManager) -> Result<(), BridgeError> {
        if let Some(prior) = &self.prior { return prior.validate_prepared(nm); }
        self.setup.verify(nm)?;
        self.setup.check_members()?;
        nm.wait_preserved(&self.setup.identity.bridge, &self.setup.before)
    }
    pub fn begin(&self, nm: &NetworkManager) -> Result<Checkpoint, BridgeError> {
        self.validate_prepared(nm)?;
        Checkpoint::create(nm, &nm.device_path(&self.setup.identity.adapter)?)
    }
    pub fn steps() -> [RemovalStep; 4] { [RemovalStep::RestoreOriginal, RemovalStep::ActivateOriginal, RemovalStep::DeletePort, RemovalStep::DeleteBridge] }
    fn check_transition(&self, nm: &NetworkManager) -> Result<(), BridgeError> {
        self.setup.check_adapter()?;
        self.setup.check_members()?;
        for profile in [&self.setup.migration.bridge, &self.setup.migration.port] { self.setup.known_owned(nm, profile)?; }
        ProfileChange::new(Some(self.setup.migration.original_disabled.clone()), Some(self.setup.original.clone()))?.check(nm)?;
        let active = nm.selected_uuid(&self.setup.identity.adapter)?;
        if active.as_deref().is_some_and(|id| id != uuid(&self.setup.original).unwrap_or("") && id != self.setup.identity.port_uuid) { return Err(conflict()); }
        Ok(())
    }
    pub fn apply(&self, nm: &NetworkManager, step: RemovalStep) -> Result<(), BridgeError> {
        self.check_transition(nm)?;
        match step {
            RemovalStep::RestoreOriginal => ProfileChange::new(Some(self.setup.migration.original_disabled.clone()), Some(self.setup.original.clone()))?.apply(nm),
            RemovalStep::ActivateOriginal => {
                let (path, profile) = nm.find_profile(uuid(&self.setup.original)?)?.ok_or_else(conflict)?;
                if !same_configuration(&profile, &self.setup.original) { return Err(conflict()); }
                let active = nm.activate(&path, &nm.device_path(&self.setup.identity.adapter)?)?;
                nm.wait_active(&active)
            },
            RemovalStep::DeletePort => ProfileChange::new(Some(self.setup.migration.port.clone()), None)?.apply(nm),
            RemovalStep::DeleteBridge => ProfileChange::new(Some(self.setup.migration.bridge.clone()), None)?.apply(nm),
        }
    }
    pub fn verify(&self, nm: &NetworkManager) -> Result<(), BridgeError> {
        self.check_transition(nm)?;
        if nm.find_profile(&self.setup.identity.bridge_uuid)?.is_some() || nm.find_profile(&self.setup.identity.port_uuid)?.is_some()
            || nm.active_uuid(&self.setup.identity.adapter)?.as_deref() != Some(uuid(&self.setup.original)?) { return Err(conflict()); }
        let original = nm.find_profile(uuid(&self.setup.original)?)?.ok_or_else(conflict)?.1;
        if !same_configuration(&original, &self.setup.original) { return Err(conflict()); }
        if self.prior.as_ref().is_some_and(|prior| prior.runtime.is_none()) {
            self.setup.before.verify_committed(&nm.runtime(&self.setup.identity.adapter)?, &self.setup.original)
        } else { nm.wait_preserved(&self.setup.identity.adapter, &self.setup.before) }
    }
    pub fn commit(&self, nm: &NetworkManager, checkpoint: &Checkpoint) -> Result<(), BridgeError> {
        self.verify(nm)?;
        checkpoint.disarm(nm)
    }
    pub fn rollback(&self, nm: &NetworkManager, checkpoint: Option<&Checkpoint>) -> Result<(), BridgeError> {
        if let Err(error) = self.check_transition(nm) {
            if let Some(checkpoint) = checkpoint { checkpoint.disarm(nm)?; }
            return Err(error);
        }
        if let Some(checkpoint) = checkpoint { checkpoint.rollback(nm)?; }
        if let Some(prior) = &self.prior {
            ProfileChange::new(Some(self.setup.original.clone()), Some(prior.original.clone()))?.apply(nm)?;
            if prior.active.as_deref() == Some(uuid(&prior.original)?) {
                let (path, _) = nm.find_profile(uuid(&prior.original)?)?.ok_or_else(conflict)?;
                let active = nm.activate(&path, &nm.device_path(&self.setup.identity.adapter)?)?;
                nm.wait_active(&active)?;
            }
            for profile in [&prior.bridge, &prior.port].into_iter().flatten() {
                ProfileChange::new(None, Some(profile.clone()))?.apply(nm)?;
            }
            if prior.active.as_deref() == Some(&self.setup.identity.port_uuid) {
                let (bridge, _) = nm.find_profile(&self.setup.identity.bridge_uuid)?.ok_or_else(conflict)?;
                let bridge_active = nm.activate(&bridge, &OwnedObjectPath::try_from("/").unwrap())?;
                let (port, _) = nm.find_profile(&self.setup.identity.port_uuid)?.ok_or_else(conflict)?;
                let active = nm.activate(&port, &nm.device_path(&self.setup.identity.adapter)?)?;
                nm.wait_active(&active)?;
                nm.wait_active(&bridge_active)?;
            } else if prior.active.is_none() {
                if let Some(active) = nm.selected_uuid(&self.setup.identity.adapter)? {
                    if active != uuid(&prior.original)? && active != self.setup.identity.port_uuid { return Err(conflict()); }
                    nm.deactivate_selected(&self.setup.identity.adapter, &active)?;
                }
                if Path::new("/sys/class/net").join(&self.setup.identity.bridge).exists() {
                    nm.deactivate_selected(&self.setup.identity.bridge, &self.setup.identity.bridge_uuid)?;
                }
            }
            return prior.before_matches(nm);
        }
        ProfileChange::new(Some(self.setup.original.clone()), Some(self.setup.migration.original_disabled.clone()))?.apply(nm)?;
        for profile in [&self.setup.migration.bridge, &self.setup.migration.port] {
            ProfileChange::new(None, Some(profile.clone()))?.apply(nm)?;
        }
        let (bridge, _) = nm.find_profile(&self.setup.identity.bridge_uuid)?.ok_or_else(conflict)?;
        let bridge_active = nm.activate(&bridge, &OwnedObjectPath::try_from("/").unwrap())?;
        let (port, _) = nm.find_profile(&self.setup.identity.port_uuid)?.ok_or_else(conflict)?;
        let active = nm.activate(&port, &nm.device_path(&self.setup.identity.adapter)?)?;
        nm.wait_active(&active)?;
        nm.wait_active(&bridge_active)?;
        self.setup.verify(nm)?;
        nm.wait_preserved(&self.setup.identity.bridge, &self.setup.before)
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkSetup {
    identity: NetworkIdentity,
    fingerprint: String,
    original: Profile,
    migration: Migration,
    before: HostSnapshot,
    mac: [u8; 6],
    mtu: u32,
    #[serde(default)]
    committed: bool,
}
fn owned_profile_matches(expected: &Profile, current: &Profile, committed: bool) -> bool {
    same_configuration(current, expected) || (!committed && same_configuration(current, &NetworkSetup::staged(expected)))
}
impl NetworkSetup {
    pub fn prepare(nm: &NetworkManager, identity: NetworkIdentity) -> Result<Self, BridgeError> {
        let adapter = crate::inspect()?.adapters.into_iter().find(|a| a.id.as_str() == identity.adapter).ok_or_else(conflict)?;
        if adapter.kind != AdapterKind::Ethernet || !adapter.link_up || !adapter.eligible { return Err(BridgeError::new("EUNSUPPORTED", "Select a connected Ethernet adapter managed by NetworkManager.")); }
        if Path::new("/sys/class/net").join(&identity.adapter).join("master").exists()
            || Path::new("/sys/class/net").join(&identity.bridge).exists()
            || nm.find_profile(&identity.bridge_uuid)?.is_some() || nm.find_profile(&identity.port_uuid)?.is_some() { return Err(conflict()); }
        let active = nm.active_profile(&identity.adapter)?;
        nm.require_durable_profile(&active.path)?;
        let original = active.profile;
        let before = nm.runtime(&identity.adapter)?;
        let (mac, mtu) = nm.link_identity(&identity.adapter)?;
        let mut migration = migrate(&original, &identity.adapter, &identity.bridge, &identity.bridge_uuid, &identity.port_uuid)?;
        migration.preserve_runtime(&before, mac, mtu)?;
        Ok(Self { identity, fingerprint: adapter.fingerprint, original, migration, before, mac, mtu, committed: false })
    }
    pub fn identity(&self) -> &NetworkIdentity { &self.identity }
    pub fn fingerprint(&self) -> &str { &self.fingerprint }
    pub fn migration(&self) -> &Migration { &self.migration }
    pub fn before(&self) -> &HostSnapshot { &self.before }
    pub fn steps() -> [NetworkStep; 5] { [NetworkStep::CreateBridge, NetworkStep::CreatePort, NetworkStep::DisableOriginal, NetworkStep::ActivateBridge, NetworkStep::ActivatePort] }
    fn staged(profile: &Profile) -> Profile {
        let mut result = profile.clone();
        result.get_mut("connection").unwrap().insert("autoconnect".into(), false.into());
        result
    }
    fn check_adapter(&self) -> Result<(), BridgeError> {
        let adapter = crate::inspect()?.adapters.into_iter().find(|a| a.id.as_str() == self.identity.adapter).ok_or_else(conflict)?;
        if adapter.fingerprint != self.fingerprint { return Err(conflict()); }
        Ok(())
    }
    pub fn validate_prepared(&self, nm: &NetworkManager) -> Result<(), BridgeError> {
        self.check_adapter()?;
        self.check_members()?;
        let current = nm.active_profile(&self.identity.adapter)?;
        nm.require_durable_profile(&current.path)?;
        if !same_configuration(&current.profile, &self.original)
            || nm.link_identity(&self.identity.adapter)? != (self.mac, self.mtu)
            || nm.find_profile(&self.identity.bridge_uuid)?.is_some() || nm.find_profile(&self.identity.port_uuid)?.is_some()
            || Path::new("/sys/class/net").join(&self.identity.bridge).exists() { return Err(conflict()); }
        self.before.verify_preserved(&nm.runtime(&self.identity.adapter)?)
    }
    pub fn begin(&self, nm: &NetworkManager) -> Result<Checkpoint, BridgeError> {
        self.validate_prepared(nm)?;
        Checkpoint::create(nm, &nm.device_path(&self.identity.adapter)?)
    }
    pub fn apply(&self, nm: &NetworkManager, step: NetworkStep) -> Result<(), BridgeError> {
        self.check_adapter()?;
        match step {
            NetworkStep::CreateBridge => ProfileChange::new(None, Some(Self::staged(&self.migration.bridge)))?.apply(nm),
            NetworkStep::CreatePort => ProfileChange::new(None, Some(Self::staged(&self.migration.port)))?.apply(nm),
            NetworkStep::DisableOriginal => ProfileChange::new(Some(self.original.clone()), Some(self.migration.original_disabled.clone()))?.apply(nm),
            NetworkStep::ActivateBridge => {
                let (path, profile) = nm.find_profile(&self.identity.bridge_uuid)?.ok_or_else(conflict)?;
                if !same_configuration(&profile, &Self::staged(&self.migration.bridge)) { return Err(conflict()); }
                // Activation completes once the separately journaled physical
                // port step supplies carrier. Do not wait for it here.
                nm.activate(&path, &OwnedObjectPath::try_from("/").unwrap()).map(|_| ())
            },
            NetworkStep::ActivatePort => {
                let (path, profile) = nm.find_profile(&self.identity.port_uuid)?.ok_or_else(conflict)?;
                if !same_configuration(&profile, &Self::staged(&self.migration.port)) { return Err(conflict()); }
                let device = nm.device_path(&self.identity.adapter)?;
                let active = nm.activate(&path, &device)?;
                nm.wait_active(&active)
            },
        }
    }
    fn known_owned(&self, nm: &NetworkManager, expected: &Profile) -> Result<Option<Profile>, BridgeError> {
        let current = nm.find_profile(uuid(expected)?)?.map(|(_, p)| p);
        if current.as_ref().is_some_and(|p| !owned_profile_matches(expected, p, self.committed)) { return Err(conflict()); }
        Ok(current)
    }
    fn check_members(&self) -> Result<(), BridgeError> {
        match fs::canonicalize(Path::new("/sys/class/net").join(&self.identity.adapter).join("master")) {
            Ok(master) if master.file_name().and_then(|name| name.to_str()) != Some(self.identity.bridge.as_str()) => return Err(conflict()),
            Ok(_) => (),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(io(error)),
        }
        let entries = match fs::read_dir(Path::new("/sys/class/net").join(&self.identity.bridge).join("brif")) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(io(error)),
        };
        for entry in entries {
            if entry.map_err(io)?.file_name().to_str() != Some(self.identity.adapter.as_str()) { return Err(BridgeError::new("ECONFLICT", "Another interface is using this bridge. Its membership has been preserved.")); }
        }
        Ok(())
    }
    pub fn verify(&self, nm: &NetworkManager) -> Result<(), BridgeError> {
        self.verify_active(nm, !self.committed)
    }
    fn verify_active(&self, nm: &NetworkManager, wait: bool) -> Result<(), BridgeError> {
        self.verify_profiles(nm)?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(40);
        while nm.active_uuid(&self.identity.bridge)?.as_deref() != Some(&self.identity.bridge_uuid)
            || nm.active_uuid(&self.identity.adapter)?.as_deref() != Some(&self.identity.port_uuid) {
            if !wait { return Err(BridgeError::new("ENEEDSREPAIR", "The managed bridge is inactive. Open bridge setup to repair or remove it.")); }
            if std::time::Instant::now() >= deadline { return Err(BridgeError::new("ETIMEOUT", "The bridge did not become active.")); }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        let master = fs::canonicalize(Path::new("/sys/class/net").join(&self.identity.adapter).join("master")).map_err(io)?;
        if master.file_name().and_then(|v| v.to_str()) != Some(self.identity.bridge.as_str()) || nm.link_identity(&self.identity.bridge)? != (self.mac, self.mtu) { return Err(conflict()); }
        if self.committed { self.before.verify_committed(&nm.runtime(&self.identity.bridge)?, &self.migration.bridge) }
        else { nm.wait_preserved(&self.identity.bridge, &self.before) }
    }
    fn verify_profiles(&self, nm: &NetworkManager) -> Result<(), BridgeError> {
        self.check_adapter()?;
        self.check_members()?;
        for profile in [&self.migration.bridge, &self.migration.port] {
            if self.known_owned(nm, profile)?.is_none() { return Err(conflict()); }
        }
        let original = nm.find_profile(uuid(&self.original)?)?.ok_or_else(conflict)?.1;
        if !same_configuration(&original, &self.migration.original_disabled) { return Err(conflict()); }
        Ok(())
    }
    /// Restore only activation after boot. Never recreate or overwrite profiles
    /// here, and never displace a connection selected outside this app.
    pub fn resume(&self, nm: &NetworkManager) -> Result<(), BridgeError> {
        if !self.committed { return Err(conflict()); }
        self.verify_profiles(nm)?;
        let selected = nm.selected_uuid(&self.identity.adapter)?;
        if selected.as_deref().is_some_and(|id| id != self.identity.port_uuid) { return Err(conflict()); }
        if Path::new("/sys/class/net").join(&self.identity.bridge).exists()
            && nm.selected_uuid(&self.identity.bridge)?.as_deref().is_some_and(|id| id != self.identity.bridge_uuid) { return Err(conflict()); }
        if nm.active_uuid(&self.identity.adapter)?.as_deref() == Some(&self.identity.port_uuid)
            && nm.active_uuid(&self.identity.bridge)?.as_deref() == Some(&self.identity.bridge_uuid) { return self.verify(nm); }
        let (bridge, _) = nm.find_profile(&self.identity.bridge_uuid)?.ok_or_else(conflict)?;
        let bridge_active = nm.activate(&bridge, &OwnedObjectPath::try_from("/").unwrap())?;
        let (port, _) = nm.find_profile(&self.identity.port_uuid)?.ok_or_else(conflict)?;
        let active = nm.activate(&port, &nm.device_path(&self.identity.adapter)?)?;
        nm.wait_active(&active)?;
        nm.wait_active(&bridge_active)?;
        self.verify_active(nm, true)
    }
    pub fn commit(&mut self, nm: &NetworkManager, checkpoint: &Checkpoint) -> Result<(), BridgeError> {
        self.verify(nm)?;
        for profile in [&self.migration.port, &self.migration.bridge] {
            ProfileChange::new(Some(Self::staged(profile)), Some(profile.clone()))?.apply(nm)?;
        }
        checkpoint.disarm(nm)?;
        self.committed = true;
        Ok(())
    }
    /// Restore even when the native checkpoint expired or disappeared at boot.
    /// Validate every affected profile before the first persistent write.
    pub fn restore(&self, nm: &NetworkManager, checkpoint: Option<&Checkpoint>) -> Result<(), BridgeError> {
        let check = (|| -> Result<(), BridgeError> {
            self.check_adapter()?;
            self.check_members()?;
            self.known_owned(nm, &self.migration.port)?;
            self.known_owned(nm, &self.migration.bridge)?;
            ProfileChange::new(Some(self.migration.original_disabled.clone()), Some(self.original.clone()))?.check(nm)?;
            let active = nm.selected_uuid(&self.identity.adapter)?;
            if active.as_deref().is_some_and(|id| id != uuid(&self.original).unwrap_or("") && id != self.identity.port_uuid) { return Err(conflict()); }
            Ok(())
        })();
        if let Err(error) = check {
            // Stop the native timer from later overwriting the very external
            // edit that caused recovery to refuse a mutation.
            if let Some(checkpoint) = checkpoint { checkpoint.disarm(nm)?; }
            return Err(error);
        }
        if let Some(checkpoint) = checkpoint { checkpoint.rollback(nm)?; }
        ProfileChange::new(Some(self.migration.original_disabled.clone()), Some(self.original.clone()))?.apply(nm)?;
        if nm.active_uuid(&self.identity.adapter)?.as_deref() != Some(uuid(&self.original)?) {
            let (path, _) = nm.find_profile(uuid(&self.original)?)?.ok_or_else(conflict)?;
            let active = nm.activate(&path, &OwnedObjectPath::try_from("/").unwrap())?;
            nm.wait_active(&active)?;
        }
        for expected in [&self.migration.port, &self.migration.bridge] {
            if let Some(current) = self.known_owned(nm, expected)? { ProfileChange::new(Some(current), None)?.apply(nm)?; }
        }
        nm.wait_preserved(&self.identity.adapter, &self.before)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn committed_profiles_do_not_accept_later_autoconnect_edits() {
        let expected = Profile(std::collections::BTreeMap::from([("connection".into(), std::collections::BTreeMap::from([("autoconnect".into(), true.into())]))]));
        let staged = NetworkSetup::staged(&expected);
        assert!(owned_profile_matches(&expected, &staged, false));
        assert!(!owned_profile_matches(&expected, &staged, true));
        assert!(owned_profile_matches(&expected, &expected, true));
        let mut edited = staged.clone();
        edited.get_mut("connection").unwrap().insert("autoconnect-priority".into(), 5i32.into());
        assert!(!owned_profile_matches(&expected, &edited, false));
    }
}
