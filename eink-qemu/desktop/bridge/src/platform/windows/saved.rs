//! Configuration that remains observable after disabling a bridge's host IP
//! bindings. DNS-client and active IP-interface CIM objects disappear then;
//! the persistent store, NetBT settings, and native DNS API remain the sources.
use crate::{model::BridgeError,windows_command::{self,Tool},windows_device::{self,DeviceIdentity},windows_profile::{Address,Route,Interface,ConfiguredDhcp}};
use serde::{Serialize,Deserialize};
use std::{ffi::OsStr,net::{IpAddr,Ipv4Addr},time::Duration};
use windows_sys::{core::GUID,Win32::NetworkManagement::IpHelper::*};
fn invalid()->BridgeError{BridgeError::new("EHOSTNETWORK","Saved Windows connection settings could not be verified.")}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
pub struct Dns {
    family:u32,domain:String,servers:String,search_list:String,profile_servers:String,
    registration:u32,register_adapter_name:u32,llmnr:u32,query_adapter_name:u32,
}
fn text(value:*const u16)->Result<String,BridgeError>{
    if value.is_null(){return Ok(String::new());}
    for length in 0..65536{
        if unsafe{*value.add(length)}==0{return String::from_utf16(unsafe{std::slice::from_raw_parts(value,length)}).map_err(|_|invalid());}
    }
    Err(invalid())
}
fn servers(value:&str,family:u32)->Result<String,BridgeError>{
    value.split(|c:char|c.is_ascii_whitespace() || c==',' || c==';').filter(|v|!v.is_empty()).map(|v|{
        let ip:IpAddr=v.parse().map_err(|_|invalid())?;
        if ip.is_ipv4()!=(family==2){return Err(invalid());}Ok(ip.to_string())
    }).collect::<Result<Vec<_>,BridgeError>>().map(|v|v.join(","))
}
fn dns(guid:&str)->Result<Vec<Dns>,BridgeError>{
    let guid=windows_device::guid(guid)?;
    let native=GUID::from_u128(u128::from_str_radix(&guid[1..37].replace('-',""),16).map_err(|_|invalid())?);
    struct Owned(DNS_INTERFACE_SETTINGS);
    impl Drop for Owned{fn drop(&mut self){unsafe{FreeInterfaceDnsSettings(&mut self.0)}}}
    let mut result=Vec::new();
    for family in [2,23]{
        let mut value=DNS_INTERFACE_SETTINGS{Version:DNS_INTERFACE_SETTINGS_VERSION1,Flags:if family==23{DNS_SETTING_IPV6 as u64}else{0},..Default::default()};
        let status=unsafe{GetInterfaceDnsSettings(native,&mut value)};
        if status!=0{return Err(BridgeError::new("EHOSTNETWORK",format!("Windows could not read saved DNS settings ({status}).")));}
        let value=Owned(value);let s=&value.0;
        if [s.RegistrationEnabled,s.RegisterAdapterName,s.EnableLLMNR,s.QueryAdapterName].iter().any(|v|*v>1){return Err(invalid());}
        result.push(Dns{family,domain:text(s.Domain)?,servers:servers(&text(s.NameServer)?,family)?,search_list:text(s.SearchList)?,
            profile_servers:servers(&text(s.ProfileNameServer)?,family)?,registration:s.RegistrationEnabled,
            register_adapter_name:s.RegisterAdapterName,llmnr:s.EnableLLMNR,query_adapter_name:s.QueryAdapterName});
    }
    Ok(result)
}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
pub struct Saved {
    guid:String,instance_id:String,interfaces:Vec<Interface<Option<u32>>>,addresses:Vec<Address>,routes:Vec<Route>,
    dhcp:Vec<ConfiguredDhcp>,netbios:u32,wins:Vec<String>,
    #[serde(default)] dns:Vec<Dns>,
}
impl Saved {
    pub fn read(expected:&DeviceIdentity)->Result<Self,BridgeError>{
        let guid=windows_device::guid(&expected.guid)?;
        if windows_device::enumerate()?.get(&guid)!=Some(expected){return Err(invalid());}
        let script=format!("$ErrorActionPreference='Stop';[Console]::OutputEncoding=New-Object System.Text.UTF8Encoding($false);$guid='{guid}';\n{}",include_str!("ip_saved.ps1"));
        let args=["-NoProfile","-NonInteractive","-Command",&script].map(OsStr::new);
        let output=windows_command::run(Tool::PowerShell,&args,Duration::from_secs(120))
            .and_then(|output|output.checked()).map_err(|error|error.context("Read saved Windows connection settings"))?;
        let mut saved:Self=serde_json::from_slice(&output.stdout).map_err(|_|invalid())?;
        saved.guid=windows_device::guid(&saved.guid)?;saved.instance_id=saved.instance_id.to_ascii_uppercase();
        if saved.guid!=guid || !saved.instance_id.eq_ignore_ascii_case(&expected.instance_id) || saved.netbios>2{return Err(invalid());}
        saved.interfaces.sort_by_key(|r|r.address_family);saved.dhcp.sort_by_key(|r|r.address_family);
        if saved.interfaces.iter().map(|r|r.address_family).collect::<Vec<_>>()!=[2,23]
            || saved.dhcp.iter().map(|r|r.address_family).collect::<Vec<_>>()!=[2,23] || saved.dhcp.iter().any(|r|r.enabled>1){return Err(invalid());}
        for row in &saved.interfaces{row.validate()?;}
        for row in &mut saved.addresses{
            let ip=crate::windows_profile::ip(&row.ip_address)?;
            if row.address_family!=if ip.is_ipv4(){2}else{23} || row.prefix_length>if ip.is_ipv4(){32}else{128}
                || row.valid_lifetime_ticks.is_none() || row.preferred_lifetime_ticks.is_none(){return Err(invalid());}
            crate::windows_profile::lifetimes(row.valid_lifetime_ticks,row.preferred_lifetime_ticks)?;
            row.ip_address=ip.to_string();
        }
        for row in &mut saved.routes{
            let (network,length)=crate::windows_profile::prefix(&row.destination_prefix)?;let next=crate::windows_profile::ip(&row.next_hop)?;
            if network.is_ipv4()!=next.is_ipv4() || row.valid_lifetime_ticks.is_none() || row.preferred_lifetime_ticks.is_none(){return Err(invalid());}
            crate::windows_profile::lifetimes(row.valid_lifetime_ticks,row.preferred_lifetime_ticks)?;
            row.destination_prefix=format!("{network}/{length}");row.next_hop=next.to_string();
        }
        saved.addresses.sort();saved.routes.sort();
        for value in &mut saved.wins{*value=value.parse::<Ipv4Addr>().map_err(|_|invalid())?.to_string();}
        saved.dns=dns(&guid)?;
        if windows_device::enumerate()?.get(&guid)!=Some(expected){return Err(invalid());}
        Ok(saved)
    }
    pub fn same_configuration(&self,current:&Self)->bool{
        self.interfaces==current.interfaces && self.addresses==current.addresses && self.routes==current.routes
            && self.dhcp==current.dhcp && self.netbios==current.netbios && self.wins==current.wins && self.dns==current.dns
    }
    pub(crate) fn during_static(&self)->Self{
        let mut value=self.clone();
        for address in &self.addresses{
            if let Some(row)=value.dhcp.iter_mut().find(|r|r.address_family==address.address_family){row.enabled=0;}
            if let Some(row)=value.interfaces.iter_mut().find(|r|r.address_family==address.address_family){if row.dhcp.is_some(){row.dhcp=Some(0);}}
        }
        value
    }
    pub(crate) fn validate_partial(&self,before:&Self,desired:&Self)->Result<(),BridgeError>{
        let conflict=||BridgeError::new("ECONFLICT","Saved connection settings changed outside the recorded bridge operation.");
        if self.guid!=before.guid || self.instance_id!=before.instance_id{return Err(conflict());}
        for value in [self,before,desired]{
            if value.interfaces.len()!=2 || value.dhcp.len()!=2 || value.dns.len()!=2{return Err(conflict());}
        }
        for index in 0..2 {
            let old=serde_json::to_value(&before.interfaces[index]).map_err(|_|invalid())?;
            let new=serde_json::to_value(&desired.interfaces[index]).map_err(|_|invalid())?;
            let current=serde_json::to_value(&self.interfaces[index]).map_err(|_|invalid())?;
            if current.as_object().unwrap().iter().any(|(key,value)|value!=&old[key] && value!=&new[key]){return Err(conflict());}
            if self.dhcp[index]!=before.dhcp[index] && self.dhcp[index]!=desired.dhcp[index]{return Err(conflict());}
            let old=&before.dns[index];let new=&desired.dns[index];let current=&self.dns[index];
            if current.family!=old.family || current.family!=new.family || current.search_list!=old.search_list
                || current.profile_servers!=old.profile_servers || current.llmnr!=old.llmnr || current.query_adapter_name!=old.query_adapter_name
                || (current.domain!=old.domain && current.domain!=new.domain) || (current.servers!=old.servers && current.servers!=new.servers)
                || (current.registration!=old.registration && current.registration!=new.registration)
                || (current.register_adapter_name!=old.register_adapter_name && current.register_adapter_name!=new.register_adapter_name){return Err(conflict());}
        }
        if (self.netbios!=before.netbios && self.netbios!=desired.netbios) || (self.wins!=before.wins && self.wins!=desired.wins)
            || self.addresses.iter().any(|r|!before.addresses.contains(r) && !desired.addresses.contains(r))
            || self.routes.iter().any(|r|!before.routes.contains(r) && !desired.routes.contains(r)){return Err(conflict());}
        Ok(())
    }
    pub fn verify(&self,expected:&DeviceIdentity)->Result<(),BridgeError>{
        let current=Self::read(expected)?;
        if self.guid!=current.guid || self.instance_id!=current.instance_id || !self.same_configuration(&current){
            return Err(BridgeError::new("ECONFLICT","Saved connection settings changed outside bridge setup. Review them before removing the bridge."));
        }
        Ok(())
    }
    /// The existing setter handles servers, suffix, and registration. Refuse
    /// extended DNS differences before attaching Ethernet instead of losing
    /// interface-specific search or name-resolution settings during migration.
    pub fn require_compatible_dns(&self,destination:&Self)->Result<(),BridgeError>{
        if self.dns.len()!=2 || destination.dns.len()!=2 || self.dns.iter().zip(&destination.dns).any(|(source,target)|
            source.family!=target.family || source.search_list!=target.search_list || source.profile_servers!=target.profile_servers
                || source.llmnr!=target.llmnr || source.query_adapter_name!=target.query_adapter_name){
            return Err(BridgeError::new("EHOSTNETWORK","This connection has additional DNS settings that automatic bridge setup cannot migrate."));
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests{
    use super::*;
    #[test]
    fn partial_saved_updates_refuse_unplanned_dns_and_netbios_changes(){
        let profile:crate::windows_profile::Snapshot=serde_json::from_str(include_str!("../../../tests/fixtures/windows-dhcp-profile.json")).unwrap();
        let before=Saved{guid:profile.guid,instance_id:profile.instance_id,interfaces:profile.interfaces.iter().map(Interface::sparse).collect(),
            addresses:Vec::new(),routes:Vec::new(),dhcp:profile.interfaces.iter().map(|i|ConfiguredDhcp{address_family:i.address_family,enabled:i.dhcp}).collect(),netbios:0,wins:Vec::new(),
            dns:[2,23].into_iter().map(|family|Dns{family,domain:String::new(),servers:String::new(),search_list:String::new(),profile_servers:String::new(),registration:1,register_adapter_name:0,llmnr:1,query_adapter_name:1}).collect()};
        let mut desired=before.clone();desired.dns[0].servers="192.0.2.53".into();desired.interfaces[0].nl_mtu=Some(1400);
        let mut partial=before.clone();partial.interfaces[0].nl_mtu=Some(1400);partial.validate_partial(&before,&desired).unwrap();
        partial.netbios=2;assert!(partial.validate_partial(&before,&desired).is_err());
        partial=before.clone();partial.dns[0].search_list="administrator.invalid".into();assert!(partial.validate_partial(&before,&desired).is_err());
        partial=before.clone();partial.dhcp.clear();assert!(partial.validate_partial(&before,&desired).is_err());
        desired.dns[0].llmnr=0;assert!(before.require_compatible_dns(&desired).is_err());
    }
    #[test]
    fn native_saved_profile_reopens_without_changing_identity(){
        let device=crate::inspect().unwrap().adapters.into_iter().find(|a|a.eligible).unwrap();
        let devices=windows_device::enumerate().unwrap();let target=devices.get(device.id.as_str()).unwrap();
        let saved=Saved::read(target).unwrap();saved.verify(target).unwrap();
    }
    #[test]
    fn dns_server_text_is_canonical_and_family_checked(){
        assert_eq!(servers("192.0.2.53; 192.0.2.54",2).unwrap(),"192.0.2.53,192.0.2.54");
        assert!(servers("2001:db8::53",2).is_err());assert!(servers("$(whoami)",2).is_err());
    }
}
