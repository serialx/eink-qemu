//! Privileged Linux operations under the shared transaction journal. Existing
//! bridges are references: their membership and profiles are never modified.
use crate::{coordinator::{PlatformOps, Prepared, Resource}, linux_helper::{self, AccessChange, Grant},
    linux_network::{NetworkIdentity, NetworkSetup, NetworkStep, NetworkRemoval, RemovalStep, NetworkRepair, RepairStep},
    linux_network_manager::NetworkManager, linux_transaction::Checkpoint, model::*};
use serde::{Deserialize, Serialize};

fn conflict() -> BridgeError { BridgeError::new("ECONFLICT", "The selected bridge changed outside the app. Review its configuration before continuing.") }
fn denied() -> BridgeError { BridgeError::new("EPERM", "Only the installed service can change bridge setup for this user.") }
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
enum Network { Managed(NetworkSetup), Existing(Adapter) }
impl Network {
    fn adapter(&self) -> &str { match self { Self::Managed(setup) => setup.identity().adapter(), Self::Existing(adapter) => adapter.id.as_str() } }
    fn bridge(&self) -> &str { match self { Self::Managed(setup) => setup.identity().bridge(), Self::Existing(adapter) => adapter.id.as_str() } }
    fn fingerprint(&self) -> &str { match self { Self::Managed(setup) => setup.fingerprint(), Self::Existing(adapter) => &adapter.fingerprint } }
    fn verify_existing(&self) -> Result<(), BridgeError> {
        let Self::Existing(expected) = self else { return Err(conflict()); };
        let current = crate::inspect()?.adapters.into_iter().find(|adapter| adapter.id == expected.id).ok_or_else(conflict)?;
        if current.kind != AdapterKind::Bridge || current.fingerprint != expected.fingerprint { return Err(conflict()); }
        Ok(())
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
enum Action { Configure, Repair(Option<NetworkRepair>), Remove(Option<NetworkRemoval>) }
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    resource: ResourceId,
    network: Network,
    grant: Grant,
    access: AccessChange,
    action: Action,
    checkpoint: Option<Checkpoint>,
}
impl Snapshot {
    pub fn grant(&self) -> &Grant { &self.grant }
    pub fn target(&self) -> LaunchTarget { LaunchTarget::LinuxBridge { name: self.grant.bridge().into(), helper: linux_helper::HELPER.into() } }
    fn removing(&self) -> bool { matches!(self.action, Action::Remove(_)) }
}
#[derive(Clone, Serialize, Deserialize)]
pub enum Mutation { Begin, Configure(NetworkStep), Repair(RepairStep), Remove(RemovalStep), Access, ConfirmWindow }
pub struct LinuxPlatform { user: Option<u32> }
fn uuid(hex: &str) -> String { format!("{}-{}-{}-{}-{}", &hex[..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..]) }
impl LinuxPlatform {
    pub fn for_user(uid: u32) -> Result<Self, BridgeError> {
        if uid == 0 || unsafe { libc::geteuid() } != 0 { return Err(denied()); }
        Ok(Self { user: Some(uid) })
    }
    pub fn for_recovery() -> Result<Self, BridgeError> {
        if unsafe { libc::geteuid() } != 0 { return Err(denied()); }
        Ok(Self { user: None })
    }
}
impl PlatformOps for LinuxPlatform {
    type Snapshot = Snapshot;
    type Mutation = Mutation;
    fn prepare(&mut self, request: PlanRequest, id: OperationId, now: u64, existing: Option<&Resource<Snapshot>>) -> Result<Prepared<Snapshot, Mutation>, BridgeError> {
        let uid = self.user.ok_or_else(denied)?;
        let (resource, network) = match &request {
            PlanRequest::Configure { adapter_id } => {
                let adapter = crate::inspect()?.adapters.into_iter().find(|a| a.id == *adapter_id).ok_or_else(conflict)?;
                let network = if adapter.kind == AdapterKind::Bridge { Network::Existing(adapter) } else {
                    let mut random = [0u8; 16]; getrandom::fill(&mut random).map_err(|_| conflict())?;
                    let port = random.iter().map(|byte| format!("{byte:02x}")).collect::<String>();
                    let identity = NetworkIdentity::new(adapter.id.as_str(), &format!("eq{}", &id.as_str()[..12]), &uuid(id.as_str()), &uuid(&port))?;
                    Network::Managed(NetworkSetup::prepare(&NetworkManager::connect()?, identity)?)
                };
                (ResourceId::new(id.as_str())?, network)
            },
            PlanRequest::Repair { resource_id } | PlanRequest::Remove { resource_id } => {
                let existing = existing.ok_or_else(conflict)?;
                if existing.owner != format!("uid:{uid}") || existing.id != *resource_id || existing.original.grant.uid() != uid { return Err(denied()); }
                (resource_id.clone(), existing.original.network.clone())
            },
        };
        let removing = matches!(request, PlanRequest::Remove { .. });
        if matches!(&network, Network::Existing(_)) && !removing { network.verify_existing()?; }
        let grant = Grant::new(uid, network.bridge())?;
        let access = linux_helper::prepare(&grant, !removing)?;
        let action = match &request {
            PlanRequest::Configure { .. } => Action::Configure,
            PlanRequest::Repair { .. } => Action::Repair(match &network { Network::Managed(setup) => Some(NetworkRepair::prepare(&NetworkManager::connect()?, setup)?), _ => None }),
            PlanRequest::Remove { .. } => Action::Remove(match &network { Network::Managed(setup) => Some(NetworkRemoval::prepare(&NetworkManager::connect()?, setup)?), _ => None }),
        };
        let disruptive = matches!(&network, Network::Managed(_));
        let mut mutations = Vec::new();
        if disruptive {
            mutations.push(Mutation::Begin);
            if removing { mutations.push(Mutation::Access); }
            match &action {
                Action::Configure => mutations.extend(NetworkSetup::steps().into_iter().map(Mutation::Configure)),
                Action::Repair(_) => mutations.extend(NetworkRepair::steps().into_iter().map(Mutation::Repair)),
                Action::Remove(_) => mutations.extend(NetworkRemoval::steps().into_iter().map(Mutation::Remove)),
            }
            if !removing { mutations.push(Mutation::Access); }
            mutations.push(Mutation::ConfirmWindow);
        } else { mutations.push(Mutation::Access); }
        let changes = if disruptive {
            if removing { vec!["Restore the original Ethernet profile and remove this app's bridge profiles.".into(), "Remove your account's access to this app-managed bridge.".into()] }
            else { vec![format!("Configure {} as a port of {} while preserving host network settings.", network.adapter(), network.bridge()), "Allow your account to use the packaged bridge helper.".into(), "Restore the previous configuration unless you keep the connection within two minutes.".into()] }
        } else if removing { vec!["Remove this app's reference and your account's helper access. Keep the existing bridge and its members.".into()] }
        else { vec![format!("Allow your account to use existing bridge {} through the packaged helper.", network.bridge()), "Keep its current profiles and membership.".into()] };
        let plan = BridgePlan { id, request, fingerprint: network.fingerprint().into(), changes, requires_authorization: true, may_disconnect: disruptive, expires_at: now.saturating_add(300) };
        Ok(Prepared { plan, snapshot: Snapshot { resource, network, grant, access, action, checkpoint: None }, mutations })
    }
    fn fingerprint(&mut self, request: &PlanRequest, snapshot: &Snapshot) -> Result<String, BridgeError> {
        match request {
            PlanRequest::Configure { adapter_id } if adapter_id.as_str() != snapshot.network.adapter() => return Err(conflict()),
            PlanRequest::Repair { resource_id } | PlanRequest::Remove { resource_id } if resource_id != &snapshot.resource => return Err(conflict()),
            _ => (),
        }
        linux_helper::check_pending(&snapshot.access)?;
        match (&snapshot.network, &snapshot.action) {
            (Network::Managed(setup), Action::Configure) => setup.validate_prepared(&NetworkManager::connect()?)?,
            (_, Action::Repair(Some(repair))) => repair.validate_prepared(&NetworkManager::connect()?)?,
            (_, Action::Remove(Some(removal))) => removal.validate_prepared(&NetworkManager::connect()?)?,
            (Network::Existing(_), _) if !snapshot.removing() => snapshot.network.verify_existing()?,
            (Network::Existing(_), Action::Remove(None)) => (),
            _ => return Err(conflict()),
        }
        Ok(snapshot.network.fingerprint().into())
    }
    fn apply(&mut self, _: &Mutation) -> Result<(), BridgeError> { Err(BridgeError::new("EPROTOCOL", "Linux bridge mutations require a durable operation snapshot.")) }
    fn apply_recorded(&mut self, mutation: &Mutation, snapshot: &mut Snapshot) -> Result<(), BridgeError> {
        if matches!(mutation, Mutation::Access) { return linux_helper::apply(&snapshot.access); }
        let nm = NetworkManager::connect()?;
        match mutation {
            Mutation::Begin => snapshot.checkpoint = Some(match (&snapshot.network, &snapshot.action) {
                (Network::Managed(setup), Action::Configure) => setup.begin(&nm)?,
                (_, Action::Repair(Some(repair))) => repair.begin(&nm)?,
                (_, Action::Remove(Some(removal))) => removal.begin(&nm)?,
                _ => return Err(conflict()),
            }),
            Mutation::Configure(step) => match &snapshot.network { Network::Managed(setup) => setup.apply(&nm, *step)?, _ => return Err(conflict()) },
            Mutation::Repair(step) => match &snapshot.action { Action::Repair(Some(repair)) => repair.apply(&nm, *step)?, _ => return Err(conflict()) },
            Mutation::Remove(step) => match &snapshot.action { Action::Remove(Some(removal)) => removal.apply(&nm, *step)?, _ => return Err(conflict()) },
            Mutation::ConfirmWindow => snapshot.checkpoint.as_ref().ok_or_else(conflict)?.confirm_window(&nm)?,
            Mutation::Access => unreachable!(),
        }
        Ok(())
    }
    fn verify(&mut self, snapshot: &Snapshot) -> Result<(), BridgeError> {
        match (&snapshot.network, &snapshot.action) {
            (_, Action::Remove(Some(removal))) => removal.verify(&NetworkManager::connect()?)?,
            (_, Action::Remove(None)) => (),
            (_, Action::Repair(Some(repair))) => repair.verify(&NetworkManager::connect()?)?,
            (Network::Managed(setup), _) => setup.verify(&NetworkManager::connect()?)?,
            (Network::Existing(_), _) => snapshot.network.verify_existing()?,
        }
        if snapshot.removing() { linux_helper::verify_applied(&snapshot.access) }
        else { linux_helper::probe_as_user(&snapshot.grant) }
    }
    fn commit(&mut self, snapshot: &Snapshot) -> Result<(), BridgeError> { self.commit_recorded(&mut snapshot.clone()) }
    fn resume(&mut self, snapshot: &Snapshot) -> Result<(), BridgeError> {
        if let Network::Managed(setup) = &snapshot.network { setup.resume(&NetworkManager::connect()?)?; }
        self.verify(snapshot)
    }
    fn commit_recorded(&mut self, snapshot: &mut Snapshot) -> Result<(), BridgeError> {
        if matches!(snapshot.network, Network::Existing(_)) { return Ok(()); }
        let nm = NetworkManager::connect()?;
        let checkpoint = snapshot.checkpoint.as_ref().ok_or_else(conflict)?;
        match (&mut snapshot.network, &snapshot.action) {
            (Network::Managed(setup), Action::Configure) => setup.commit(&nm, checkpoint),
            (_, Action::Repair(Some(repair))) => repair.commit(&nm, checkpoint),
            (_, Action::Remove(Some(removal))) => removal.commit(&nm, checkpoint),
            _ => Err(conflict()),
        }
    }
    fn rollback(&mut self, snapshot: &Snapshot) -> Result<(), BridgeError> {
        match (&snapshot.network, &snapshot.action) {
            (Network::Managed(setup), Action::Configure) => setup.restore(&NetworkManager::connect()?, snapshot.checkpoint.as_ref())?,
            (_, Action::Repair(Some(repair))) => repair.rollback(&NetworkManager::connect()?, snapshot.checkpoint.as_ref())?,
            (_, Action::Remove(Some(removal))) => removal.rollback(&NetworkManager::connect()?, snapshot.checkpoint.as_ref())?,
            (Network::Existing(_), _) => (),
            _ => return Err(conflict()),
        }
        linux_helper::restore(&snapshot.access)
    }
}
