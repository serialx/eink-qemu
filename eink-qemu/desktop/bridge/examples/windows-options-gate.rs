//! Changes options only on the disposable VM's unbridged, app-owned TAP.
#[cfg(not(windows))]
fn main(){panic!("Requires the disposable Windows VM");}
#[cfg(windows)]
fn main(){if let Err(error)=run(){eprintln!("{error:?}");std::process::exit(1);}}
#[cfg(windows)]
fn run()->Result<(),Box<dyn std::error::Error>>{
    use eink_bridge::{model::ResourceId,windows_device::DeviceIdentity,windows_driver,windows_options,
        windows_profile::Snapshot,windows_security,windows_topology::Topology};
    use serde::{Serialize,Deserialize};use std::{fs,io::Write,path::Path};
    #[derive(Deserialize)]struct Fixture{resource:ResourceId,device:Option<DeviceIdentity>}
    #[derive(Serialize,Deserialize)]struct Undo{target:DeviceIdentity,before:Snapshot,desired:Snapshot}
    windows_security::require_system()?;
    if std::env::var("COMPUTERNAME").as_deref()!=Ok("WIN-SUNSKHPNVMO"){return Err("Wrong disposable host".into());}
    let root=Path::new(r"C:\eink-driver-native-gate");
    let file=fs::File::open(root.join("network-device.json"))?;windows_security::validate_handle(&file,None)?;
    let fixture:Fixture=serde_json::from_reader(file)?;
    let target=fixture.device.ok_or("Missing dedicated fixture")?;
    if target.instance_id!=windows_driver::instance_id(&fixture.resource){return Err("Wrong fixture ownership".into());}
    Topology::read()?.require_unbridged(&target)?;
    let path=root.join(format!("options-{}.json",fixture.resource.as_str()));
    let args=std::env::args().skip(1).collect::<Vec<_>>();
    if args==["--restore"] || args==["--verify"]{
        let file=fs::File::open(path)?;windows_security::validate_handle(&file,None)?;
        let undo:Undo=serde_json::from_reader(file)?;
        if undo.target!=target{return Err("Options fixture changed".into());}
        if args==["--verify"]{
            let current=Snapshot::read(&target)?;
            let changes=undo.desired.configuration_changes(&current);
            if !changes.is_empty(){return Err(format!("Options did not survive restart: {changes:?}").into());}
            println!("nativeOptionsSurviveRestart=passed");return Ok(());
        }
        windows_options::apply(&target,&undo.desired,&undo.before)?;
        println!("nativeOptionsRestored=passed");return Ok(());
    }
    let connected=args==["--connected"];
    if args!=["--run"] && !connected{return Err("Expected --run, --connected or --restore".into());}
    let before=Snapshot::read(&target)?;let mut desired=before.clone();
    for row in &mut desired.interfaces{row.nl_mtu=1400;row.automatic_metric=0;row.interface_metric=55;}
    if connected {
        for row in &mut desired.interfaces {row.dhcp=0;}
        for row in desired.configured_dhcp.as_mut().ok_or("Missing configured DHCP")? {row.enabled=0;}
        for row in desired.persistent_interfaces.as_mut().ok_or("Missing persistent options")? {
            row.nl_mtu=Some(1400);row.automatic_metric=Some(0);row.interface_metric=Some(55);
        }
    }
    desired.manual_dns[0].name_server="192.0.2.53,192.0.2.54".into();
    desired.manual_dns[1].name_server="2001:db8::53".into();
    desired.manual_dns[0].domain="bridge-test.invalid".into();
    desired.dns_client[0].register_this_connections_address=false;
    desired.dns_client[0].use_suffix_when_registering=false;
    let legacy=desired.legacy.as_mut().ok_or("Missing legacy settings")?;
    if legacy.tcpip_netbios_options.is_some() {
        legacy.tcpip_netbios_options=Some(2);
        legacy.manual_servers=Some(vec!["192.0.2.80".into()]);
    } else if connected {return Err("Connected fixture has no legacy options".into());}
    else { println!("nativeLegacyOptions=unavailable-on-disconnected-fixture"); }
    desired.normalize()?;
    let mut file=fs::OpenOptions::new().write(true).create_new(true).open(path)?;windows_security::validate_handle(&file,None)?;
    serde_json::to_writer(&mut file,&Undo{target:target.clone(),before:before.clone(),desired:desired.clone()})?;file.flush()?;file.sync_all()?;drop(file);
    let mut stale=before.clone();
    stale.interfaces[0].nl_mtu=1499;
    println!("nativeOptionsStage=reject-stale");
    let error=windows_options::apply(&target,&stale,&desired).unwrap_err();
    if error.code!="ECONFLICT"{return Err(format!("Stale settings produced {error:?}").into());}
    if !before.configuration_changes(&Snapshot::read(&target)?).is_empty(){return Err("Rejected change altered configuration".into());}
    println!("nativeOptionsStage=apply");windows_options::apply(&target,&before,&desired)?;
    println!("nativeOptionsStage=replay");windows_options::apply(&target,&before,&desired)?;
    if connected {
        let current=Snapshot::read(&target)?;
        if !desired.configuration_changes(&current).is_empty(){return Err("Connected options did not persist".into());}
        if windows_options::validate_transition(&desired,&before).unwrap_err().code!="EHOSTNETWORK"{return Err("Unsafe override erase was permitted".into());}
        println!("nativeConnectedOptionsApplyReplay=passed");return Ok(());
    }
    println!("nativeOptionsStage=restore");windows_options::apply(&target,&desired,&before)?;
    windows_options::apply(&target,&desired,&before)?;
    if !before.configuration_changes(&Snapshot::read(&target)?).is_empty(){return Err("Options were not restored".into());}
    println!("nativeOptionsApplyReplayRestore=passed");Ok(())
}
