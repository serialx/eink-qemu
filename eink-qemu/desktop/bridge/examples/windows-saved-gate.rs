//! Saved-configuration checks on an isolated bridge made from two owned TAPs.
#[cfg(not(windows))]
fn main(){panic!("Requires the disposable Windows VM");}
#[cfg(windows)]
fn main(){if let Err(error)=run(){eprintln!("{error:?}");std::process::exit(1);}}
#[cfg(windows)]
fn run()->Result<(),Box<dyn std::error::Error>>{
    use eink_bridge::{model::ResourceId,windows_device::DeviceIdentity,windows_driver,windows_network_bridge::{self as native,HostProtocol},windows_saved::Saved,windows_security,windows_topology::Topology};
    use serde::Deserialize;use std::{fs,io::Write,path::Path};
    #[derive(Deserialize)]struct Member{resource:ResourceId,device:Option<DeviceIdentity>}
    #[derive(Deserialize)]struct Record{members:Vec<Member>}
    windows_security::require_system()?;
    if std::env::var("COMPUTERNAME").as_deref()!=Ok("WIN-SUNSKHPNVMO"){return Err("Wrong disposable host".into());}
    let root=Path::new(r"C:\eink-driver-native-gate");
    let file=fs::File::open(root.join("network-pair.json"))?;windows_security::validate_handle(&file,None)?;
    let record:Record=serde_json::from_reader(file)?;
    if record.members.len()!=2{return Err("Missing isolated pair".into());}
    let devices=record.members.iter().map(|m|{
        let device=m.device.as_ref().ok_or("Missing device")?;
        if device.instance_id!=windows_driver::instance_id(&m.resource){return Err("Wrong owned device");}
        Ok(device.clone())
    }).collect::<Result<Vec<_>,&str>>()?;
    let topology=Topology::read()?;let physical=topology.device("{5238c509-ecda-49fb-bfdc-fd0be1922ff5}")?;
    let bridge=topology.owned_prepared_bridge(&record.members[0].resource,&devices[0],&record.members[1].resource,&devices[1],physical)?.ok_or("Missing owned bridge")?;
    topology.require_members(&bridge,&devices)?;
    let before=Saved::read(&bridge)?;
    let mut journal=fs::OpenOptions::new().write(true).create_new(true).open(root.join(format!("saved-{}.json",record.members[0].resource.as_str())))?;
    windows_security::validate_handle(&journal,None)?;serde_json::to_writer(&mut journal,&before)?;journal.flush()?;journal.sync_all()?;drop(journal);
    println!("nativeSavedStage=disable");
    native::set_host_binding(&bridge,HostProtocol::Ipv4,true,false)?;
    native::set_host_binding(&bridge,HostProtocol::Ipv6,true,false)?;
    println!("nativeSavedStage=verify-disabled");before.verify(&bridge)?;
    let mut changed=serde_json::to_value(&before)?;changed["netbios"]=serde_json::json!(2);
    let changed:Saved=serde_json::from_value(changed)?;
    if changed.verify(&bridge).unwrap_err().code!="ECONFLICT"{return Err("Changed saved plan was accepted".into());}
    println!("nativeSavedStage=restore");
    native::set_host_binding(&bridge,HostProtocol::Ipv4,false,true)?;
    native::set_host_binding(&bridge,HostProtocol::Ipv6,false,true)?;
    before.verify(&bridge)?;
    println!("nativeSavedDisabledBindingsConflictRestore=passed");Ok(())
}
