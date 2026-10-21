//! One request from a kernel-authenticated named-pipe peer. The service shares
//! its volatile approval store across connections; no approval enters a journal.
use crate::{authorization::windows::GrantStore, coordinator::{Coordinator, PlatformOps, Resource, ResourceLease},
    model::*, protocol::*, transport::windows::Peer};
use std::{path::Path, sync::{Arc, Mutex}, time::{SystemTime, UNIX_EPOCH}};

pub type Approvals = Arc<Mutex<GrantStore>>;
pub trait WindowsPlatformOps: PlatformOps {
    fn target(snapshot: &Self::Snapshot) -> Result<LaunchTarget, BridgeError>;
}
fn now() -> u64 { SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() }
fn denied() -> BridgeError { BridgeError::new("EAUTHDENIED", "Approve the current bridge plan through the Windows administrator prompt.") }
pub struct Dispatcher<P: WindowsPlatformOps> {
    coordinator: Coordinator<P>, peer: Peer, approvals: Approvals,
}
impl<P: WindowsPlatformOps> Dispatcher<P> {
    /// `root` and `platform` come from the installed service, never IPC. The
    /// peer remains pinned for the entire request, including native mutation.
    pub fn new(root: &Path, peer: Peer, platform: P, approvals: Approvals) -> Result<Self,BridgeError> {
        crate::windows_security::require_system()?;
        peer.revalidate()?;
        let coordinator=Coordinator::open(root,peer.caller().principal(),platform)?;
        Ok(Self {coordinator,peer,approvals})
    }
    pub fn peer(&self) -> &Peer { &self.peer }
    pub fn request(&mut self, request: HelperRequest) -> (HelperReply,Option<ResourceLease>) {
        let result=if request.version!=VERSION {
            Err(BridgeError::new("EPROTOCOL","Bridge helper version does not match the app."))
        } else {
            self.peer.revalidate().and_then(|_| {
                if let RequestKind::Acquire {selection}=&request.request {
                    let (resource,lease)=self.verified(selection)?;
                    Ok((ReplyKind::Lease {selection:selection.clone(),target:P::target(&resource.original)?},Some(lease)))
                } else {self.dispatch(request.request).map(|reply|(reply,None))}
            })
        };
        match result {
            Ok((reply,lease))=>(HelperReply::new(reply),lease),
            Err(error)=>(HelperReply::new(ReplyKind::Error {error}),None),
        }
    }
    fn dispatch(&mut self, request: RequestKind) -> Result<ReplyKind,BridgeError> {
        match request {
            RequestKind::Inspect=>Ok(ReplyKind::Inspection {inspection:self.inspect()?}),
            RequestKind::Plan {request}=>Ok(ReplyKind::Plan {plan:self.coordinator.plan(request,now())?}),
            RequestKind::BeginAuthorization {plan_id}=>{
                let (plan,revision)=self.coordinator.reviewed_plan(&plan_id,now())?;
                let authorization=self.approvals.lock().map_err(|_|denied())?.begin(&self.peer,plan,revision)?;
                Ok(ReplyKind::Authorization {authorization})
            },
            RequestKind::ApproveAuthorization {authorization}=>{
                self.approvals.lock().map_err(|_|denied())?.approve(&self.peer,&authorization)?;
                Ok(ReplyKind::Authorized)
            },
            RequestKind::Apply {plan_id,authorization}=>{
                let proof=authorization.as_ref().ok_or_else(denied)?;
                let (plan,revision)=self.coordinator.reviewed_plan(&plan_id,now())?;
                self.approvals.lock().map_err(|_|denied())?.consume(&self.peer,proof,&plan,revision)?;
                // The grant is consumed even if the native fingerprint changed.
                // Coordinator rechecks the protected plan under its own lock.
                Ok(ReplyKind::Operation {operation:Some(self.coordinator.apply(&plan_id,now())?)})
            },
            RequestKind::Keep {operation_id}=>Ok(ReplyKind::Operation {operation:Some(self.coordinator.keep(&operation_id,now())?)}),
            RequestKind::Cancel {operation_id}=>Ok(ReplyKind::Operation {operation:Some(self.coordinator.cancel(&operation_id)?)}),
            RequestKind::Recover=>Ok(ReplyKind::Operation {operation:self.coordinator.operation()?}),
            RequestKind::Verify {selection}=>{
                let (_, _lease)=self.verified(&selection)?;
                Ok(ReplyKind::Verification {readiness:Readiness::Ready,selection:Some(selection)})
            },
            RequestKind::Acquire {..}=>Err(BridgeError::new("EPROTOCOL","Acquiring a bridge requires handle transfer.")),
        }
    }
    fn verified(&mut self, selection: &BridgeSelection) -> Result<(Resource<P::Snapshot>,ResourceLease),BridgeError> {
        let id=selection.resource_id.as_ref().ok_or_else(||BridgeError::new("ENEEDSSETUP","Choose a managed bridge connection first."))?;
        let (resource,lease)=self.coordinator.verify_resource(id)?;
        if selection.version!=VERSION || selection.platform!=Platform::Windows
            || selection.adapter_id!=resource.adapter_id || selection.fingerprint!=resource.fingerprint {
            return Err(BridgeError::new("ESTALE","The saved bridge connection has changed."));
        }
        Ok((resource,lease))
    }
    fn inspect(&mut self) -> Result<Inspection,BridgeError> {
        let mut inspection=crate::inspect()?;
        inspection.support=SupportState::Ready;
        // This endpoint is served only after the native runtime has verified
        // its pinned x64 driver package. Local discovery cannot grant setup.
        inspection.capabilities=Capabilities {create:true,existing_bridge:false,reason:None};
        inspection.external_override=None;
        inspection.active_operation=self.coordinator.operation()?;
        if let Some(resource)=self.coordinator.resources()?.last() {
            let selection=BridgeSelection {version:VERSION,platform:Platform::Windows,
                adapter_id:resource.adapter_id.clone(),fingerprint:resource.fingerprint.clone(),resource_id:Some(resource.id.clone()),
                display_name:inspection.adapters.iter().find(|adapter|adapter.id==resource.adapter_id)
                    .map(|adapter|adapter.name.clone()).unwrap_or_else(||resource.adapter_id.as_str().into())};
            inspection.readiness=selection_readiness(&selection,Platform::Windows,&inspection.adapters);
            if inspection.readiness==Readiness::NeedsSetup {
                inspection.readiness=match self.coordinator.verify_resource(&resource.id) {
                    Ok(_)=>Readiness::Ready,
                    Err(error) if error.code=="EBUSY"=>Readiness::NeedsSetup,
                    Err(_)=>Readiness::NeedsRepair,
                };
            }
            inspection.selection=Some(selection);
        }
        Ok(inspection)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{authorization::AuthorizationProof,coordinator::Prepared,transport::windows};
    use std::{io::{Read,Write},sync::atomic::{AtomicUsize,Ordering},time::Duration};
    struct NoNetwork(Arc<AtomicUsize>);
    impl PlatformOps for NoNetwork {
        type Snapshot=();type Mutation=();
        fn prepare(&mut self,request:PlanRequest,id:OperationId,now:u64,_:Option<&Resource<()>>)->Result<Prepared<(),()>,BridgeError>{
            Ok(Prepared {plan:BridgePlan {id,request,fingerprint:"test-reviewed-network".into(),changes:vec!["Test without network changes".into()],
                requires_authorization:true,may_disconnect:false,expires_at:now+300},snapshot:(),mutations:vec![()]})
        }
        fn fingerprint(&mut self,_:&PlanRequest,_:&())->Result<String,BridgeError>{Ok("test-reviewed-network".into())}
        fn apply(&mut self,_:&())->Result<(),BridgeError>{self.0.fetch_add(1,Ordering::SeqCst);Ok(())}
        fn verify(&mut self,_:&())->Result<(),BridgeError>{Ok(())}
        fn commit(&mut self,_:&())->Result<(),BridgeError>{Ok(())}
        fn rollback(&mut self,_:&())->Result<(),BridgeError>{Ok(())}
    }
    impl WindowsPlatformOps for NoNetwork {
        fn target(_:&())->Result<LaunchTarget,BridgeError>{Ok(LaunchTarget::WindowsTap {name:"test-only".into()})}
    }
    fn call(dispatcher:&mut Dispatcher<NoNetwork>,request:RequestKind)->ReplyKind{
        dispatcher.request(HelperRequest {version:VERSION,request}).0.reply
    }
    fn rejected(reply:ReplyKind,code:&str){match reply{ReplyKind::Error {error}=>assert_eq!(error.code,code),other=>panic!("Expected rejection: {other:?}")}}
    fn plan(dispatcher:&mut Dispatcher<NoNetwork>,adapter:&str)->BridgePlan{
        match call(dispatcher,RequestKind::Plan {request:PlanRequest::Configure {adapter_id:AdapterId::new(adapter).unwrap()}}){
            ReplyKind::Plan {plan}=>plan,other=>panic!("{other:?}")
        }
    }
    fn begin(dispatcher:&mut Dispatcher<NoNetwork>,plan:&BridgePlan)->AuthorizationProof{
        match call(dispatcher,RequestKind::BeginAuthorization {plan_id:plan.id.clone()}){
            ReplyKind::Authorization {authorization}=>authorization,other=>panic!("{other:?}")
        }
    }
    #[test]
    #[ignore="requires LocalSystem and an unused bridge endpoint in the disposable Windows VM; no network changes"]
    fn native_peer_dispatch_requires_current_one_use_approval_and_returns_a_lease(){
        crate::windows_security::require_system().unwrap();
        assert_eq!(std::env::var("EINK_BRIDGE_DISPOSABLE_WINDOWS_TESTS").as_deref(),Ok("1"));
        let mut listener=windows::Listener::bind().unwrap();
        let mut client=windows::connect().unwrap();client.write_all(&[1]).unwrap();
        let mut server=listener.accept(Duration::from_secs(5)).unwrap().unwrap();
        let mut marker=[0];server.read_exact(&mut marker).unwrap();
        let peer=server.peer().unwrap();
        let root=tempfile::tempdir().unwrap();
        let count=Arc::new(AtomicUsize::new(0));let approvals=Arc::new(Mutex::new(GrantStore::default()));
        let mut dispatcher=Dispatcher::new(root.path(),peer,NoNetwork(count.clone()),approvals.clone()).unwrap();
        let reviewed=plan(&mut dispatcher,"test-adapter");
        rejected(call(&mut dispatcher,RequestKind::Apply {plan_id:reviewed.id.clone(),authorization:None}),"EAUTHDENIED");
        let unapproved=begin(&mut dispatcher,&reviewed);
        rejected(call(&mut dispatcher,RequestKind::Apply {plan_id:reviewed.id.clone(),authorization:Some(unapproved.clone())}),"EAUTHDENIED");
        rejected(call(&mut dispatcher,RequestKind::ApproveAuthorization {authorization:unapproved}),"EAUTHDENIED");
        let obsolete=begin(&mut dispatcher,&reviewed);
        assert!(matches!(call(&mut dispatcher,RequestKind::ApproveAuthorization {authorization:obsolete.clone()}),ReplyKind::Authorized));
        call(&mut dispatcher,RequestKind::Cancel {operation_id:reviewed.id.clone()});
        let reviewed=plan(&mut dispatcher,"test-adapter");
        rejected(call(&mut dispatcher,RequestKind::Apply {plan_id:reviewed.id.clone(),authorization:Some(obsolete)}),"EAUTHDENIED");
        assert_eq!(count.load(Ordering::SeqCst),0);
        let proof=begin(&mut dispatcher,&reviewed);
        assert!(matches!(call(&mut dispatcher,RequestKind::ApproveAuthorization {authorization:proof.clone()}),ReplyKind::Authorized));
        rejected(call(&mut dispatcher,RequestKind::ApproveAuthorization {authorization:proof.clone()}),"EAUTHDENIED");
        let operation=call(&mut dispatcher,RequestKind::Apply {plan_id:reviewed.id.clone(),authorization:Some(proof.clone())});
        assert!(matches!(operation,ReplyKind::Operation {operation:Some(operation)} if operation.state==OperationState::Ready));
        assert_eq!(count.load(Ordering::SeqCst),1);
        rejected(call(&mut dispatcher,RequestKind::ApproveAuthorization {authorization:proof}),"EAUTHDENIED");
        let selection=BridgeSelection {version:VERSION,platform:Platform::Windows,adapter_id:AdapterId::new("test-adapter").unwrap(),
            fingerprint:reviewed.fingerprint,resource_id:Some(ResourceId::new(reviewed.id.as_str()).unwrap()),display_name:"test".into()};
        let (reply,lease)=dispatcher.request(HelperRequest {version:VERSION,request:RequestKind::Acquire {selection:selection.clone()}});
        assert!(matches!(reply.reply,ReplyKind::Lease {..}));assert!(lease.is_some());
        let changed=BridgeSelection {fingerprint:"replaced".into(),..selection};
        rejected(call(&mut dispatcher,RequestKind::Verify {selection:changed}),"ESTALE");
        let later=plan(&mut dispatcher,"another-adapter");let proof=begin(&mut dispatcher,&later);
        assert!(matches!(call(&mut dispatcher,RequestKind::ApproveAuthorization {authorization:proof.clone()}),ReplyKind::Authorized));
        *approvals.lock().unwrap()=GrantStore::default();
        rejected(call(&mut dispatcher,RequestKind::Apply {plan_id:later.id,authorization:Some(proof)}),"EAUTHDENIED");
        assert_eq!(count.load(Ordering::SeqCst),1);
    }
}
