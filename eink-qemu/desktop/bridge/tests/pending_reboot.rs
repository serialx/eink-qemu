//! Driver installation may require a restart before any host membership change.
use eink_bridge::{coordinator::{Coordinator,PlatformOps,Prepared,Resource},model::*};
use serde::{Serialize,Deserialize};
use std::sync::{Arc,Mutex};
#[derive(Clone,Serialize,Deserialize)]
struct Snapshot { reboot: bool }
#[derive(Clone)]
struct Host { installed:Arc<Mutex<bool>>, mutations:Arc<Mutex<Vec<u8>>>, booted:bool, changed:bool }
impl Host {fn new()->Self {Self{installed:Arc::new(Mutex::new(false)),mutations:Arc::new(Mutex::new(vec![])),booted:false,changed:false}}}
impl PlatformOps for Host {
 type Snapshot=Snapshot;type Mutation=u8;
 fn prepare(&mut self,request:PlanRequest,id:OperationId,now:u64,_:Option<&Resource<Snapshot>>)->Result<Prepared<Snapshot,u8>,BridgeError> {
  Ok(Prepared{plan:BridgePlan{id,request,fingerprint:"original".into(),changes:vec!["Install a driver, then configure Ethernet".into()],requires_authorization:true,may_disconnect:true,expires_at:now+300},snapshot:Snapshot{reboot:false},mutations:vec![0,1]})
 }
 fn fingerprint(&mut self,_:&PlanRequest,_:&Snapshot)->Result<String,BridgeError>{Ok(if self.changed{"changed"}else{"original"}.into())}
 fn apply(&mut self,_:&u8)->Result<(),BridgeError>{unreachable!()}
 fn apply_recorded(&mut self,step:&u8,snapshot:&mut Snapshot)->Result<(),BridgeError>{self.mutations.lock().unwrap().push(*step);if *step==0{*self.installed.lock().unwrap()=true;snapshot.reboot=true;}Ok(())}
 fn awaiting_reboot(&self,snapshot:&Snapshot)->bool{snapshot.reboot}
 fn resume_after_reboot(&mut self,snapshot:&mut Snapshot)->Result<(),BridgeError>{if !self.booted{return Err(BridgeError::new("EREBOOT","Restart first"));}snapshot.reboot=false;Ok(())}
 fn verify(&mut self,_:&Snapshot)->Result<(),BridgeError>{Ok(())}
 fn commit(&mut self,_:&Snapshot)->Result<(),BridgeError>{Ok(())}
 fn rollback(&mut self,_:&Snapshot)->Result<(),BridgeError>{*self.installed.lock().unwrap()=false;Ok(())}
}
fn begin(root:&std::path::Path,host:Host)->(Coordinator<Host>,OperationId){
 let mut c=Coordinator::open(root,"owner",host).unwrap();
 let plan=c.plan(PlanRequest::Configure{adapter_id:AdapterId::new("physical").unwrap()},100).unwrap();
 let operation=c.apply(&plan.id,101).unwrap();
 assert!(operation.reboot_required);assert_eq!(operation.state,OperationState::Applying);assert!(operation.confirmation_deadline.is_none());
 (c,plan.id)
}
#[test]
fn reboot_barrier_survives_watchdog_and_restart_without_touching_host_membership(){
 let root=tempfile::tempdir().unwrap();let host=Host::new();let(mut c,id)=begin(root.path(),host.clone());
 assert_eq!(*host.mutations.lock().unwrap(),vec![0]);
 assert_eq!(c.keep(&id,102).unwrap_err().code,"ESTALE");
 assert!(c.resources().unwrap().is_empty());
 c.recover(999).unwrap();drop(c);
 let service=Coordinator::open(root.path(),"service",host.clone()).unwrap();
 let worker=eink_bridge::recovery::RecoveryWorker::start(service,std::time::Duration::from_millis(20),||999).unwrap();
 worker.health().unwrap();worker.stop().unwrap();
 assert!(*host.installed.lock().unwrap());assert_eq!(*host.mutations.lock().unwrap(),vec![0]);
 let mut c=Coordinator::open(root.path(),"owner",host.clone()).unwrap();
 assert_eq!(c.apply(&id,1000).unwrap_err().code,"EREBOOT");
 assert!(c.operation().unwrap().unwrap().reboot_required);
 let mut booted=host.clone();booted.booted=true;
 let mut other=Coordinator::open(root.path(),"other",booted.clone()).unwrap();assert_eq!(other.apply(&id,1000).unwrap_err().code,"EPERM");
 let mut c=Coordinator::open(root.path(),"owner",booted).unwrap();
 let op=c.apply(&id,1000).unwrap();assert!(!op.reboot_required);assert_eq!(op.state,OperationState::Verifying);
 assert!(op.confirmation_deadline.unwrap()>=1120);assert_eq!(*host.mutations.lock().unwrap(),vec![0,1],"never reinstall the driver on resume");
 assert_eq!(c.keep(&id,1001).unwrap().state,OperationState::Ready);
}
#[test]
fn changed_network_refuses_resume_and_owner_can_cancel_the_driver_only_stage(){
 let root=tempfile::tempdir().unwrap();let mut host=Host::new();let(c,id)=begin(root.path(),host.clone());drop(c);
 host.booted=true;host.changed=true;let mut c=Coordinator::open(root.path(),"owner",host.clone()).unwrap();
 assert_eq!(c.apply(&id,1000).unwrap_err().code,"ESTALE");assert_eq!(*host.mutations.lock().unwrap(),vec![0]);
 let op=c.cancel(&id).unwrap();assert_eq!(op.state,OperationState::Cancelled);assert!(!op.reboot_required);assert!(!*host.installed.lock().unwrap());
}
