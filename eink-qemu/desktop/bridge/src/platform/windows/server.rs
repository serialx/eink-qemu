//! Installed service loop. Recovery and approval ownership outlive all desktop
//! connections; individual clients cannot stop the watchdog or change paths.
use crate::{authorization::windows::GrantStore,coordinator::Coordinator,journal::Journal,model::*,
    protocol::{HelperReply,ReplyKind,RequestKind},recovery::RecoveryWorker,transport::{self,windows::{Listener,Peer}},
    windows_dispatch::{Dispatcher,WindowsPlatformOps},windows_security};
use std::{fs::{File,OpenOptions},os::windows::{fs::OpenOptionsExt,io::AsRawHandle},path::Path,
    sync::{Arc,Mutex,atomic::{AtomicBool,AtomicUsize,Ordering}},thread::JoinHandle,time::{Duration,SystemTime,UNIX_EPOCH}};
use windows_sys::Win32::Storage::FileSystem::*;
fn io(error:impl std::fmt::Display)->BridgeError{BridgeError::new("ESERVICE",error.to_string())}
fn now()->u64{SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()}
struct InstallationLock { _directories:Vec<File>, _file:File }
impl InstallationLock {
    fn open(root:&Path)->Result<Self,BridgeError>{
        if !root.is_absolute() || root.components().any(|part|matches!(part,std::path::Component::ParentDir)){return Err(io("Invalid installed service directory."));}
        let mut directories=Vec::new();
        for ancestor in root.ancestors().collect::<Vec<_>>().into_iter().rev(){
            let file=OpenOptions::new().read(true).share_mode(FILE_SHARE_READ|FILE_SHARE_WRITE)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS|FILE_FLAG_OPEN_REPARSE_POINT).open(ancestor).map_err(io)?;
            let mut info=BY_HANDLE_FILE_INFORMATION::default();
            if unsafe{GetFileInformationByHandle(file.as_raw_handle(),&mut info)}==0
                || info.dwFileAttributes&FILE_ATTRIBUTE_REPARSE_POINT!=0 || info.dwFileAttributes&FILE_ATTRIBUTE_DIRECTORY==0{
                return Err(io("Service directories cannot be reparse points."));
            }
            if ancestor==root{windows_security::validate_handle(&file,None)?;}
            directories.push(file);
        }
        // Installer creates the private directory. Never take ownership of an
        // arbitrary pre-existing directory on behalf of an IPC client.
        Journal::open(root)?;
        let file=OpenOptions::new().read(true).write(true).create(true).share_mode(0)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT).open(root.join("service.lock"))
            .map_err(|error|if error.raw_os_error()==Some(32){BridgeError::new("EBUSY","The bridge service is already running.")}else{io(error)})?;
        windows_security::validate_private_handle(&file,"S-1-5-18")?;
        if file.metadata().map_err(io)?.len()!=0{return Err(io("Invalid service lock."));}
        Ok(Self{_directories:directories,_file:file})
    }
}
fn health(worker:&Mutex<Result<RecoveryWorker,BridgeError>>)->Result<(),BridgeError>{
    match &*worker.lock().map_err(|_|io("Recovery state is unavailable."))?{Ok(worker)=>worker.health(),Err(error)=>Err(error.clone())}
}
fn start<P:WindowsPlatformOps+Send+'static>(root:&Path,platform:P)->Result<RecoveryWorker,BridgeError>{
    RecoveryWorker::start(Coordinator::open(root,"service",platform)?,Duration::from_secs(1),now)
}
/// Called by the fixed SCM entry point with factories for the native platform.
/// The installer owns `root`; factories and readiness callbacks are native code,
/// never request data. Stop waits for bounded in-flight work, then restores any
/// unconfirmed operation before releasing the service lock.
pub fn serve<P,F,R,N>(root:&Path,for_peer:F,for_recovery:R,stop:Arc<AtomicBool>,ready:N)->Result<(),BridgeError>
where P:WindowsPlatformOps+Send+'static,F:Fn(&Peer)->Result<P,BridgeError>+Send+Sync+'static,
    R:Fn()->Result<P,BridgeError>+Send+Sync+'static,N:FnOnce()->Result<(),BridgeError>{
    windows_security::require_system()?;
    let _installation=InstallationLock::open(root)?;
    let root=root.to_path_buf();let for_peer=Arc::new(for_peer);let for_recovery=Arc::new(for_recovery);
    let worker=Arc::new(Mutex::new(for_recovery().and_then(|platform|start(&root,platform))));
    let approvals=Arc::new(Mutex::new(GrantStore::default()));
    let mut listener=Listener::bind()?;
    ready()?;
    let active=Arc::new(AtomicUsize::new(0));let mut clients:Vec<JoinHandle<()>>=Vec::new();
    let result=loop{
        if stop.load(Ordering::Acquire){break Ok(());}
        // Keep join handles bounded as well as active clients.
        let mut index=0;
        while index<clients.len(){if clients[index].is_finished(){let _=clients.swap_remove(index).join();}else{index+=1;}}
        let mut pipe=match listener.accept(Duration::from_millis(100)){
            Ok(Some(pipe))=>pipe,Ok(None)=>continue,Err(error)=>break Err(error),
        };
        if active.load(Ordering::Acquire)>=16{continue;}
        active.fetch_add(1,Ordering::AcqRel);
        let count=active.clone();let root=root.clone();let factory=for_peer.clone();let recover=for_recovery.clone();
        let worker=worker.clone();let approvals=approvals.clone();
        let client=std::thread::Builder::new().name("bridge-request".into()).spawn(move||{
            struct Active(Arc<AtomicUsize>);
            impl Drop for Active{fn drop(&mut self){self.0.fetch_sub(1,Ordering::AcqRel);}}
            let _active=Active(count);
            let _result=(||->Result<(),BridgeError>{
                // Named-pipe impersonation authenticates the most recently
                // read message. Read one bounded frame before acquiring Peer.
                let request=transport::read_request(&mut pipe)?;
                let peer=pipe.peer()?;let platform=factory(&peer)?;
                let mut dispatcher=Dispatcher::new(&root,peer,platform,approvals)?;
                if let Err(error)=health(&worker){
                    if !matches!(request.request,RequestKind::Inspect|RequestKind::Recover|RequestKind::Cancel{..}){
                        pipe.timeout(Duration::from_secs(5));
                        transport::write_reply(&mut pipe,&HelperReply::new(ReplyKind::Error{error}))?;
                        return pipe.finish_response();
                    }
                }
                let retry=matches!(request.request,RequestKind::Cancel{..});
                let (reply,lease)=dispatcher.request(request);
                if retry && matches!(&reply.reply,ReplyKind::Operation{operation:Some(operation)} if matches!(operation.state,OperationState::Cancelled|OperationState::Ready))
                    && health(&worker).is_err(){
                    *worker.lock().map_err(|_|io("Recovery state is unavailable."))?=recover().and_then(|platform|start(&root,platform));
                }
                // Native setup may have consumed the input deadline. Give the
                // response its own bounded write/ACK window.
                pipe.timeout(Duration::from_secs(5));
                transport::write_reply(&mut pipe,&reply)?;
                if let Some(lease)=lease{
                    pipe.send_lease(dispatcher.peer(),lease.file())?;
                    // Keep the original lease until the receiver acknowledges
                    // its duplicate, even if the desktop disconnects midway.
                    pipe.finish_response()?;
                }else{pipe.finish_response()?;}
                Ok(())
            })();
        });
        match client{Ok(client)=>clients.push(client),Err(error)=>{active.fetch_sub(1,Ordering::AcqRel);break Err(io(error));}}
    };
    drop(listener);
    for client in clients{let _=client.join();}
    drop(worker); // Stop and join the watchdog before final reconciliation.
    let recovery=Coordinator::open(&root,"service",for_recovery()?)?.recover_startup();
    result?;
    if let Some(operation)=recovery?{
        if operation.state==OperationState::Failed{
            if let Some(error)=operation.error.filter(|error|matches!(error.code.as_str(),"EROLLBACK"|"ECLEANUP")){return Err(error);}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests{
    use super::*;
    use crate::{coordinator::{PlatformOps,Prepared,Resource},protocol::{HelperRequest,RequestKind},transport::HelperClient};
    #[derive(Clone,Default)]
    struct NoNetwork{restored:Arc<AtomicUsize>}
    impl PlatformOps for NoNetwork{
        type Snapshot=();type Mutation=();
        fn prepare(&mut self,request:PlanRequest,id:OperationId,now:u64,_:Option<&Resource<()>>)->Result<Prepared<(),()>,BridgeError>{
            let may_disconnect=matches!(&request,PlanRequest::Configure{adapter_id} if adapter_id.as_str()=="pending");
            Ok(Prepared{plan:BridgePlan{id,request,fingerprint:"service-test".into(),changes:vec!["Test without network changes".into()],
                requires_authorization:true,may_disconnect,expires_at:now+300},snapshot:(),mutations:vec![()]})
        }
        fn fingerprint(&mut self,_:&PlanRequest,_:&())->Result<String,BridgeError>{Ok("service-test".into())}
        fn apply(&mut self,_:&())->Result<(),BridgeError>{Ok(())}
        fn verify(&mut self,_:&())->Result<(),BridgeError>{Ok(())}
        fn commit(&mut self,_:&())->Result<(),BridgeError>{Ok(())}
        fn rollback(&mut self,_:&())->Result<(),BridgeError>{self.restored.fetch_add(1,Ordering::SeqCst);Ok(())}
    }
    impl WindowsPlatformOps for NoNetwork{
        fn target(_:&())->Result<LaunchTarget,BridgeError>{Ok(LaunchTarget::WindowsTap{name:"test-only".into()})}
    }
    struct Running{stop:Arc<AtomicBool>,thread:Option<JoinHandle<Result<(),BridgeError>>>}
    impl Running{
        fn start(root:&Path,backend:NoNetwork)->Self{
            let root=root.to_path_buf();let stop=Arc::new(AtomicBool::new(false));let flag=stop.clone();
            let (send,receive)=std::sync::mpsc::channel();
            let thread=std::thread::spawn(move||{let recovery=backend.clone();serve(&root,move|_|Ok(backend.clone()),move||Ok(recovery.clone()),flag,
                move||{send.send(()).unwrap();Ok(())})});
            receive.recv_timeout(Duration::from_secs(10)).unwrap();
            Self{stop,thread:Some(thread)}
        }
        fn stop(mut self){self.stop.store(true,Ordering::Release);self.thread.take().unwrap().join().unwrap().unwrap();}
    }
    impl Drop for Running{fn drop(&mut self){self.stop.store(true,Ordering::Release);if let Some(thread)=self.thread.take(){let _=thread.join();}}}
    fn request(request:RequestKind)->Result<ReplyKind,BridgeError>{
        Ok(HelperClient::connect()?.request(HelperRequest{version:VERSION,request})?.reply)
    }
    fn plan(adapter:&str)->BridgePlan{
        match request(RequestKind::Plan{request:PlanRequest::Configure{adapter_id:AdapterId::new(adapter).unwrap()}}).unwrap(){ReplyKind::Plan{plan}=>plan,other=>panic!("{other:?}")}
    }
    fn approve(plan:&BridgePlan)->crate::authorization::AuthorizationProof{
        let ReplyKind::Authorization{authorization}=request(RequestKind::BeginAuthorization{plan_id:plan.id.clone()}).unwrap()else{panic!("Missing proof")};
        assert!(matches!(request(RequestKind::ApproveAuthorization{authorization:authorization.clone()}).unwrap(),ReplyKind::Authorized));authorization
    }
    #[test]
    #[ignore="requires LocalSystem and an unused endpoint in the disposable VM; service-loop test makes no network changes"]
    fn native_service_connections_restart_approval_and_surviving_lease(){
        windows_security::require_system().unwrap();
        assert_eq!(std::env::var("EINK_BRIDGE_DISPOSABLE_WINDOWS_TESTS").as_deref(),Ok("1"));
        let root=tempfile::tempdir().unwrap();let backend=NoNetwork::default();
        let server=Running::start(root.path(),backend.clone());
        let reviewed=plan("configured");let proof=approve(&reviewed);
        assert!(matches!(request(RequestKind::Apply{plan_id:reviewed.id.clone(),authorization:Some(proof)}).unwrap(),ReplyKind::Operation{operation:Some(operation)} if operation.state==OperationState::Ready));
        let id=ResourceId::new(reviewed.id.as_str()).unwrap();
        let selection=BridgeSelection{version:VERSION,platform:Platform::Windows,adapter_id:AdapterId::new("configured").unwrap(),
            fingerprint:reviewed.fingerprint,resource_id:Some(id.clone()),display_name:"test".into()};
        let mut pipe=transport::windows::connect().unwrap();
        let reply=transport::exchange(&mut pipe,&HelperRequest{version:VERSION,request:RequestKind::Acquire{selection}}).unwrap();
        assert!(matches!(reply.reply,ReplyKind::Lease{..}));
        let lease=pipe.receive_lease().unwrap();pipe.acknowledge().unwrap();drop(pipe);
        assert_eq!(request(RequestKind::Plan{request:PlanRequest::Remove{resource_id:id.clone()}}).unwrap_err().code,"EBUSY");
        let next=plan("pending");let obsolete=approve(&next);
        server.stop();
        let server=Running::start(root.path(),backend.clone());
        assert_eq!(request(RequestKind::Apply{plan_id:next.id.clone(),authorization:Some(obsolete)}).unwrap_err().code,"EAUTHDENIED");
        assert_eq!(request(RequestKind::Plan{request:PlanRequest::Remove{resource_id:id.clone()}}).unwrap_err().code,"EBUSY");
        drop(lease);
        let proof=approve(&next);
        assert!(matches!(request(RequestKind::Apply{plan_id:next.id.clone(),authorization:Some(proof)}).unwrap(),ReplyKind::Operation{operation:Some(operation)} if operation.state==OperationState::Verifying));
        server.stop();
        assert_eq!(backend.restored.load(Ordering::SeqCst),1,"Stopping restores unconfirmed setup independently of the client");
        let server=Running::start(root.path(),backend.clone());
        assert!(matches!(request(RequestKind::Recover).unwrap(),ReplyKind::Operation{operation:Some(operation)} if operation.state==OperationState::Cancelled));
        assert!(matches!(request(RequestKind::Plan{request:PlanRequest::Remove{resource_id:id}}).unwrap(),ReplyKind::Plan{..}));
        server.stop();
    }
}
