//! Fixed helper operations. The listener supplies kernel credentials; no caller
//! can select a principal, journal directory, executable or packet socket.
use crate::{authorization::macos::Authorization, coordinator::{Coordinator, Resource, ResourceLease}, macos_platform::{MacPlatform, Snapshot},
    macos_service::LocalUser, model::*, protocol::*, transport::Caller};
use std::path::Path;

pub struct Dispatcher { coordinator: Coordinator<MacPlatform> }
impl Dispatcher {
    /// `root` is selected by the service entry point, never deserialized from IPC.
    pub fn new(root: &Path, caller: &Caller) -> Result<Self, BridgeError> {
        crate::transport::unix::validate_system_directory(root)?;
        let user = LocalUser::lookup(caller.uid()?)?;
        Ok(Self { coordinator: Coordinator::open(root, caller.principal(), MacPlatform::for_user(user)?)? })
    }
    pub fn request(&mut self, request: HelperRequest, now: u64) -> (HelperReply, Option<ResourceLease>) {
        let result = if request.version != VERSION { Err(BridgeError::new("EPROTOCOL", "Bridge helper version does not match the app.")) }
        else if let RequestKind::Acquire { selection } = &request.request {
            self.verified(selection).map(|(resource, lease)| (ReplyKind::Lease {
                selection: selection.clone(), target: LaunchTarget::MacSocket { path: resource.original.service().socket() },
            }, Some(lease)))
        } else { self.dispatch(request, now).map(|reply| (reply, None)) };
        match result {
            Ok((reply, lease)) => (HelperReply::new(reply), lease),
            Err(error) => (HelperReply::new(ReplyKind::Error { error }), None),
        }
    }
    fn dispatch(&mut self, request: HelperRequest, now: u64) -> Result<ReplyKind, BridgeError> {
        if request.version != VERSION { return Err(BridgeError::new("EPROTOCOL", "Bridge helper version does not match the app.")); }
        match request.request {
            RequestKind::Inspect => Ok(ReplyKind::Inspection { inspection: self.inspect()? }),
            RequestKind::Plan { request } => Ok(ReplyKind::Plan { plan: self.coordinator.plan(request, now)? }),
            RequestKind::Apply { plan_id, authorization } => {
                let proof = authorization.ok_or_else(|| BridgeError::new("EAUTHDENIED", "Administrator authorization is required for bridge setup."))?;
                let _authorization = Authorization::verify(&proof)?;
                Ok(ReplyKind::Operation { operation: Some(self.coordinator.apply_after_authorization(&plan_id, now)?) })
            },
            RequestKind::Keep { operation_id } => Ok(ReplyKind::Operation { operation: Some(self.coordinator.keep(&operation_id, now)?) }),
            RequestKind::Cancel { operation_id } => Ok(ReplyKind::Operation { operation: Some(self.coordinator.cancel(&operation_id)?) }),
            // Reattaching a UI must not roll back another user's operation or
            // truncate a live confirmation window. Startup recovery belongs to
            // the independent service worker, before its listener is opened.
            RequestKind::Recover => Ok(ReplyKind::Operation { operation: self.coordinator.operation()? }),
            RequestKind::Verify { selection } => {
                let (_, _lease) = self.verified(&selection)?;
                Ok(ReplyKind::Verification { readiness: Readiness::Ready, selection: Some(selection) })
            },
            RequestKind::Acquire { .. } => Err(BridgeError::new("EPROTOCOL", "A transport lease requires descriptor transfer.")),
            RequestKind::BeginAuthorization { .. } | RequestKind::ApproveAuthorization { .. } =>
                Err(BridgeError::new("EPROTOCOL", "macOS authorization requires an Authorization Services credential.")),
        }
    }
    fn verified(&mut self, selection: &BridgeSelection) -> Result<(Resource<Snapshot>, ResourceLease), BridgeError> {
        let id = selection.resource_id.as_ref().ok_or_else(|| BridgeError::new("ENEEDSSETUP", "Choose a managed bridge connection first."))?;
        let (resource, lease) = self.coordinator.verify_resource(id)?;
        if selection.version != VERSION || selection.platform != Platform::Macos
            || selection.adapter_id != resource.adapter_id || selection.fingerprint != resource.fingerprint {
            return Err(BridgeError::new("ESTALE", "The saved bridge connection has changed."));
        }
        Ok((resource, lease))
    }
    fn inspect(&mut self) -> Result<Inspection, BridgeError> {
        let mut inspection = crate::inspect()?;
        inspection.support = SupportState::Ready;
        // A service's environment cannot describe the desktop's inherited
        // launch overrides. The desktop adds its own value to this snapshot.
        inspection.external_override = None;
        inspection.active_operation = self.coordinator.operation()?;
        if let Some(resource) = self.coordinator.resources()?.last() {
            let selection = BridgeSelection { version: VERSION, platform: Platform::Macos,
                adapter_id: resource.adapter_id.clone(), fingerprint: resource.fingerprint.clone(), resource_id: Some(resource.id.clone()),
                display_name: inspection.adapters.iter().find(|a| a.id == resource.adapter_id)
                    .map(|a| a.name.clone()).unwrap_or_else(|| resource.adapter_id.as_str().into()) };
            inspection.readiness = selection_readiness(&selection, Platform::Macos, &inspection.adapters);
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
