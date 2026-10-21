//! Disposable native gate for creating a bridge before touching Ethernet.
#[cfg(not(windows))]
fn main() { panic!("Requires the disposable Windows test VM"); }
#[cfg(windows)]
fn main() { if let Err(error)=run() { eprintln!("{error:?}"); std::process::exit(1); } }
#[cfg(windows)]
fn run()->Result<(),Box<dyn std::error::Error>> {
    use eink_bridge::{model::ResourceId,windows_device::{self,DeviceIdentity},windows_driver::{self,VerifiedPackage},
        windows_security,windows_topology::Topology,windows_network_bridge as native,windows_membership};
    use serde::{Serialize,Deserialize};
    use std::{fs,path::Path,io::Write};
    #[derive(Serialize,Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Member { resource:ResourceId, device:Option<DeviceIdentity> }
    #[derive(Serialize,Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Record { members:Vec<Member> }
    fn save(path:&Path,record:&Record)->Result<(),Box<dyn std::error::Error>> {
        let mut temp=tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
        serde_json::to_writer(&mut temp,record)?;temp.flush()?;temp.as_file().sync_all()?;temp.persist(path)?;Ok(())
    }
    windows_security::require_system()?;
    if std::env::var("COMPUTERNAME").as_deref()!=Ok("WIN-SUNSKHPNVMO") {return Err("Wrong disposable test host".into());}
    let root=Path::new(r"C:\eink-driver-native-gate");let path=root.join("network-pair.json");
    let args=std::env::args().skip(1).collect::<Vec<_>>();
    if args==["--prepare"] {
        if path.exists() || root.join("network-device.json").exists() {return Err("Recover the previous network fixture first".into());}
        if !Topology::read()?.bridges().is_empty() {return Err("An existing bridge needs recovery first".into());}
        let package=VerifiedPackage::open(&root.join("driver"))?;
        let mut record=Record{members:Vec::new()};
        for _ in 0..2 {
            let mut random=[0;16];getrandom::fill(&mut random)?;
            record.members.push(Member{resource:ResourceId::new(random.iter().map(|v|format!("{v:02x}")).collect::<String>())?,device:None});
        }
        let file=fs::OpenOptions::new().write(true).create_new(true).open(&path)?;
        windows_security::validate_handle(&file,None)?;drop(file);save(&path,&record)?;
        for index in 0..2 {
            let installed=windows_driver::create(&package,&record.members[index].resource)?;
            if installed.reboot_required {return Err("Fixture needs restart before continuing".into());}
            record.members[index].device=Some(installed.identity.ok_or("Missing TAP identity")?);save(&path,&record)?;
        }
        println!("{}",serde_json::to_string(&record)?);return Ok(());
    }
    if args==["--remove"] && !path.exists() {println!("nativePairFixtureRemoved=alreadyAbsent");return Ok(());}
    let file=fs::File::open(&path)?;windows_security::validate_handle(&file,None)?;
    let record:Record=serde_json::from_reader(file)?;
    if record.members.len()!=2 || record.members[0].resource==record.members[1].resource {return Err("Invalid pair fixture".into());}
    let topology=Topology::read()?;
    if args==["--remove"] {
        let devices=windows_device::enumerate()?;
        for member in &record.members {
            if let Some(device)=devices.values().find(|d|d.instance_id==windows_driver::instance_id(&member.resource)) {
                if topology.bindings(&device.guid)?.get("ms_implat")!=Some(&false) {return Err("Detach the fixture members before removal".into());}
            }
        }
        for member in &record.members {
            if windows_driver::remove(&member.resource,member.device.as_ref())? {return Err("Restart required for fixture cleanup".into());}
        }
        fs::remove_file(&path)?;println!("nativePairFixtureRemoved=passed");
    } else {
        let members=record.members.iter().map(|member| {
            let device=member.device.clone().ok_or("Missing fixture device")?;
            if device.instance_id!=windows_driver::instance_id(&member.resource) {return Err("Fixture instance changed");}
            Ok(device)
        }).collect::<Result<Vec<_>,&str>>()?;
        let devices=windows_device::enumerate()?;
        let physical=devices.get(&windows_device::guid("{5238C509-ECDA-49FB-BFDC-FD0BE1922FF5}")?).ok_or("Missing test Ethernet")?;
        if args==["--create-bridge"] {
            let bridge=windows_membership::create_isolated(&record.members[0].resource,&members[0],
                &record.members[1].resource,&members[1],physical)?;
            let path=root.join(format!("pair-bridge-{}.json",record.members[0].resource.as_str()));
            if path.exists() {
                let recorded:DeviceIdentity=serde_json::from_reader(fs::File::open(&path)?)?;
                if recorded!=bridge {return Err("Fixture bridge identity changed".into());}
            } else {
                let mut file=fs::OpenOptions::new().write(true).create_new(true).open(path)?;
                windows_security::validate_handle(&file,None)?;
                serde_json::to_writer(&mut file,&bridge)?;file.flush()?;file.sync_all()?;
            }
            println!("nativeIsolatedBridgeCreated=passed");return Ok(());
        }
        if args==["--destroy-bridge"] {
            let path=root.join(format!("pair-bridge-{}.json",record.members[0].resource.as_str()));
            let file=fs::File::open(path)?;windows_security::validate_handle(&file,None)?;
            let recorded:DeviceIdentity=serde_json::from_reader(file)?;
            let options=[vec![members[0].clone()],members.clone(),vec![members[0].clone(),physical.clone()],
                vec![members[0].clone(),members[1].clone(),physical.clone()]];
            let current=if topology.bridges().is_empty() {options[3].clone()} else {
                options.into_iter().find(|expected|topology.require_transition_members(&recorded,expected).is_ok())
                    .ok_or("Unexpected fixture membership")?
            };
            windows_membership::destroy(&recorded,&current)?;
            windows_membership::destroy(&recorded,&current)?;
            println!("nativeBridgeDestroyAndReplay=passed");return Ok(());
        }
        let bridges=topology.bridges();
        if bridges.len()!=1 {return Err("Missing unique fixture bridge".into());}
        let bridge=bridges[0];
        if args!=["--prepare-address"] && args!=["--capture-address"] && args!=["--preserve-address"] {
            let path=root.join(format!("pair-bridge-{}.json",record.members[0].resource.as_str()));
            let file=fs::File::open(path)?;windows_security::validate_handle(&file,None)?;
            let recorded:DeviceIdentity=serde_json::from_reader(file)?;
            if &recorded!=bridge {return Err("Fixture bridge identity changed".into());}
            let tap=&members[0];let bootstrap=&members[1];
            if args==["--host-bindings"] {
                topology.require_members(bridge,&members)?;
                let before=topology.bindings(&bridge.guid)?.clone();
                let mut journal=fs::OpenOptions::new().write(true).create_new(true).open(root.join(format!("pair-bindings-{}.json",record.members[0].resource.as_str())))?;
                windows_security::validate_handle(&journal,None)?;
                serde_json::to_writer(&mut journal,&serde_json::json!({"bridge":bridge,"before":before}))?;
                journal.flush()?;journal.sync_all()?;drop(journal);
                for (component,protocol) in [("ms_tcpip",native::HostProtocol::Ipv4),("ms_tcpip6",native::HostProtocol::Ipv6)] {
                    if before.get(component)!=Some(&true) {return Err("Unexpected fresh bridge host binding".into());}
                    println!("nativeHostBindingStage=disable-{component}");
                    native::set_host_binding(bridge,protocol,true,false)?;
                    native::set_host_binding(bridge,protocol,true,false)?;
                    println!("nativeHostBindingStage=restore-{component}");
                    native::set_host_binding(bridge,protocol,false,true)?;
                    native::set_host_binding(bridge,protocol,false,true)?;
                    if Topology::read()?.bindings(&bridge.guid)?!=&before {return Err("Host binding replay changed another protocol".into());}
                }
                println!("nativeHostBindingsAndReplay=passed");return Ok(());
            }
            let (before,member,present)=match args.as_slice() {
                [command] if command=="--attach-bootstrap" => (vec![tap.clone()],bootstrap,true),
                [command] if command=="--attach-physical" => (members.clone(),physical,true),
                [command] if command=="--detach-bootstrap" => (vec![tap.clone(),bootstrap.clone(),physical.clone()],bootstrap,false),
                [command] if command=="--detach-physical" => (vec![tap.clone(),physical.clone()],physical,false),
                [command] if command=="--reattach-physical" => (vec![tap.clone()],physical,true),
                _ => return Err("Unknown fixture command".into()),
            };
            windows_membership::set_member(bridge,&before,member,present)?;
            windows_membership::set_member(bridge,&before,member,present)?;
            println!("nativeMembershipChangeAndReplay=passed");return Ok(());
        }
        if args==["--preserve-address"] {
            topology.require_transition_members(bridge,&[members[0].clone(),members[1].clone(),physical.clone()])?;
            let file=fs::File::open(root.join(format!("pair-address-{}.json",record.members[0].resource.as_str())))?;
            windows_security::validate_handle(&file,None)?;
            let saved:serde_json::Value=serde_json::from_reader(file)?;
            let saved_bridge:DeviceIdentity=serde_json::from_value(saved["bridge"].clone())?;
            let before:native::BridgeAddress=serde_json::from_value(saved["before"].clone())?;
            if &saved_bridge!=bridge {return Err("Saved bridge address identity changed".into());}
            native::preserve_address(bridge,&before,"4AE293973B07")?;
            native::preserve_address(bridge,&before,"4AE293973B07")?;
            println!("nativeAttachedBridgeAddress=passed");return Ok(());
        }
        topology.require_members(bridge,&members)?;
        let before=native::address(bridge)?;
        let mut journal=fs::OpenOptions::new().write(true).create_new(true).open(root.join(format!("pair-address-{}.json",record.members[0].resource.as_str())))?;
        windows_security::validate_handle(&journal,None)?;
        serde_json::to_writer(&mut journal,&serde_json::json!({"bridge":bridge,"before":before,"targets":["001122334455","4AE293973B07"]}))?;
        journal.flush()?;journal.sync_all()?;drop(journal);
        if args==["--capture-address"] {println!("nativeBridgeAddressCaptured=passed");return Ok(());}
        let global=native::preserve_address(bridge,&before,"001122334455")?;
        native::preserve_address(bridge,&global,"4AE293973B07")?;
        println!("nativeIsolatedBridgeAddress=passed");
    }
    Ok(())
}
