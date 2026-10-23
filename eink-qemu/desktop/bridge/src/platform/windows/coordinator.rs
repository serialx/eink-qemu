//! Windows lifecycle under the protected shared transaction journal. Device
//! creation intent is durable before PnP work; a bridge is owned only through
//! its dedicated TAP membership, never its display name.
use crate::{coordinator::{PlatformOps,Prepared,Resource},model::*,windows_boot::{self,BootIdentity},
    windows_device::{self,DeviceIdentity},windows_driver::{self,VerifiedPackage},windows_membership as membership,
    windows_network_bridge::{self as native,BridgeAddress,HostProtocol},windows_options as options,
    windows_profile::Snapshot as Profile,windows_saved::Saved,windows_routes,windows_security,windows_static,
    windows_topology::Topology,windows_verify};
use serde::{Serialize,Deserialize};
use std::{collections::BTreeMap,path::{Path,PathBuf},time::{Duration,Instant}};
fn conflict()->BridgeError{BridgeError::new("ECONFLICT","The Windows bridge or connection settings changed. Review the current network before continuing.")}
fn missing()->BridgeError{BridgeError::new("ENEEDSREPAIR","The recorded bridge is incomplete. Restore or remove its setup before configuring it again.")}
fn removal_bindings(before:&BTreeMap<String,bool>)->BTreeMap<String,bool>{
    let mut expected=before.clone();
    // With both IP transports disabled, Windows also disables its SMB client
    // and server bindings. Require that exact native state; unrelated bindings
    // still have to match the recorded configuration before destruction.
    for component in ["ms_tcpip","ms_tcpip6","ms_msclient","ms_server"]{
        if let Some(value)=expected.get_mut(component){*value=false;}
    }
    expected
}
#[derive(Clone,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
struct Bridge {device:DeviceIdentity,address:BridgeAddress,baseline:Profile,saved:Saved,bindings:BTreeMap<String,bool>}
#[derive(Clone,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
struct Prior {profile:Option<Profile>,saved:Saved,physical_attached:bool,ipv4:bool,ipv6:bool}
#[derive(Clone,Serialize,Deserialize)]
enum Action {Configure,Repair(Prior),Remove(Prior)}
#[derive(Clone,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    resource:ResourceId,bootstrap_resource:ResourceId,physical:DeviceIdentity,fingerprint:String,
    original:Profile,original_saved:Saved,installed_saved:Option<Saved>,physical_bindings:BTreeMap<String,bool>,tap:Option<DeviceIdentity>,bootstrap:Option<DeviceIdentity>,
    bridge:Option<Bridge>,migration_before:Option<Profile>,options_before:Option<Profile>,
    reboot:Option<BootIdentity>,action:Action,committed:bool,
}
impl Snapshot {
    fn tap(&self)->Result<&DeviceIdentity,BridgeError>{self.tap.as_ref().ok_or_else(missing)}
    fn bridge(&self)->Result<&Bridge,BridgeError>{self.bridge.as_ref().ok_or_else(missing)}
    fn removing(&self)->bool{matches!(self.action,Action::Remove(_))}
    fn prior(&self)->Option<&Prior>{match &self.action{Action::Repair(v)|Action::Remove(v)=>Some(v),_=>None}}
}
#[derive(Clone,Serialize,Deserialize)]
pub enum Mutation {InstallTap,InstallBootstrap,CreateBridge,AttachBootstrap,CaptureBridge,AttachPhysical,PreserveAddress,
    CaptureMigration,StaticConfiguration,CaptureOptions,Options,DetachBootstrap,RemoveBootstrap,DisableIpv4,DisableIpv6,DetachPhysical,
    RepairAttach,RepairIpv4,RepairIpv6}
