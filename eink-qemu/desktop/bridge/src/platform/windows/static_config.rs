//! Populate static addressing on a journaled, app-owned destination. Rollback
//! removes that destination; this module does not edit the physical adapter.
use crate::{model::BridgeError,windows_command::{self,Tool},windows_device::{self,DeviceIdentity},windows_profile::{Address,Route,Snapshot},windows_security};
use serde::Serialize;
use std::{collections::BTreeSet,ffi::OsStr,time::Duration};
fn conflict()->BridgeError{BridgeError::new("ECONFLICT","Static connection settings changed. Review bridge setup before continuing.")}
#[derive(Serialize)]
#[serde(rename_all="camelCase")]
struct Configuration<'a>{guid:&'a str,instance_id:&'a str,addresses:&'a [Address],routes:&'a [Route]}
fn configuration(value:&Snapshot)->Configuration<'_>{Configuration{guid:&value.guid,instance_id:&value.instance_id,addresses:&value.addresses,routes:&value.routes}}
fn quoted(value:&impl Serialize)->Result<String,BridgeError>{Ok(format!("'{}'",serde_json::to_string(value).map_err(|_|conflict())?.replace('\'',"''")))}
/// Validate during planning. Active-only manual addresses require a separate
/// restoration path and must not silently become persistent during conversion.
pub fn validate_source(source:&Snapshot)->Result<(),BridgeError>{
    let mut source=source.clone();source.normalize()?;source.require_restorable()?;
    let mut addresses=BTreeSet::new();let mut routes=BTreeSet::new();
    for row in &source.addresses{
        if row.prefix_origin!=1 || row.suffix_origin!=1 || !addresses.insert(&row.ip_address){return Err(conflict());}
        if row.address_family==2 && (source.interfaces.iter().any(|i|i.address_family==2 && i.dhcp!=0)
            || source.persistent_interfaces.as_ref().unwrap().iter().any(|i|i.address_family==2 && i.dhcp==Some(1))){
            return Err(BridgeError::new("EHOSTNETWORK","This connection combines DHCP and static IPv4 addresses and needs a compatible migration method."));
        }
    }
    for row in &source.active_addresses{
        if row.prefix_origin==1 && !source.addresses.iter().any(|saved|saved.ip_address==row.ip_address && saved.prefix_length==row.prefix_length && saved.skip_as_source==row.skip_as_source){
            return Err(BridgeError::new("EHOSTNETWORK","This connection has temporary manual addresses that cannot be made persistent by bridge setup."));
        }
    }
    for row in &source.routes{
        if row.protocol!=3 || row.publish>2 || row.route_metric>u16::MAX as u32 || !routes.insert((&row.destination_prefix,&row.next_hop)){return Err(conflict());}
    }
    Ok(())
}
/// The caller must establish destination ownership and journal `before` before
/// this call. Only bridge adapters or dedicated app TAPs are accepted. Existing
/// static entries must already match the desired configuration; no entries are
/// overwritten or removed. DHCP is disabled before adding manual addresses;
/// the caller applies the remaining source options after this step.
pub fn populate(target:&DeviceIdentity,before:&Snapshot,desired:&Snapshot)->Result<Snapshot,BridgeError>{
    windows_security::require_system()?;
    if !target.is_bridge() && !(target.is_tap() && target.instance_id.starts_with(r"ROOT\EINKQEMUTAP\")){return Err(conflict());}
    let mut before=before.clone();let mut desired=desired.clone();
    before.normalize()?;desired.normalize()?;before.require_restorable()?;validate_source(&desired)?;
    if before.guid!=target.guid || !before.instance_id.eq_ignore_ascii_case(&target.instance_id)
        || windows_device::enumerate()?.get(&target.guid)!=Some(target)
        || before.addresses.iter().any(|row|!desired.addresses.contains(row)) || before.routes.iter().any(|row|!desired.routes.contains(row)){return Err(conflict());}
    let script=format!("$ErrorActionPreference='Stop';[Console]::OutputEncoding=New-Object System.Text.UTF8Encoding($false);\n{}\n$guid='{}';$before={}|ConvertFrom-Json;$desired={}|ConvertFrom-Json;\n{}\n[Console]::Out.Write((Read-EinkNetwork $guid|ConvertTo-Json -Depth 8 -Compress))",
        include_str!("ip_snapshot.ps1"),windows_device::guid(&target.guid)?,quoted(&configuration(&before))?,quoted(&configuration(&desired))?,include_str!("ip_static.ps1"));
    if script.encode_utf16().count()>30000{return Err(BridgeError::new("EHOSTNETWORK","The static configuration is too large for a native bridge operation."));}
    let args=["-NoProfile","-NonInteractive","-Command",&script].map(OsStr::new);
    let output=windows_command::run(Tool::PowerShell,&args,Duration::from_secs(180))?;
    if output.code==73{return Err(conflict());}
    let output=output.checked()?;let mut current:Snapshot=serde_json::from_slice(&output.stdout).map_err(|_|conflict())?;current.normalize()?;current.require_restorable()?;
    if current.guid!=target.guid || !current.instance_id.eq_ignore_ascii_case(&target.instance_id)
        || windows_device::enumerate()?.get(&target.guid)!=Some(target) || current.addresses!=desired.addresses || current.routes!=desired.routes{return Err(conflict());}
    Ok(current)
}

#[cfg(test)]
mod tests{
    use super::*;
    fn fixture()->Snapshot{
        let mut value:Snapshot=serde_json::from_str(include_str!("../../../tests/fixtures/windows-dhcp-profile.json")).unwrap();
        value.persistent_interfaces=Some(value.interfaces.iter().map(crate::windows_profile::Interface::sparse).collect());
        value.legacy=Some(crate::windows_profile::Legacy{tcpip_netbios_options:Some(0),wins_enable_lm_hosts_lookup:Some(true),wins_primary_server:None,wins_secondary_server:None,wins_scope_id:Some(String::new()),manual_servers:Some(Vec::new())});
        value.normalize().unwrap();value
    }
    #[test]
    fn static_planning_rejects_temporary_manual_addresses_and_ambiguous_routes(){
        let mut value=fixture();validate_source(&value).unwrap();
        let mut address=value.active_addresses.iter().find(|row|row.address_family==2).unwrap().clone();
        address.prefix_origin=1;address.suffix_origin=1;address.address_state=None;
        address.valid_lifetime_ticks=Some(i64::MAX as u64);address.preferred_lifetime_ticks=Some(i64::MAX as u64);
        value.addresses.push(address.clone());
        assert_eq!(validate_source(&value).unwrap_err().code,"EHOSTNETWORK");
        value.interfaces.iter_mut().find(|row|row.address_family==2).unwrap().dhcp=0;
        value.persistent_interfaces.as_mut().unwrap().iter_mut().find(|row|row.address_family==2).unwrap().dhcp=Some(0);
        validate_source(&value).unwrap();
        address.ip_address="192.0.2.77".into();value.active_addresses.push(address);
        assert_eq!(validate_source(&value).unwrap_err().code,"EHOSTNETWORK");
        value.active_addresses.pop();
        let route=Route{destination_prefix:"198.51.100.0/24".into(),next_hop:"192.0.2.1".into(),route_metric:42,protocol:3,publish:0,valid_lifetime_ticks:Some(i64::MAX as u64),preferred_lifetime_ticks:Some(i64::MAX as u64)};
        value.routes.push(route.clone());validate_source(&value).unwrap();
        value.routes.push(Route{route_metric:43,..route});assert_eq!(validate_source(&value).unwrap_err().code,"ECONFLICT");
    }
    #[test]
    fn native_static_script_parses(){
        let script=format!("$ErrorActionPreference='Stop';$text={}|ConvertFrom-Json;$tokens=$null;$errors=$null;[void][System.Management.Automation.Language.Parser]::ParseInput($text,[ref]$tokens,[ref]$errors);if($errors.Count){{throw ($errors|Out-String)}}",quoted(&include_str!("ip_static.ps1")).unwrap());
        let args=["-NoProfile","-NonInteractive","-Command",&script].map(OsStr::new);
        windows_command::run(Tool::PowerShell,&args,Duration::from_secs(120)).unwrap().checked().unwrap();
    }
}
