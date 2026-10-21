//! Full retained-resource lifecycle in the named disposable VM only.
#[cfg(not(windows))]
fn main(){panic!("Requires Windows");}
#[cfg(windows)]
fn main(){if let Err(error)=run(){eprintln!("{error:?}");std::process::exit(1);}}
#[cfg(windows)]
fn run()->Result<(),Box<dyn std::error::Error>>{
    use eink_bridge::{coordinator::Coordinator,model::*,windows_device,windows_driver,
        windows_network_bridge::{self,HostProtocol},windows_platform::WindowsPlatform,
        windows_profile::Snapshot,windows_security,windows_topology::Topology};
    use std::{fs,io::Write,path::Path,time::{SystemTime,UNIX_EPOCH}};
    fn now()->u64{SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()}
    fn record(path:&Path,value:&impl serde::Serialize)->Result<(),Box<dyn std::error::Error>>{
        let mut file=fs::OpenOptions::new().write(true).create_new(true).open(path)?;
        windows_security::validate_private_handle(&file,"S-1-5-18")?;
        serde_json::to_writer(&mut file,value)?;file.flush()?;file.sync_all()?;Ok(())
    }
    fn confirm(c:&mut Coordinator<WindowsPlatform>,request:PlanRequest)->Result<(),Box<dyn std::error::Error>>{
        let plan=c.plan(request,now())?;
        let applied=c.apply(&plan.id,now())?;
        println!("applied={}",serde_json::to_string(&applied)?);
        if applied.state!=OperationState::Verifying || applied.confirmation_deadline.is_none() || applied.error.is_some(){return Err("Operation did not reach confirmation".into());}
        let kept=c.keep(&plan.id,now())?;
        if kept.state!=OperationState::Ready || kept.error.is_some(){return Err(format!("Confirmation failed: {kept:?}").into());}
        Ok(())
    }
    windows_security::require_system()?;
    if std::env::var("COMPUTERNAME").as_deref()!=Ok("WIN-SUNSKHPNVMO"){return Err("Wrong disposable host".into());}
    let root=Path::new(r"C:\eink-driver-native-gate\lifecycle-native-v2");
    let package=Path::new(r"C:\eink-driver-native-gate\driver");
    let args=std::env::args().skip(1).collect::<Vec<_>>();
    let mut coordinator=Coordinator::open(root,"sid:S-1-5-18",WindowsPlatform::for_user("sid:S-1-5-18",package)?)?;
    let id=AdapterId::new("{5238c509-ecda-49fb-bfdc-fd0be1922ff5}")?;
    let physical=windows_device::enumerate()?.get(id.as_str()).cloned().ok_or("Missing Ethernet")?;
    if args==["--verify-original"]{
        let file=fs::File::open(root.join("physical-before.json"))?;windows_security::validate_private_handle(&file,"S-1-5-18")?;
        let before:Snapshot=serde_json::from_reader(file)?;let after=Snapshot::read(&physical)?;
        if before.instance_id!=physical.instance_id || !before.configuration_changes(&after).is_empty() || !before.runtime_changes(&after)?.is_empty(){return Err("Original Ethernet did not return".into());}
        record(&root.join("physical-after-recovery.json"),&after)?;
        println!("nativeLifecycleOriginalNetwork=passed");return Ok(());
    }
    if args==["--cleanup"]{
        if let Some(operation)=coordinator.recover_startup()?{
            if !matches!(operation.state,OperationState::Ready|OperationState::Cancelled){
                let restored=coordinator.cancel(&operation.id)?;
                if !matches!(restored.state,OperationState::Ready|OperationState::Cancelled){return Err("Pending operation requires inspection".into());}
            }
        }
        let resources=coordinator.resources()?;
        if resources.len()>1{return Err("Unexpected fixture resources".into());}
        for resource in resources{confirm(&mut coordinator,PlanRequest::Remove{resource_id:resource.id})?;}
        println!("nativeLifecycleCleanup=passed");return Ok(());
    }
    if args!=["--exercise"]{return Err("Unknown lifecycle gate invocation".into());}
    for name in ["network-device.json","network-pair.json"]{if root.parent().unwrap().join(name).exists(){return Err("Another fixture is active".into());}}
    if windows_device::enumerate()?.values().any(|d|d.instance_id.starts_with(r"ROOT\EINKQEMUTAP\")){return Err("A dedicated TAP already exists".into());}
    let armed=fs::File::open(root.join("recovery-armed"))?;windows_security::validate_private_handle(&armed,"S-1-5-18")?;
    let before=Snapshot::read(&physical)?;record(&root.join("physical-before.json"),&before)?;
    println!("nativeLifecycleStage=configure-keep");
    confirm(&mut coordinator,PlanRequest::Configure{adapter_id:id})?;
    let resources=coordinator.resources()?;
    if resources.len()!=1{return Err("Committed resource missing".into());}
    let resource=resources[0].id.clone();
    let (_,lease)=coordinator.verify_resource(&resource)?;
    if coordinator.plan(PlanRequest::Repair{resource_id:resource.clone()},now()).unwrap_err().code!="EBUSY"{return Err("Lease did not protect resource".into());}
    drop(lease);
    println!("nativeLifecycleLease=passed");
    let devices=windows_device::enumerate()?;
    let tap=devices.values().find(|d|d.instance_id==windows_driver::instance_id(&resource)).ok_or("Missing owned TAP")?;
    let topology=Topology::read()?;
    let bridge=topology.owned_bridge(&resource,tap,&physical)?.ok_or("Missing owned bridge")?;
    let bindings=topology.bindings(&bridge.guid)?.clone();
    println!("nativeLifecycleStage=disabled-binding-repair");
    windows_network_bridge::set_host_binding(&bridge,HostProtocol::Ipv6,true,false)?;
    confirm(&mut coordinator,PlanRequest::Repair{resource_id:resource.clone()})?;
    if Topology::read()?.bindings(&bridge.guid)?!=&bindings{return Err("Repair changed other bridge bindings".into());}
    println!("nativeLifecycleRepair=passed");
    println!("nativeLifecycleStage=remove-cancel");
    let plan=coordinator.plan(PlanRequest::Remove{resource_id:resource.clone()},now())?;
    let applied=coordinator.apply(&plan.id,now())?;
    if applied.state!=OperationState::Verifying || applied.error.is_some(){return Err(format!("Removal did not reach confirmation: {applied:?}").into());}
    if coordinator.cancel(&plan.id)?.state!=OperationState::Cancelled{return Err("Removal cancellation failed".into());}
    drop(coordinator.verify_resource(&resource)?);
    if Topology::read()?.bindings(&bridge.guid)?!=&bindings{return Err("Cancelled removal did not restore all bridge bindings".into());}
    println!("nativeLifecycleRemoveCancel=passed");
    println!("nativeLifecycleStage=remove-keep");
    confirm(&mut coordinator,PlanRequest::Remove{resource_id:resource})?;
    if !coordinator.resources()?.is_empty(){return Err("Removed resource remains registered".into());}
    let after=Snapshot::read(&physical)?;record(&root.join("physical-after.json"),&after)?;
    if !before.configuration_changes(&after).is_empty() || !before.runtime_changes(&after)?.is_empty(){return Err("Original Ethernet was not restored".into());}
    if windows_device::enumerate()?.values().any(|d|d.instance_id.starts_with(r"ROOT\EINKQEMUTAP\")){return Err("Fixture TAP remains".into());}
    println!("nativeLifecycleKeepRepairRemove=passed");Ok(())
}
