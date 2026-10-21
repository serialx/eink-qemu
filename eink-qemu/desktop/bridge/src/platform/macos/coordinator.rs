//! macOS mutations under the shared durable coordinator. Every restoration
//! input is serializable; repair/removal need no process-local identity cache.
use crate::{coordinator::{PlatformOps, Prepared, Resource}, macos_service::{LocalUser, NativeService, ServiceSpec}, model::*};
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    service: ServiceSpec,
    was_configured: bool,
    was_running: bool,
    removing: bool,
}
impl Snapshot {
    pub fn service(&self) -> &ServiceSpec { &self.service }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum Mutation { Start(ServiceSpec), Remove(ServiceSpec) }

pub struct MacPlatform { native: NativeService, user: Option<LocalUser> }
impl MacPlatform {
    pub fn for_user(user: LocalUser) -> Result<Self, BridgeError> { Ok(Self { native: NativeService::new()?, user: Some(user) }) }
    pub fn for_recovery() -> Result<Self, BridgeError> { Ok(Self { native: NativeService::new()?, user: None }) }
}
impl PlatformOps for MacPlatform {
    type Snapshot = Snapshot;
    type Mutation = Mutation;
    fn prepare(&mut self, request: PlanRequest, id: OperationId, now: u64, existing: Option<&Resource<Snapshot>>) -> Result<Prepared<Snapshot, Mutation>, BridgeError> {
        let user = self.user.ok_or_else(|| BridgeError::new("EPERM", "Recovery cannot authorize new bridge setup."))?;
        self.native.check_payload()?;
        let service = match &request {
            PlanRequest::Configure { adapter_id } => {
                let adapter = crate::inspect()?.adapters.into_iter().find(|a| a.id == *adapter_id)
                    .ok_or_else(|| BridgeError::new("EDISCONNECTED", "The selected connection is no longer available."))?;
                ServiceSpec::new(user, ResourceId::new(id.as_str())?, &adapter)?
            },
            PlanRequest::Repair { resource_id } | PlanRequest::Remove { resource_id } => {
                let resource = existing.ok_or_else(|| BridgeError::new("ENOENT", "Bridge setup no longer exists."))?;
                let service = resource.original.service.clone();
                if service.resource_id() != resource_id || service.user() != user || resource.owner != format!("uid:{}", user.uid()) {
                    return Err(BridgeError::new("EPERM", "This bridge setup belongs to another user."));
                }
                service
            },
        };
        let removing = matches!(request, PlanRequest::Remove { .. });
        if !removing { service.validate_adapter(&crate::inspect()?.adapters)?; }
        let (was_configured, was_running) = self.native.state(&service)?;
        if matches!(request, PlanRequest::Configure { .. }) && (was_configured || was_running) {
            return Err(BridgeError::new("ECONFLICT", "This bridge identity already exists."));
        }
        let mutations = vec![if removing { Mutation::Remove(service.clone()) } else { Mutation::Start(service.clone()) }];
        let changes = if removing { vec!["Stop and remove this app's bridge service and packet socket.".into()] }
            else { vec![format!("Connect the bridge service to {}.", service.adapter_id().as_str()),
                "Allow only your user account to open its packet socket.".into(), "Check access without administrator privileges.".into()] };
        let plan = BridgePlan { id, request, fingerprint: service.fingerprint().into(), changes, requires_authorization: true,
            may_disconnect: false, expires_at: now.saturating_add(300) };
        Ok(Prepared { plan, snapshot: Snapshot { service, was_configured, was_running, removing }, mutations })
    }
    fn fingerprint(&mut self, request: &PlanRequest, snapshot: &Snapshot) -> Result<String, BridgeError> {
        // Only the coordinator's protected snapshot resolves opaque resource IDs.
        match request {
            PlanRequest::Configure { adapter_id } if adapter_id != snapshot.service.adapter_id() => return Err(BridgeError::new("ESTALE", "The planned adapter has changed.")),
            PlanRequest::Repair { resource_id } | PlanRequest::Remove { resource_id } if resource_id != snapshot.service.resource_id() => return Err(BridgeError::new("ESTALE", "The planned bridge has changed.")),
            _ => (),
        }
        self.native.check_configuration(&snapshot.service)?;
        // Removing an app-owned service is safe while its adapter is unplugged.
        // It does not modify that adapter or any host networking configuration.
        if !matches!(request, PlanRequest::Remove { .. }) { snapshot.service.validate_adapter(&crate::inspect()?.adapters)?; }
        Ok(snapshot.service.fingerprint().into())
    }
    fn apply(&mut self, mutation: &Mutation) -> Result<(), BridgeError> {
        match mutation { Mutation::Start(service) => self.native.start(service), Mutation::Remove(service) => self.native.remove(service) }
    }
    fn verify(&mut self, snapshot: &Snapshot) -> Result<(), BridgeError> {
        if snapshot.removing {
            if self.native.state(&snapshot.service)? != (false, false) || snapshot.service.socket().try_exists().map_err(|e| BridgeError::new("EIO", e.to_string()))? {
                return Err(BridgeError::new("ESERVICE", "The bridge service has not been removed."));
            }
            Ok(())
        } else { self.native.verify(&snapshot.service) }
    }
    fn commit(&mut self, _: &Snapshot) -> Result<(), BridgeError> { Ok(()) }
    fn rollback(&mut self, snapshot: &Snapshot) -> Result<(), BridgeError> {
        self.native.restore(&snapshot.service, snapshot.was_configured, snapshot.was_running)
    }
}
