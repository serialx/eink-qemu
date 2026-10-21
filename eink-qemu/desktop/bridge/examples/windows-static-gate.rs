//! Native static population gate on a connected disposable, app-owned TAP.
#[cfg(not(windows))]
fn main(){panic!("Requires the disposable Windows VM");}
#[cfg(windows)]
fn main(){if let Err(error)=run(){eprintln!("{error:?}");std::process::exit(1);}}
#[cfg(windows)]
fn run()->Result<(),Box<dyn std::error::Error>>{
    use eink_bridge::{model::ResourceId,windows_device::DeviceIdentity,windows_driver,windows_profile::{Snapshot,Address,Route},windows_static,windows_security,windows_topology::Topology};
    use serde::Deserialize;use std::{fs,io::Write,path::Path};
    #[derive(Deserialize)]struct Fixture{resource:ResourceId,device:Option<DeviceIdentity>}
    windows_security::require_system()?;
    if std::env::var("COMPUTERNAME").as_deref()!=Ok("WIN-SUNSKHPNVMO"){return Err("Wrong disposable host".into());}
    let root=Path::new(r"C:\eink-driver-native-gate");
    let file=fs::File::open(root.join("network-device.json"))?;windows_security::validate_handle(&file,None)?;
    let fixture:Fixture=serde_json::from_reader(file)?;let target=fixture.device.ok_or("Missing owned TAP")?;
    if target.instance_id!=windows_driver::instance_id(&fixture.resource){return Err("Fixture ownership changed".into());}
    Topology::read()?.require_unbridged(&target)?;
    let before=Snapshot::read(&target)?;
    if !before.addresses.is_empty() || !before.routes.is_empty(){return Err("Fixture already has static configuration".into());}
    let mut desired=before.clone();for row in &mut desired.interfaces{row.dhcp=0;}
    for (ip,prefix,family) in [("192.0.2.10",24,2),("2001:db8:fffe::10",64,23)]{
        desired.addresses.push(Address{ip_address:ip.into(),prefix_length:prefix,address_family:family,prefix_origin:1,suffix_origin:1,skip_as_source:true,address_state:None,valid_lifetime_ticks:Some(i64::MAX as u64),preferred_lifetime_ticks:Some(i64::MAX as u64)});
    }
    for (prefix,next,metric) in [("198.51.100.0/24","192.0.2.1",42),("2001:db8:ffff::/64","2001:db8:fffe::1",43)]{
        desired.routes.push(Route{destination_prefix:prefix.into(),next_hop:next.into(),route_metric:metric,protocol:3,publish:0,valid_lifetime_ticks:Some(i64::MAX as u64),preferred_lifetime_ticks:Some(i64::MAX as u64)});
    }
    desired.normalize()?;windows_static::validate_source(&desired)?;
    let path=root.join(format!("static-{}.json",fixture.resource.as_str()));
    let mut file=fs::OpenOptions::new().write(true).create_new(true).open(path)?;windows_security::validate_handle(&file,None)?;
    serde_json::to_writer(&mut file,&serde_json::json!({"target":target,"before":before,"desired":desired}))?;file.flush()?;file.sync_all()?;drop(file);
    println!("nativeStaticStage=populate");
    let populated=windows_static::populate(&target,&before,&desired)?;
    println!("nativeStaticStage=replay");
    let replayed=windows_static::populate(&target,&before,&desired)?;
    if !populated.configuration_changes(&replayed).is_empty(){return Err("Replay changed configuration".into());}
    let mut changed=desired.clone();changed.routes[0].route_metric+=1;
    println!("nativeStaticStage=reject-changed");
    if windows_static::populate(&target,&replayed,&changed).unwrap_err().code!="ECONFLICT"{return Err("Changed static route was not refused".into());}
    let after=Snapshot::read(&target)?;
    if !replayed.configuration_changes(&after).is_empty(){return Err("Rejected route changed configuration".into());}
    for desired in &desired.addresses{
        if !after.active_addresses.iter().any(|row|row.ip_address==desired.ip_address && row.prefix_length==desired.prefix_length && row.skip_as_source==desired.skip_as_source && row.address_state==Some(4)){return Err("Static address is not usable".into());}
    }
    println!("nativeStaticPopulateReplayConflict=passed");Ok(())
}
