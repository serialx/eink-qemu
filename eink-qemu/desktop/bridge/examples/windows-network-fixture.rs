//! Disposable Windows native-test fixture. Never used by desktop IPC.
#[cfg(windows)]
fn main() {
    if let Err(error)=run() { eprintln!("{error:?}"); std::process::exit(1); }
}
#[cfg(not(windows))]
fn main() { panic!("This fixture requires the disposable Windows test VM"); }
#[cfg(windows)]
fn run()->Result<(),Box<dyn std::error::Error>> {
    use eink_bridge::{model::ResourceId,windows_device::{self,DeviceIdentity},windows_driver::{self,VerifiedPackage},windows_security,windows_topology::Topology};
    use serde::{Serialize,Deserialize};
    use std::{fs,path::Path,io::Write};
    #[derive(Serialize,Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Record { resource:ResourceId, device:Option<DeviceIdentity> }
    windows_security::require_system()?;
    if std::env::var("COMPUTERNAME").as_deref()!=Ok("WIN-SUNSKHPNVMO") {return Err("Wrong disposable Windows test host".into());}
    let root=Path::new(r"C:\eink-driver-native-gate");
    let path=root.join("network-device.json");
    let args=std::env::args().skip(1).collect::<Vec<_>>();
    if args==["--prepare"] {
        if path.exists() {return Err("A prior fixture needs recovery".into());}
        if !Topology::read()?.bridges().is_empty() {return Err("Restore the prior network probe first".into());}
        let package=VerifiedPackage::open(&root.join("driver"))?;
        let mut random=[0;16];getrandom::fill(&mut random)?;
        let resource=ResourceId::new(random.iter().map(|v|format!("{v:02x}")).collect::<String>())?;
        let mut record=Record{resource,device:None};
        let mut file=fs::OpenOptions::new().write(true).create_new(true).open(&path)?;
        windows_security::validate_handle(&file,None)?;
        serde_json::to_writer(&mut file,&record)?;file.flush()?;file.sync_all()?;drop(file);
        let installed=windows_driver::create(&package,&record.resource)?;
        if installed.reboot_required {return Err("Fixture driver requires restart; recover it before another test".into());}
        record.device=Some(installed.identity.ok_or("Missing installed TAP identity")?);
        let mut temp=tempfile::NamedTempFile::new_in(root)?;
        serde_json::to_writer(&mut temp,&record)?;temp.flush()?;temp.as_file().sync_all()?;temp.persist(&path)?;
        println!("{}",serde_json::to_string(&record)?);
    } else if args==["--preserve-mac"] {
        let file=fs::File::open(&path)?;windows_security::validate_handle(&file,None)?;
        let record:Record=serde_json::from_reader(file)?;
        let tap=record.device.as_ref().ok_or("Missing owned TAP identity")?;
        let topology=Topology::read()?;
        let physical=topology.device("{5238C509-ECDA-49FB-BFDC-FD0BE1922FF5}")?;
        let bridge=topology.owned_bridge(&record.resource,tap,physical)?.ok_or("Missing owned bridge")?;
        use eink_bridge::windows_network_bridge::{self as native,BridgeAddress};
        let before=native::address(&bridge)?;
        let mut journal=fs::OpenOptions::new().write(true).create_new(true).open(root.join(format!("network-bridge-address-{}.json",record.resource.as_str())))?;
        windows_security::validate_handle(&journal,None)?;
        serde_json::to_writer(&mut journal,&serde_json::json!({"bridge":bridge,"before":before,"desired":"4AE293973B07"}))?;
        journal.flush()?;journal.sync_all()?;drop(journal);
        println!("nativeBridgeAddressStage=reject-stale");
        let stale=BridgeAddress{configured:Some("001122334455".into()),..before.clone()};
        match native::preserve_address(&bridge,&stale,"4AE293973B07") {
            Err(error) if error.code=="ECONFLICT"=>(),
            other=>return Err(format!("Expected a stale-address conflict, got {other:?}").into()),
        }
        if native::address(&bridge)?!=before {return Err("Rejected change modified the bridge".into());}
        println!("nativeBridgeAddressStage=preserve");
        let after=native::preserve_address(&bridge,&before,"4AE293973B07")?;
        println!("nativeBridgeAddressStage=replay");
        if native::preserve_address(&bridge,&before,"4AE293973B07")?!=after {return Err("Bridge address replay was not idempotent".into());}
        println!("nativeBridgeAddressStage=reject-changed");
        match native::preserve_address(&bridge,&before,"001122334455") {
            Err(error) if error.code=="ECONFLICT"=>(),
            other=>return Err(format!("Expected an external-address conflict, got {other:?}").into()),
        }
        if native::address(&bridge)?!=after {return Err("Rejected change modified the bridge".into());}
        println!("nativeBridgeAddressPreserved=passed");
    } else if args==["--remove"] {
        if !path.exists() {println!("nativeNetworkFixtureRemoved=alreadyAbsent");return Ok(());}
        let file=fs::File::open(&path)?;windows_security::validate_handle(&file,None)?;
        let record:Record=serde_json::from_reader(file)?;
        let devices=windows_device::enumerate()?;
        if let Some(device)=devices.values().find(|d|d.instance_id==windows_driver::instance_id(&record.resource)) {
            if Topology::read()?.bindings(&device.guid)?.get("ms_implat")!=Some(&false) {return Err("Remove the fixture bridge before deleting its TAP".into());}
        }
        if windows_driver::remove(&record.resource,record.device.as_ref())? {return Err("Restart required to finish removing the fixture TAP".into());}
        fs::remove_file(&path)?;println!("nativeNetworkFixtureRemoved=passed");
    } else {return Err("Expected --prepare, --preserve-mac, or --remove".into());}
    Ok(())
}
