//! Kernel-authenticated requests. Polkit authorizes the peer process, never a
//! user identity, command, action name, or authorization blob supplied by IPC.
use crate::{coordinator::{Coordinator, Resource, ResourceLease}, linux_platform::{LinuxPlatform, Snapshot},
    linux_authorization, model::*, protocol::*, transport::Caller};
use std::{path::Path, time::{SystemTime, UNIX_EPOCH}};
fn now() -> u64 { SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() }
pub struct Dispatcher { coordinator: Coordinator<LinuxPlatform>, caller: Caller }
impl Dispatcher {
    pub fn new(root: &Path, caller: &Caller) -> Result<Self, BridgeError> {
        crate::transport::unix::validate_system_directory(root)?;
        caller.process().revalidate()?;
        Ok(Self { coordinator: Coordinator::open(root, caller.principal(), LinuxPlatform::for_user(caller.uid()?)?)?, caller: caller.clone() })
    }
    pub fn request(&mut self, request: HelperRequest) -> (HelperReply, Option<ResourceLease>) {
        let result = if request.version != VERSION { Err(BridgeError::new("EPROTOCOL", "Bridge helper version does not match the app.")) }
        else if let RequestKind::Acquire { selection } = &request.request {
            self.verified(selection).map(|(resource, lease)| (ReplyKind::Lease { selection: selection.clone(), target: resource.original.target() }, Some(lease)))
        } else { self.dispatch(request.request).map(|reply| (reply, None)) };
        match result {
            Ok((reply, lease)) => (HelperReply::new(reply), lease),
            Err(error) => (HelperReply::new(ReplyKind::Error { error }), None),
        }
    }
    fn dispatch(&mut self, request: RequestKind) -> Result<ReplyKind, BridgeError> {
        match request {
            RequestKind::Inspect => Ok(ReplyKind::Inspection { inspection: self.inspect()? }),
            RequestKind::Plan { request } => Ok(ReplyKind::Plan { plan: self.coordinator.plan(request, now())? }),
            RequestKind::Apply { plan_id, authorization } => {
                if authorization.is_some() { return Err(BridgeError::new("EPROTOCOL", "Linux authorization is verified directly with system policy.")); }
                let operation = self.coordinator.operation()?.ok_or_else(|| BridgeError::new("ESTALE", "Review bridge setup before applying it."))?;
                if operation.id != plan_id || operation.state != OperationState::AwaitingAuthorization { return Err(BridgeError::new("ESTALE", "Review the current bridge plan before applying it.")); }
                linux_authorization::authorize(&self.caller)?;
                // Authentication may take minutes. Recheck expiry with the
                // current time, not the time the connection was accepted.
                Ok(ReplyKind::Operation { operation: Some(self.coordinator.apply(&plan_id, now())?) })
            },
            RequestKind::Keep { operation_id } => Ok(ReplyKind::Operation { operation: Some(self.coordinator.keep(&operation_id, now())?) }),
            RequestKind::Cancel { operation_id } => Ok(ReplyKind::Operation { operation: Some(self.coordinator.cancel(&operation_id)?) }),
            RequestKind::Recover => Ok(ReplyKind::Operation { operation: self.coordinator.operation()? }),
            RequestKind::Verify { selection } => {
                let (_, _lease) = self.verified(&selection)?;
                Ok(ReplyKind::Verification { readiness: Readiness::Ready, selection: Some(selection) })
            },
            RequestKind::Acquire { .. } => Err(BridgeError::new("EPROTOCOL", "Acquiring a bridge requires descriptor transfer.")),
        }
    }
    fn verified(&mut self, selection: &BridgeSelection) -> Result<(Resource<Snapshot>, ResourceLease), BridgeError> {
        let id = selection.resource_id.as_ref().ok_or_else(|| BridgeError::new("ENEEDSSETUP", "Choose a managed bridge connection first."))?;
        let (resource, lease) = self.coordinator.verify_resource(id)?;
        if selection.version != VERSION || selection.platform != Platform::Linux
            || selection.adapter_id != resource.adapter_id || selection.fingerprint != resource.fingerprint { return Err(BridgeError::new("ESTALE", "The saved bridge connection has changed.")); }
        Ok((resource, lease))
    }
    fn inspect(&mut self) -> Result<Inspection, BridgeError> {
        let mut inspection = crate::inspect()?;
        inspection.support = if crate::linux_helper::check_helper().is_ok() { SupportState::Ready } else { SupportState::NeedsUpdate };
        inspection.external_override = None;
        inspection.active_operation = self.coordinator.operation()?;
        if let Some(resource) = self.coordinator.resources()?.last() {
            let selection = BridgeSelection { version: VERSION, platform: Platform::Linux,
                adapter_id: resource.adapter_id.clone(), fingerprint: resource.fingerprint.clone(), resource_id: Some(resource.id.clone()),
                display_name: inspection.adapters.iter().find(|adapter| adapter.id == resource.adapter_id)
                    .map(|adapter| adapter.name.clone()).unwrap_or_else(|| resource.adapter_id.as_str().into()) };
            inspection.readiness = selection_readiness(&selection, Platform::Linux, &inspection.adapters);
            if inspection.readiness == Readiness::NeedsSetup {
                inspection.readiness = match self.coordinator.verify_resource(&resource.id) {
                    Ok(_) => Readiness::Ready,
                    Err(error) if error.code == "EBUSY" => Readiness::NeedsSetup,
                    Err(_) => Readiness::NeedsRepair,
                };
            }
            inspection.selection = Some(selection);
        }
        Ok(inspection)
    }
}