pub struct WindowsPlatform {owner:Option<String>,package:PathBuf}
impl WindowsPlatform {
    /// Only the installed service supplies this protected package path and
    /// authenticated principal. Neither is a request field.
    pub fn for_user(owner:&str,package:&Path)->Result<Self,BridgeError>{
        windows_security::require_system()?;if owner.is_empty(){return Err(conflict());}
        Ok(Self{owner:Some(owner.into()),package:package.into()})
    }
    pub fn for_recovery(package:&Path)->Result<Self,BridgeError>{windows_security::require_system()?;Ok(Self{owner:None,package:package.into()})}
    fn physical(snapshot:&Snapshot)->Result<(),BridgeError>{
        let adapter=crate::inspect()?.adapters.into_iter().find(|a|a.id.as_str()==snapshot.physical.guid).ok_or_else(missing)?;
        if !adapter.eligible || adapter.fingerprint!=snapshot.fingerprint || windows_device::enumerate()?.get(&snapshot.physical.guid)!=Some(&snapshot.physical){return Err(conflict());}
        Ok(())
    }
    fn original_network(snapshot:&Snapshot,runtime:bool)->Result<(),BridgeError>{
        Self::physical(snapshot)?;
        snapshot.original_saved.verify(&snapshot.physical)?;
        let current=Profile::read(&snapshot.physical)?;
        if !snapshot.original.configuration_changes(&current).is_empty(){return Err(conflict());}
        if runtime && !snapshot.original.runtime_changes(&current)?.is_empty(){return Err(BridgeError::new("ESTALE","The active connection changed. Refresh bridge setup before continuing."));}
        windows_static::validate_source(&current)?;windows_routes::validate_source(&snapshot.physical,&current)
    }
    fn devices(snapshot:&Snapshot)->Result<(DeviceIdentity,Option<DeviceIdentity>),BridgeError>{
        let devices=windows_device::enumerate()?;
        let tap=devices.values().find(|d|d.instance_id==windows_driver::instance_id(&snapshot.resource)).cloned().ok_or_else(missing)?;
        if !tap.is_tap() || snapshot.tap.as_ref().is_some_and(|expected|expected!=&tap){return Err(conflict());}
        let bootstrap=devices.values().find(|d|d.instance_id==windows_driver::instance_id(&snapshot.bootstrap_resource)).cloned();
        if bootstrap.as_ref().is_some_and(|d|!d.is_tap() || snapshot.bootstrap.as_ref().is_some_and(|expected|expected!=d)){return Err(conflict());}
        Ok((tap,bootstrap))
    }
    fn owned(snapshot:&Snapshot,topology:&Topology)->Result<Option<DeviceIdentity>,BridgeError>{
        let (tap,bootstrap)=Self::devices(snapshot)?;
        let bridge=if let Some(bootstrap)=bootstrap {
            topology.owned_prepared_bridge(&snapshot.resource,&tap,&snapshot.bootstrap_resource,&bootstrap,&snapshot.physical)?
        }else{topology.owned_bridge(&snapshot.resource,&tap,&snapshot.physical)?};
        if bridge.as_ref().is_some_and(|b|snapshot.bridge.as_ref().is_some_and(|saved|saved.device!=*b)){return Err(conflict());}
        Ok(bridge)
    }
    fn members(snapshot:&Snapshot,topology:&Topology)->Result<Vec<DeviceIdentity>,BridgeError>{
        let bridge=Self::owned(snapshot,topology)?.ok_or_else(missing)?;
        let (tap,bootstrap)=Self::devices(snapshot)?;
        let mut members=vec![tap];
        for device in bootstrap.into_iter().chain(Some(snapshot.physical.clone())){
            if topology.bindings(&device.guid)?.get("ms_implat")==Some(&true){members.push(device);}
        }
        topology.require_transition_members(&bridge,&members)?;Ok(members)
    }
    fn await_network(snapshot:&Snapshot,device:&DeviceIdentity,expected:&Profile,immediate:bool)->Result<(),BridgeError>{
        let deadline=Instant::now()+Duration::from_secs(120);
        loop {
            Self::physical(snapshot)?;
            let current=Profile::read(device)?;
            let ready=network_ready(expected,&current,immediate)?;
            if ready{return Ok(());}
            if Instant::now()>=deadline{return Err(BridgeError::new("EHOSTNETWORK","Windows has not restored usable host addresses, routes, and DNS."));}
            std::thread::sleep(Duration::from_millis(500));
        }
    }
    fn binding(snapshot:&Snapshot,protocol:HostProtocol,before:bool,enabled:bool)->Result<(),BridgeError>{
        let topology=Topology::read()?;Self::members(snapshot,&topology)?;
        native::set_host_binding(&snapshot.bridge()?.device,protocol,before,enabled)
    }
    fn installed_address(snapshot:&Snapshot)->Result<(),BridgeError>{
        let address=native::address(&snapshot.bridge()?.device)?;
        let expected=snapshot.original.mac.replace('-',"");
        if address.current!=expected || address.configured.as_deref()!=Some(&expected){return Err(conflict());}
        Ok(())
    }
    fn remove_devices(snapshot:&Snapshot)->Result<(),BridgeError>{
        // A missing endpoint does not erase the exact PnP installation intent.
        // remove also handles a crash between device registration and GUID allocation.
        if windows_driver::remove(&snapshot.bootstrap_resource,snapshot.bootstrap.as_ref())?
            | windows_driver::remove(&snapshot.resource,snapshot.tap.as_ref())? {
            return Err(BridgeError::new("EREBOOT","Windows requires a restart to finish removing the bridge adapter."));
        }
        Ok(())
    }
    fn destroy_owned(snapshot:&Snapshot)->Result<(),BridgeError>{
        let topology=Topology::read()?;
        if topology.bridges().is_empty(){topology.require_unbridged(&snapshot.physical)?;return Ok(());}
        let bridge=Self::owned(snapshot,&topology)?.ok_or_else(conflict)?;
        let members=Self::members(snapshot,&topology)?;
        if let Some(saved)=&snapshot.bridge {
            let expected=if snapshot.removing(){removal_bindings(&saved.bindings)}else{saved.bindings.clone()};
            if topology.bindings(&bridge.guid)?!=&expected{return Err(conflict());}
            if snapshot.removing(){
                snapshot.prior().ok_or_else(missing)?.saved.verify(&bridge)?;
                Self::installed_address(snapshot)?;
            }
            if matches!(snapshot.action,Action::Configure){
                let current_saved=Saved::read(&bridge)?;
                current_saved.validate_partial(&saved.saved,&snapshot.original_saved)
                    .or_else(|_|current_saved.validate_partial(&saved.saved,&snapshot.original_saved.during_static()))?;
                let current=Profile::read(&bridge)?;
                let before=snapshot.options_before.as_ref().or(snapshot.migration_before.as_ref()).unwrap_or(&saved.baseline);
                let mut during_static=snapshot.original.clone();
                for address in &snapshot.original.addresses{
                    during_static.interfaces.iter_mut().find(|i|i.address_family==address.address_family).ok_or_else(conflict)?.dhcp=0;
                    during_static.configured_dhcp.as_mut().ok_or_else(conflict)?.iter_mut().find(|i|i.address_family==address.address_family).ok_or_else(conflict)?.enabled=0;
                }
                options::validate_current(before,&snapshot.original,&current)
                    .or_else(|_|options::validate_current(before,&during_static,&current))?;
                windows_routes::validate_source(&bridge,&current)?;
                let address=native::address(&bridge)?;
                let wanted=snapshot.original.mac.replace('-',"");
                if address!=saved.address && (address.current!=wanted || address.configured.as_deref()!=Some(&wanted)){return Err(conflict());}
            }
        }
        membership::destroy(&bridge,&members)
    }
}
fn network_ready(expected:&Profile,current:&Profile,immediate:bool)->Result<bool,BridgeError>{
    let changes=expected.configuration_changes(current);
    if !changes.is_empty(){
        // Router advertisements can restore an active IPv6 MTU after the
        // saved settings are already back. Wait for that exact value, without
        // accepting it early or masking a persistent/user configuration edit.
        if let Some(ipv6)=expected.interfaces.iter().find(|row|row.address_family==23) {
            let router_mtu=ipv6.router_discovery==1 && expected.persistent_interfaces.as_ref()
                .and_then(|rows|rows.iter().find(|row|row.address_family==23))
                .is_some_and(|row|row.nl_mtu.is_none());
            if router_mtu {
                let mut settled=current.clone();
                if let Some(row)=settled.interfaces.iter_mut().find(|row|row.address_family==23){row.nl_mtu=ipv6.nl_mtu;}
                if expected.configuration_changes(&settled).is_empty(){return Ok(false);}
            }
        }
        let mut error=conflict();error.detail=Some(format!("Changed settings: {}.",changes.join(", ")));
        return Err(error);
    }
    Ok(if immediate{expected.runtime_changes(current)?.is_empty()}else{usable(expected,current)})
}
/// Long-lived resources may renew DHCP leases or IPv6 privacy addresses.
/// Preserve fixed settings; require the original address families and default
/// routing/DNS capabilities without freezing a months-old lease value.
fn usable(expected:&Profile,current:&Profile)->bool{
    use std::net::IpAddr;
    fn scope(address:&crate::windows_profile::Address)->Option<bool>{
        if address.address_state!=Some(4){return None;}
        match address.ip_address.parse::<IpAddr>().ok()?{
            IpAddr::V4(ip) if !ip.is_unspecified() && !ip.is_loopback() && !ip.is_multicast()=>Some(!ip.is_link_local()),
            IpAddr::V6(ip) if !ip.is_unspecified() && !ip.is_loopback() && !ip.is_multicast()=>Some(!ip.is_unicast_link_local()),
            _=>None,
        }
    }
    if expected.mac!=current.mac{return false;}
    for family in [2,23]{
        let required=expected.active_addresses.iter().filter(|a|a.address_family==family).filter_map(scope).max();
        if let Some(global)=required {
            if !current.active_addresses.iter().filter(|a|a.address_family==family).filter_map(scope).any(|present|present || !global){return false;}
        }
        let default=if family==2{"0.0.0.0/0"}else{"::/0"};
        if expected.active_routes.iter().any(|r|r.destination_prefix==default) && !current.active_routes.iter().any(|r|r.destination_prefix==default){return false;}
        if expected.dns.iter().any(|d|d.address_family==family && !d.server_addresses.is_empty())
            && !current.dns.iter().any(|d|d.address_family==family && !d.server_addresses.is_empty()){return false;}
        let protocol=if family==2{"Tcpip"}else{"Tcpip6"};
        if let Some(manual)=expected.manual_dns.iter().find(|d|d.protocol==protocol && !d.name_server.is_empty()){
            let servers=manual.name_server.split(',').collect::<Vec<_>>();
            if current.dns.iter().find(|d|d.address_family==family).is_none_or(|d|d.server_addresses.iter().map(String::as_str).collect::<Vec<_>>()!=servers){return false;}
        }
    }
    if expected.routes.iter().any(|saved|!current.active_routes.iter().any(|r|r.destination_prefix==saved.destination_prefix && r.next_hop==saved.next_hop
        && r.route_metric==saved.route_metric && r.protocol==saved.protocol && r.publish==saved.publish)){return false;}
    expected.addresses.iter().all(|saved|current.active_addresses.iter().any(|a|a.ip_address==saved.ip_address && a.prefix_length==saved.prefix_length
        && a.skip_as_source==saved.skip_as_source && a.address_state==Some(4)))
}

