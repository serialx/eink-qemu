//! Full native cancellation gate. Only the named disposable x64 VM is allowed;
//! its outer runner arms startup/timed recovery before invoking this example.
#[cfg(not(windows))]
fn main(){panic!("Requires the disposable Windows VM");}
#[cfg(windows)]
fn main(){if let Err(error)=run(){eprintln!("{error:?}");std::process::exit(1);}}
#[cfg(windows)]
fn run()->Result<(),Box<dyn std::error::Error>>{
    use eink_bridge::{coordinator::Coordinator,model::*,windows_device,windows_platform::WindowsPlatform,windows_profile::Snapshot,windows_security};
    use std::{fs,io::Write,path::Path,time::{SystemTime,UNIX_EPOCH}};
    fn now()->u64{SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()}
    fn record(path:&Path,value:&impl serde::Serialize)->Result<(),Box<dyn std::error::Error>>{
        let mut file=fs::OpenOptions::new().write(true).create_new(true).open(path)?;windows_security::validate_handle(&file,None)?;
        serde_json::to_writer(&mut file,value)?;file.flush()?;file.sync_all()?;Ok(())
    }
    windows_security::require_system()?;
    if std::env::var("COMPUTERNAME").as_deref()!=Ok("WIN-SUNSKHPNVMO"){return Err("Wrong disposable host".into());}
    let root=Path::new(r"C:\eink-driver-native-gate\coordinator-native-v2");
    let package=Path::new(r"C:\eink-driver-native-gate\driver");
    let args=std::env::args().skip(1).collect::<Vec<_>>();
    if args==["--verify-original"]{
        let file=fs::File::open(root.join("physical-before.json"))?;windows_security::validate_handle(&file,None)?;
        let mut before:Snapshot=serde_json::from_reader(file)?;before.normalize()?;before.require_restorable()?;
        let device=windows_device::enumerate()?.get(&before.guid).cloned().ok_or("Missing original adapter")?;
        if !device.instance_id.eq_ignore_ascii_case(&before.instance_id){return Err("Original adapter was replaced".into());}
        let after=Snapshot::read(&device)?;
        if !before.configuration_changes(&after).is_empty() || !before.runtime_changes(&after)?.is_empty(){return Err("Original network is not restored".into());}
        record(&root.join("physical-after-recovery.json"),&after)?;
        println!("nativeCoordinatorOriginalNetwork=passed");return Ok(());
    }
    if args==["--recover"]{
        let mut coordinator=Coordinator::open(root,"sid:S-1-5-18",WindowsPlatform::for_recovery(package)?)?;
        let operation=coordinator.recover_startup()?;
        println!("recovery={}",serde_json::to_string(&operation)?);
        if operation.as_ref().is_some_and(|op|op.error.as_ref().is_some_and(|e|e.code=="EROLLBACK")){
            return Err("Native recovery needs inspection".into());
        }
        return Ok(());
    }
    if args!=["--configure-cancel"]{return Err("Expected --configure-cancel or --recover".into());}
    for name in ["network-device.json","network-pair.json"]{if root.parent().unwrap().join(name).exists(){return Err("Another fixture is active".into());}}
    if windows_device::enumerate()?.values().any(|d|d.instance_id.starts_with(r"ROOT\EINKQEMUTAP\")){return Err("A dedicated TAP remains from another test".into());}
    let armed=fs::File::open(root.join("recovery-armed"))?;windows_security::validate_handle(&armed,None)?;
    let mut coordinator=Coordinator::open(root,"sid:S-1-5-18",WindowsPlatform::for_user("sid:S-1-5-18",package)?)?;
    let id=AdapterId::new("{5238c509-ecda-49fb-bfdc-fd0be1922ff5}")?;
    let device=windows_device::enumerate()?.get(id.as_str()).cloned().ok_or("Missing test Ethernet")?;
    let before=Snapshot::read(&device)?;record(&root.join("physical-before.json"),&before)?;
    println!("nativeCoordinatorStage=plan");
    let plan=coordinator.plan(PlanRequest::Configure{adapter_id:id},now())?;record(&root.join("reviewed-plan.json"),&plan)?;
    println!("nativeCoordinatorStage=apply");
    let applied=coordinator.apply(&plan.id,now());
    // Cancel even on failure, using the same journal, before reporting it.
    println!("nativeCoordinatorStage=cancel");
    let cancelled=coordinator.cancel(&plan.id)?;
    println!("cancelled={}",serde_json::to_string(&cancelled)?);
    if cancelled.state!=OperationState::Cancelled{return Err("Native cancellation did not complete".into());}
    let after=Snapshot::read(&device)?;record(&root.join("physical-after.json"),&after)?;
    if !before.configuration_changes(&after).is_empty() || !before.runtime_changes(&after)?.is_empty(){return Err("Physical network was not restored".into());}
    if windows_device::enumerate()?.values().any(|d|d.instance_id.starts_with(r"ROOT\EINKQEMUTAP\")){return Err("Owned TAP cleanup is incomplete".into());}
    let applied=applied?;
    println!("applied={}",serde_json::to_string(&applied)?);
    if applied.state!=OperationState::Verifying || applied.confirmation_deadline.is_none() || applied.error.is_some(){return Err("Native setup did not reach confirmation".into());}
    println!("nativeCoordinatorConfigureCancel=passed");Ok(())
}