impl PlatformOps for WindowsPlatform {
    type Snapshot=Snapshot;type Mutation=Mutation;
    fn prepare(&mut self,request:PlanRequest,id:OperationId,now:u64,existing:Option<&Resource<Snapshot>>)->Result<Prepared<Snapshot,Mutation>,BridgeError>{
        let owner=self.owner.as_ref().ok_or_else(conflict)?;VerifiedPackage::open(&self.package)?;
        let topology=Topology::read()?;
        let (snapshot,mutations)=match &request{
            PlanRequest::Configure{adapter_id}=>{
                let adapter=crate::inspect()?.adapters.into_iter().find(|a|a.id==*adapter_id && a.eligible && a.link_up).ok_or_else(missing)?;
                let physical=topology.device(adapter_id.as_str())?.clone();topology.require_unbridged(&physical)?;
                let original=Profile::read(&physical)?;let original_saved=Saved::read(&physical)?;windows_static::validate_source(&original)?;windows_routes::validate_source(&physical,&original)?;
                let bindings=topology.bindings(&physical.guid)?.clone();
                if bindings.get("ms_tcpip")!=Some(&true) || bindings.get("ms_tcpip6")!=Some(&true){return Err(BridgeError::new("EUNSUPPORTED","Automatic setup requires both IPv4 and IPv6 bindings on Ethernet."));}
                let mut bytes=[0u8;16];getrandom::fill(&mut bytes).map_err(|_|conflict())?;
                let bootstrap_resource=ResourceId::new(bytes.iter().map(|b|format!("{b:02x}")).collect::<String>())?;
                (Snapshot{resource:ResourceId::new(id.as_str())?,bootstrap_resource,physical,fingerprint:adapter.fingerprint,
                    original,original_saved,installed_saved:None,physical_bindings:bindings,tap:None,bootstrap:None,bridge:None,migration_before:None,options_before:None,reboot:None,action:Action::Configure,committed:false},
                    vec![Mutation::InstallTap,Mutation::InstallBootstrap,Mutation::CreateBridge,Mutation::AttachBootstrap,Mutation::CaptureBridge,
                        Mutation::AttachPhysical,Mutation::PreserveAddress,Mutation::CaptureMigration,Mutation::StaticConfiguration,Mutation::CaptureOptions,Mutation::Options,Mutation::DetachBootstrap,Mutation::RemoveBootstrap])
            },
            PlanRequest::Repair{resource_id}|PlanRequest::Remove{resource_id}=>{
                let resource=existing.filter(|r|r.owner==*owner && r.id==*resource_id).ok_or_else(conflict)?;
                let mut snapshot=resource.original.clone();Self::physical(&snapshot)?;
                let bridge=Self::owned(&snapshot,&topology)?.ok_or_else(missing)?;
                snapshot.original_saved.verify(&snapshot.physical)?;
                snapshot.installed_saved.as_ref().ok_or_else(missing)?.verify(&bridge)?;
                Self::installed_address(&snapshot)?;
                let saved=Saved::read(&bridge)?;
                let members=Self::members(&snapshot,&topology)?;
                let attached=members.contains(&snapshot.physical);
                let bindings=topology.bindings(&bridge.guid)?;
                let ipv4=*bindings.get("ms_tcpip").ok_or_else(conflict)?;let ipv6=*bindings.get("ms_tcpip6").ok_or_else(conflict)?;
                // Windows removes active IP/DNS objects when their bindings
                // are disabled. The saved store still establishes whether a
                // repair can re-enable those exact recorded settings.
                let profile=if ipv4 && ipv6 {Some(Profile::read(&bridge)?)}else{None};
                if profile.as_ref().is_some_and(|profile|!snapshot.original.configuration_changes(profile).is_empty()){return Err(conflict());}
                if !attached{Self::original_network(&snapshot,false)?;}
                let prior=Prior{profile,saved,physical_attached:attached,ipv4,ipv6};
                let removing=matches!(request,PlanRequest::Remove{..});
                snapshot.action=if removing{Action::Remove(prior)}else{Action::Repair(prior)};snapshot.committed=false;
                let mutations=if removing{vec![Mutation::DisableIpv4,Mutation::DisableIpv6,Mutation::DetachPhysical]}
                    else{vec![Mutation::RepairAttach,Mutation::RepairIpv4,Mutation::RepairIpv6]};
                (snapshot,mutations)
            },
        };
        let changes=if snapshot.removing(){vec!["Restore Ethernet and check its original connection settings.".into(),"Remove this app's bridge and TAP adapters after you keep the restored connection.".into()]}
            else{vec!["Create a dedicated TAP connection and attach it to Ethernet.".into(),"Preserve the host's IP settings, routes, DNS, and hardware address.".into(),"Restore the previous connection unless you keep it within two minutes.".into()]};
        let plan=BridgePlan{id,request,fingerprint:snapshot.fingerprint.clone(),changes,requires_authorization:true,may_disconnect:true,expires_at:now.saturating_add(300)};
        Ok(Prepared{plan,snapshot,mutations})
    }
    fn fingerprint(&mut self,request:&PlanRequest,s:&Snapshot)->Result<String,BridgeError>{
        match request{
            PlanRequest::Configure{adapter_id} if adapter_id.as_str()!=s.physical.guid=>return Err(conflict()),
            PlanRequest::Remove{resource_id}|PlanRequest::Repair{resource_id} if resource_id!=&s.resource=>return Err(conflict()),_=>(),
        }
        Self::physical(s)?;
        if matches!(s.action,Action::Configure){Self::original_network(s,true)?;Topology::read()?.require_unbridged(&s.physical)?;}
        else{
            let topology=Topology::read()?;let members=Self::members(s,&topology)?;let prior=s.prior().ok_or_else(conflict)?;
            s.original_saved.verify(&s.physical)?;prior.saved.verify(&s.bridge()?.device)?;
            Self::installed_address(s)?;
            if members.contains(&s.physical)!=prior.physical_attached{return Err(conflict());}
            let bindings=topology.bindings(&s.bridge()?.device.guid)?;
            if bindings.get("ms_tcpip")!=Some(&prior.ipv4) || bindings.get("ms_tcpip6")!=Some(&prior.ipv6){return Err(conflict());}
            if let Some(expected)=&prior.profile{
                let profile=Profile::read(&s.bridge()?.device)?;
                if !expected.configuration_changes(&profile).is_empty() || !expected.runtime_changes(&profile)?.is_empty(){return Err(conflict());}
            }
        }
        Ok(s.fingerprint.clone())
    }
    fn apply(&mut self,_:&Mutation)->Result<(),BridgeError>{Err(BridgeError::new("EPROTOCOL","Windows networking requires a durable operation snapshot."))}
    fn mutation_stage(&self,step:&Mutation)->&'static str{
        match step {
            Mutation::InstallTap|Mutation::InstallBootstrap=>"Install networking support",
            Mutation::CreateBridge|Mutation::AttachBootstrap|Mutation::CaptureBridge=>"Prepare bridge connection",
            Mutation::AttachPhysical|Mutation::PreserveAddress|Mutation::CaptureMigration=>"Connect Ethernet",
            Mutation::StaticConfiguration|Mutation::CaptureOptions|Mutation::Options=>"Preserve connection settings",
            Mutation::DetachBootstrap|Mutation::RemoveBootstrap=>"Finish bridge setup",
            Mutation::DisableIpv4|Mutation::DisableIpv6|Mutation::DetachPhysical=>"Restore Ethernet connection",
            Mutation::RepairAttach|Mutation::RepairIpv4|Mutation::RepairIpv6=>"Repair bridge connection",
        }
    }
    fn apply_recorded(&mut self,step:&Mutation,s:&mut Snapshot)->Result<(),BridgeError>{
        match step {
            Mutation::InstallTap|Mutation::InstallBootstrap=>{
                let primary=matches!(step,Mutation::InstallTap);let resource=if primary{&s.resource}else{&s.bootstrap_resource};
                let boot=windows_boot::current()?;let installed=windows_driver::create(&VerifiedPackage::open(&self.package)?,resource)?;
                if primary{s.tap=installed.identity;}else{s.bootstrap=installed.identity;}
                if installed.reboot_required{s.reboot=Some(boot);}
            },
            Mutation::CreateBridge=>{
                let tap=s.tap()?;let bootstrap=s.bootstrap.as_ref().ok_or_else(missing)?;
                let bridge=membership::create_isolated(&s.resource,tap,&s.bootstrap_resource,bootstrap,&s.physical)?;
                // No physical changes until the subsequent captured profile is durable.
                let baseline=Profile::read(&bridge)?;let address=native::address(&bridge)?;
                let bindings=Topology::read()?.bindings(&bridge.guid)?.clone();
                let saved=Saved::read(&bridge)?;
                s.bridge=Some(Bridge{device:bridge,address,baseline,saved,bindings});
            },
            Mutation::AttachBootstrap=>membership::set_member(&s.bridge()?.device,&[s.tap()?.clone()],s.bootstrap.as_ref().ok_or_else(missing)?,true)?,
            Mutation::CaptureBridge=>{Self::original_network(s,true)?;let b=s.bridge.as_mut().ok_or_else(missing)?;b.address=native::address(&b.device)?;b.baseline=Profile::read(&b.device)?;b.saved=Saved::read(&b.device)?;s.original_saved.require_compatible_dns(&b.saved)?;},
            Mutation::AttachPhysical=>{
                Self::original_network(s,true)?;
                let topology=Topology::read()?;
                if topology.bindings(&s.physical.guid)?!=&s.physical_bindings{return Err(conflict());}
                membership::set_member(&s.bridge()?.device,&[s.tap()?.clone(),s.bootstrap.as_ref().ok_or_else(missing)?.clone()],&s.physical,true)?;
            },
            Mutation::PreserveAddress=>{let b=s.bridge()?;native::preserve_address(&b.device,&b.address,&s.original.mac)?;},
            Mutation::CaptureMigration=>s.migration_before=Some(Profile::read(&s.bridge()?.device)?),
            Mutation::StaticConfiguration=>{windows_static::populate(&s.bridge()?.device,s.migration_before.as_ref().ok_or_else(missing)?,&s.original)?;},
            Mutation::CaptureOptions=>s.options_before=Some(Profile::read(&s.bridge()?.device)?),
            Mutation::Options=>options::apply(&s.bridge()?.device,s.options_before.as_ref().ok_or_else(missing)?,&s.original)?,
            Mutation::DetachBootstrap=>membership::set_member(&s.bridge()?.device,&[s.tap()?.clone(),s.bootstrap.as_ref().ok_or_else(missing)?.clone(),s.physical.clone()],s.bootstrap.as_ref().ok_or_else(missing)?,false)?,
            Mutation::RemoveBootstrap=>{if windows_driver::remove(&s.bootstrap_resource,s.bootstrap.as_ref())?{return Err(BridgeError::new("EREBOOT","Windows requires a restart to remove the temporary setup adapter."));}},
            Mutation::DisableIpv4=>Self::binding(s,HostProtocol::Ipv4,s.prior().ok_or_else(missing)?.ipv4,false)?,
            Mutation::DisableIpv6=>Self::binding(s,HostProtocol::Ipv6,s.prior().ok_or_else(missing)?.ipv6,false)?,
            Mutation::DetachPhysical=>membership::set_member(&s.bridge()?.device,&[s.tap()?.clone(),s.physical.clone()],&s.physical,false)?,
            Mutation::RepairAttach=>membership::set_member(&s.bridge()?.device,&[s.tap()?.clone()],&s.physical,true)?,
            Mutation::RepairIpv4=>Self::binding(s,HostProtocol::Ipv4,s.prior().ok_or_else(missing)?.ipv4,true)?,
            Mutation::RepairIpv6=>Self::binding(s,HostProtocol::Ipv6,s.prior().ok_or_else(missing)?.ipv6,true)?,
        }
        Ok(())
    }
    fn awaiting_reboot(&self,s:&Snapshot)->bool{s.reboot.is_some()}
    fn resume_after_reboot(&mut self,s:&mut Snapshot)->Result<(),BridgeError>{
        if s.reboot.as_ref()==Some(&windows_boot::current()?){return Err(BridgeError::new("EREBOOT","Restart Windows before continuing setup."));}
        let devices=windows_device::enumerate()?;
        for (resource,target) in [(&s.resource,&mut s.tap),(&s.bootstrap_resource,&mut s.bootstrap)]{
            if let Some(device)=devices.values().find(|d|d.instance_id==windows_driver::instance_id(resource)){
                if !device.is_tap() || target.as_ref().is_some_and(|old|old!=device){return Err(conflict());}
                windows_verify::probe(&device.guid)?;*target=Some(device.clone());
            }
        }
        if s.tap.is_none(){return Err(missing());}s.reboot=None;Ok(())
    }
    fn verify(&mut self,s:&Snapshot)->Result<(),BridgeError>{
        let topology=Topology::read()?;
        if s.removing(){
            s.prior().ok_or_else(missing)?.saved.verify(&s.bridge()?.device)?;s.original_saved.verify(&s.physical)?;
            topology.require_transition_members(&s.bridge()?.device,&[s.tap()?.clone()])?;
            if topology.bindings(&s.physical.guid)?!=&s.physical_bindings{return Err(conflict());}
            let prior=s.prior().ok_or_else(missing)?;
            Self::await_network(s,&s.physical,prior.profile.as_ref().unwrap_or(&s.original),prior.profile.is_some())
        }else{
            topology.require_members(&s.bridge()?.device,&[s.tap()?.clone(),s.physical.clone()])?;
            let (expected,immediate)=match &s.action{
                Action::Repair(prior)=>(prior.profile.as_ref().unwrap_or(&s.original),prior.profile.is_some() && !s.committed),
                _=>(&s.original,!s.committed),
            };
            Self::await_network(s,&s.bridge()?.device,expected,immediate)?;
            if !s.original_saved.same_configuration(&Saved::read(&s.bridge()?.device)?){return Err(conflict());}
            windows_verify::probe(&s.tap()?.guid).map(|_|())
        }
    }
    fn commit(&mut self,_:&Snapshot)->Result<(),BridgeError>{Ok(())}
    fn commit_recorded(&mut self,s:&mut Snapshot)->Result<(),BridgeError>{
        if !s.removing(){s.installed_saved=Some(Saved::read(&s.bridge()?.device)?);}
        s.committed=true;Ok(())
    }
    fn needs_committed_cleanup(&self,s:&Snapshot)->bool{s.removing()}
    fn cleanup_committed(&mut self,s:&Snapshot)->Result<(),BridgeError>{
        Self::destroy_owned(s)?;Self::remove_devices(s)?;s.original_saved.verify(&s.physical)?;
        Self::await_network(s,&s.physical,&s.original,false)
    }
    fn rollback(&mut self,s:&Snapshot)->Result<(),BridgeError>{
        if matches!(s.action,Action::Configure){
            Self::destroy_owned(s)?;Self::remove_devices(s)?;s.original_saved.verify(&s.physical)?;
            let topology=Topology::read()?;topology.require_unbridged(&s.physical)?;
            if topology.bindings(&s.physical.guid)?!=&s.physical_bindings{return Err(conflict());}
            Self::await_network(s,&s.physical,&s.original,false)
        }else{
            let prior=s.prior().ok_or_else(missing)?;let topology=Topology::read()?;Self::members(s,&topology)?;
            // Removal/repair only change bindings and membership. An outside
            // IP, DNS, or MAC edit is not permission to reconnect the adapters.
            s.original_saved.verify(&s.physical)?;prior.saved.verify(&s.bridge()?.device)?;
            Self::installed_address(s)?;
            if prior.physical_attached{
                membership::set_member(&s.bridge()?.device,&[s.tap()?.clone()],&s.physical,true)?;
            }else{
                // Release bridge addresses before restoring the detached state.
                Self::binding(s,HostProtocol::Ipv4,true,false)?;Self::binding(s,HostProtocol::Ipv6,true,false)?;
                membership::set_member(&s.bridge()?.device,&[s.tap()?.clone(),s.physical.clone()],&s.physical,false)?;
            }
            Self::binding(s,HostProtocol::Ipv4,!prior.ipv4,prior.ipv4)?;Self::binding(s,HostProtocol::Ipv6,!prior.ipv6,prior.ipv6)?;
            prior.saved.verify(&s.bridge()?.device)?;
            if let Some(profile)=&prior.profile{
                Self::await_network(s,&s.bridge()?.device,profile,prior.physical_attached)?;
            }
            if !prior.physical_attached{Self::original_network(s,false)?;}
            Ok(())
        }
    }
}
impl crate::windows_dispatch::WindowsPlatformOps for WindowsPlatform {
    fn target(s:&Snapshot)->Result<LaunchTarget,BridgeError>{windows_verify::target(&s.tap()?.guid)}
}

#[cfg(test)]
mod tests{
    use super::*;
    #[test]
    fn removal_requires_disabled_ip_dependencies_and_preserves_other_binding_checks(){
        let before=BTreeMap::from([("ms_tcpip".into(),true),("ms_tcpip6".into(),true),
            ("ms_msclient".into(),true),("ms_server".into(),true),("ms_bridge".into(),true),
            ("ms_implat".into(),false),("third_party_filter".into(),true)]);
        let expected=removal_bindings(&before);
        for component in ["ms_tcpip","ms_tcpip6","ms_msclient","ms_server"]{assert_eq!(expected[component],false);}
        for component in ["ms_bridge","ms_implat","third_party_filter"]{assert_eq!(expected[component],before[component]);}
        assert_eq!(expected.len(),before.len());
        let mut changed=expected.clone();changed.insert("third_party_filter".into(),false);
        assert_ne!(changed,expected);
    }
    fn profile()->Profile{
        let mut profile:Profile=serde_json::from_str(include_str!("../../../tests/fixtures/windows-dhcp-profile.json")).unwrap();
        profile.normalize().unwrap();profile
    }
    #[test]
    fn recovery_waits_for_router_mtu_without_waiving_settings_or_connectivity(){
        let mut expected=profile();
        expected.persistent_interfaces=Some(expected.interfaces.iter().map(crate::windows_profile::Interface::sparse).collect());
        expected.persistent_interfaces.as_mut().unwrap()[1].nl_mtu=None;
        expected.interfaces[1].router_discovery=1;
        expected.interfaces[1].nl_mtu=1280;
        let mut pending=expected.clone();pending.interfaces[1].nl_mtu=1500;
        assert_eq!(network_ready(&expected,&pending,false).unwrap(),false);
        assert_eq!(network_ready(&expected,&expected,false).unwrap(),true);
        pending=expected.clone();pending.active_routes.retain(|r|r.destination_prefix!="::/0");
        assert_eq!(network_ready(&expected,&pending,false).unwrap(),false);
        for (family,saved,discovery) in [(0,false,1),(1,true,1),(1,false,0)] {
            let mut before=expected.clone();before.interfaces[family].router_discovery=discovery;
            if saved{before.persistent_interfaces.as_mut().unwrap()[family].nl_mtu=Some(1280);}
            let mut changed=before.clone();changed.interfaces[family].nl_mtu=1400;
            assert_eq!(network_ready(&before,&changed,false).unwrap_err().code,"ECONFLICT");
        }
        pending=expected.clone();pending.interfaces[1].nl_mtu=1500;pending.interfaces[1].forwarding=1;
        assert_eq!(network_ready(&expected,&pending,false).unwrap_err().code,"ECONFLICT");
        expected.persistent_interfaces=None;
        pending=expected.clone();pending.interfaces[1].nl_mtu=1500;
        assert_eq!(network_ready(&expected,&pending,false).unwrap_err().code,"ECONFLICT");
    }
    #[test]
    fn committed_readiness_allows_lease_rotation_but_requires_global_connectivity(){
        let before=profile();let mut current=before.clone();
        current.active_addresses.iter_mut().find(|a|a.address_family==2).unwrap().ip_address="192.0.2.99".into();
        assert!(usable(&before,&current));
        current.active_addresses.retain(|a|a.address_family!=23 || a.ip_address.starts_with("fe80:"));
        assert!(!usable(&before,&current));
        current=before.clone();current.active_routes.retain(|r|r.destination_prefix!="0.0.0.0/0");
        assert!(!usable(&before,&current));
    }
    #[test]
    fn committed_readiness_requires_active_manual_routes_and_dns(){
        let mut before=profile();let mut route=before.active_routes.iter().find(|r|r.destination_prefix=="0.0.0.0/0").unwrap().clone();
        route.destination_prefix="198.51.100.0/24".into();before.routes.push(route.clone());before.active_routes.push(route.clone());
        assert!(usable(&before,&before));
        let mut current=before.clone();current.active_routes.retain(|r|r!=&route);assert!(!usable(&before,&current));
        current=before.clone();before.manual_dns[0].name_server="192.0.2.53".into();
        assert!(!usable(&before,&current));
        current.dns[0].server_addresses=vec!["192.0.2.53".into()];assert!(usable(&before,&current));
    }
    #[test]
    fn existing_link_local_only_networks_do_not_gain_a_new_global_requirement(){
        let mut before=profile();before.active_addresses.retain(|a|a.address_family==23 && a.ip_address.starts_with("fe80:"));
        before.active_routes.retain(|r|r.protocol==2);before.dns.iter_mut().for_each(|d|d.server_addresses.clear());
        assert!(usable(&before,&before));
    }
}
