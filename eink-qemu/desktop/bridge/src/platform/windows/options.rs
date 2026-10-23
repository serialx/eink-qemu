//! Per-adapter IP options, separate from address/route migration. The caller
//! journals both snapshots before applying and owns all affected identities.
use crate::{model::BridgeError,windows_command::{self,Tool},windows_device::{self,DeviceIdentity},windows_profile::Snapshot};
use std::{ffi::OsStr,time::Duration};
fn conflict()->BridgeError{BridgeError::new("ECONFLICT","The connection settings changed outside bridge setup. Review the current configuration.")}
fn quoted(value:&impl serde::Serialize)->Result<String,BridgeError>{
    Ok(format!("'{}'",serde_json::to_string(value).map_err(|_|conflict())?.replace('\'',"''")))
}
/// Validate the complete option transition during planning, before any host
/// membership changes. In particular, missing legacy properties are not zero
/// values, and saved overrides cannot be erased through this setter.
pub fn validate_transition(before:&Snapshot,desired:&Snapshot)->Result<(),BridgeError>{
    let mut before=before.clone();let mut desired=desired.clone();
    before.normalize()?;desired.normalize()?;before.require_restorable()?;desired.require_restorable()?;
    // There is no supported per-option cmdlet to remove a saved override.
    // Refuse such an operation before any change; rollback deletes a newly
    // created owned bridge rather than rewriting defaults on the host NIC.
    for (old,new) in before.persistent_interfaces.as_ref().unwrap().iter().zip(desired.persistent_interfaces.as_ref().unwrap()) {
        let old=serde_json::to_value(old).map_err(|_|conflict())?;
        let new=serde_json::to_value(new).map_err(|_|conflict())?;
        if old.as_object().unwrap().iter().any(|(key,value)|!value.is_null() && new[key].is_null()) {
            return Err(BridgeError::new("EHOSTNETWORK","Removing a saved interface override requires recreating the app-owned bridge."));
        }
    }
    let old=before.legacy.as_ref().ok_or_else(conflict)?;let new=desired.legacy.as_ref().ok_or_else(conflict)?;
    if old.wins_enable_lm_hosts_lookup!=new.wins_enable_lm_hosts_lookup || old.wins_scope_id!=new.wins_scope_id{return Err(conflict());}
    if new.manual_servers.as_ref().is_some_and(|servers|servers.len()>2)
        || (old.tcpip_netbios_options.is_none() != new.tcpip_netbios_options.is_none())
        || before.manual_dns[1].domain!=desired.manual_dns[1].domain {
        return Err(BridgeError::new("EHOSTNETWORK","This adapter has extended legacy settings that need a compatible native migration method."));
    }
    Ok(())
}
/// Read-only conflict check for restoration of a partially applied change.
/// Each value must belong to a recorded state; a third-party edit is not
/// permission to delete the destination or overwrite that field.
pub fn validate_current(before:&Snapshot,desired:&Snapshot,current:&Snapshot)->Result<(),BridgeError>{
    let mut before=before.clone();let mut desired=desired.clone();let mut current=current.clone();
    for value in [&mut before,&mut desired,&mut current]{value.normalize()?;value.require_restorable()?;}
    if before.guid!=current.guid || !before.instance_id.eq_ignore_ascii_case(&current.instance_id){return Err(conflict());}
    fn values(value:&impl serde::Serialize)->Result<serde_json::Value,BridgeError>{serde_json::to_value(value).map_err(|_|conflict())}
    for family in 0..2 {
        let enabled=current.configured_dhcp.as_ref().unwrap()[family].enabled;
        if enabled!=before.configured_dhcp.as_ref().unwrap()[family].enabled && enabled!=desired.configured_dhcp.as_ref().unwrap()[family].enabled{return Err(conflict());}
        let old=values(&before.interfaces[family])?;let new=values(&desired.interfaces[family])?;
        let old_saved=values(&before.persistent_interfaces.as_ref().unwrap()[family])?;
        let new_saved=values(&desired.persistent_interfaces.as_ref().unwrap()[family])?;
        let active=values(&current.interfaces[family])?;
        let saved=values(&current.persistent_interfaces.as_ref().unwrap()[family])?;
        for (field,value) in saved.as_object().unwrap(){if value!=&old_saved[field] && value!=&new_saved[field]{return Err(conflict());}}
        for (field,value) in active.as_object().unwrap(){
            if field=="Dhcp" || (field=="InterfaceMetric" && active["AutomaticMetric"]==1){continue;}
            if [&old[field],&new[field],&old_saved[field],&new_saved[field]].into_iter().all(|known|known!=value){return Err(conflict());}
        }
    }
    for i in 0..2{
        let old=&before.manual_dns[i];let new=&desired.manual_dns[i];let actual=&current.manual_dns[i];
        if (actual.name_server!=old.name_server && actual.name_server!=new.name_server)
            || (actual.domain!=old.domain && actual.domain!=new.domain){return Err(conflict());}
    }
    let client=&current.dns_client[0];let old=&before.dns_client[0];let new=&desired.dns_client[0];
    if (client.register_this_connections_address!=old.register_this_connections_address && client.register_this_connections_address!=new.register_this_connections_address)
        || (client.use_suffix_when_registering!=old.use_suffix_when_registering && client.use_suffix_when_registering!=new.use_suffix_when_registering){return Err(conflict());}
    let actual=values(current.legacy.as_ref().unwrap())?;let old=values(before.legacy.as_ref().unwrap())?;let new=values(desired.legacy.as_ref().unwrap())?;
    for (field,value) in actual.as_object().unwrap(){
        // Runtime WINS values may be learned from DHCP. ManualServers is the
        // restoration input and is checked independently below.
        if matches!(field.as_str(),"WINSPrimaryServer"|"WINSSecondaryServer"){continue;}
        if value!=&old[field] && value!=&new[field]{return Err(conflict());}
    }
    if current.addresses.iter().any(|r|!before.addresses.contains(r) && !desired.addresses.contains(r))
        || current.routes.iter().any(|r|!before.routes.contains(r) && !desired.routes.contains(r)){return Err(conflict());}
    for actual in current.active_addresses.iter().filter(|a|a.prefix_origin==1){
        if !before.active_addresses.iter().chain(&before.addresses).chain(&desired.addresses).any(|known|
            known.ip_address==actual.ip_address && known.prefix_length==actual.prefix_length && known.skip_as_source==actual.skip_as_source){return Err(conflict());}
    }
    Ok(())
}

/// Validate the app-owned bridge before removing an interrupted setup.
pub fn validate_owned_removal(before:&Snapshot,desired:&Snapshot,current:&Snapshot,baseline:&Snapshot)->Result<(),BridgeError>{
    if validate_current(before,desired,current).is_ok(){return Ok(());}
    let mut before=before.clone();let mut desired=desired.clone();let mut current=current.clone();let mut baseline=baseline.clone();
    for value in [&mut before,&mut desired,&mut current,&mut baseline]{value.normalize()?;value.require_restorable()?;}
    if baseline.guid!=current.guid || !baseline.instance_id.eq_ignore_ascii_case(&current.instance_id){return Err(conflict());}
    // A reboot can discard an RA-learned MTU and restore the bridge's recorded
    // initial value. Accept only that known runtime reset for destruction of
    // this owned bridge, never a saved override or a different active value.
    // The physical NIC still has to pass exact restoration and readiness.
    let router_default=[&before,&desired,&current,&baseline].into_iter().all(|profile|
        profile.interfaces[1].router_discovery==1
        && profile.persistent_interfaces.as_ref().unwrap()[1].nl_mtu.is_none());
    if !router_default || current.interfaces[1].nl_mtu!=baseline.interfaces[1].nl_mtu{return Err(conflict());}
    before.interfaces[1].nl_mtu=baseline.interfaces[1].nl_mtu;
    validate_current(&before,&desired,&current)
}

/// Apply the intended options or finish a partially applied operation. Each
/// field must still match one of the recorded states. This function may change
/// DHCP runtime state; callers must also retain address/route restoration data.
/// Global WINS options are checked, never changed on behalf of one adapter.
pub fn apply(target:&DeviceIdentity,before:&Snapshot,desired:&Snapshot)->Result<(),BridgeError>{
    let mut before=before.clone();let mut desired=desired.clone();
    before.normalize()?;desired.normalize()?;before.require_restorable()?;desired.require_restorable()?;
    if before.guid!=target.guid || !before.instance_id.eq_ignore_ascii_case(&target.instance_id)
        || windows_device::enumerate()?.get(&target.guid)!=Some(target){return Err(conflict());}
    validate_transition(&before,&desired)?;
    let script=format!("$ErrorActionPreference='Stop'; [Console]::OutputEncoding=New-Object System.Text.UTF8Encoding($false);\n{}\n$guid='{}';$before={}|ConvertFrom-Json;$desired={}|ConvertFrom-Json;\n{}\n[Console]::Out.Write((Read-EinkNetwork $guid|ConvertTo-Json -Depth 8 -Compress))",
        include_str!("ip_snapshot.ps1"),windows_device::guid(&target.guid)?,quoted(&before)?,quoted(&desired)?,include_str!("ip_options.ps1"));
    if script.encode_utf16().count()>30000{return Err(BridgeError::new("EHOSTNETWORK","The adapter configuration is too large for the native options operation."));}
    let args=["-NoProfile","-NonInteractive","-Command",&script].map(OsStr::new);
    let output=windows_command::run(Tool::PowerShell,&args,Duration::from_secs(180))?;
    if output.code==73{return Err(conflict());}
    let output=output.checked()?;
    let mut current:Snapshot=serde_json::from_slice(&output.stdout).map_err(|_|conflict())?;current.normalize()?;
    if current.guid!=target.guid || !current.instance_id.eq_ignore_ascii_case(&target.instance_id)
        || windows_device::enumerate()?.get(&target.guid)!=Some(target){return Err(conflict());}
    // Address/route steps have their own guards and run separately. Do not
    // claim that updating options alone has completed network migration.
    let changes=desired.configuration_changes(&current);
    if changes.iter().any(|change|!matches!(*change,"static addresses"|"persistent routes")){return Err(conflict());}
    Ok(())
}

#[cfg(test)]
mod tests{
    use super::*;
    fn fixture()->Snapshot {
        let mut value:Snapshot=serde_json::from_str(include_str!("../../../tests/fixtures/windows-dhcp-profile.json")).unwrap();
        value.persistent_interfaces=Some(value.interfaces.iter().map(crate::windows_profile::Interface::sparse).collect());
        value.legacy=Some(crate::windows_profile::Legacy {tcpip_netbios_options:Some(0),wins_enable_lm_hosts_lookup:Some(true),
            wins_primary_server:None,wins_secondary_server:None,wins_scope_id:Some(String::new()),manual_servers:Some(Vec::new())});
        value.configured_dhcp=Some(value.interfaces.iter().map(|i|crate::windows_profile::ConfiguredDhcp{address_family:i.address_family,enabled:i.dhcp}).collect());
        value.normalize().unwrap();value.require_restorable().unwrap();value
    }
    #[test]
    fn rollback_accepts_partial_updates_but_preserves_external_edits(){
        let before=fixture();let mut desired=before.clone();
        desired.interfaces[0].nl_mtu=1400;desired.manual_dns[0].name_server="192.0.2.53".into();
        let mut partial=before.clone();partial.interfaces[0].nl_mtu=1400;
        validate_current(&before,&desired,&partial).unwrap();
        partial.manual_dns[0].domain="administrator.invalid".into();
        assert_eq!(validate_current(&before,&desired,&partial).unwrap_err().code,"ECONFLICT");
        partial=before.clone();partial.persistent_interfaces.as_mut().unwrap()[0].nl_mtu=Some(1300);
        assert_eq!(validate_current(&before,&desired,&partial).unwrap_err().code,"ECONFLICT");
        partial=before.clone();let mut address=partial.active_addresses[0].clone();
        address.ip_address="192.0.2.99".into();address.prefix_origin=1;partial.active_addresses.push(address);
        assert_eq!(validate_current(&before,&desired,&partial).unwrap_err().code,"ECONFLICT");
    }
    #[test]
    fn interrupted_removal_accepts_only_recorded_router_mtu_reset(){
        let mut baseline=fixture();
        for row in baseline.persistent_interfaces.as_mut().unwrap(){row.nl_mtu=None;}
        let mut before=baseline.clone();before.interfaces[1].nl_mtu=1280;
        let desired=before.clone();let current=baseline.clone();
        assert_eq!(validate_current(&before,&desired,&current).unwrap_err().code,"ECONFLICT");
        validate_owned_removal(&before,&desired,&current,&baseline).unwrap();
        for case in 0..10 {
            let mut old=before.clone();let mut new=desired.clone();let mut actual=current.clone();let mut initial=baseline.clone();
            match case {
                0=>actual.interfaces[1].nl_mtu=1400,
                1=>actual.interfaces[0].nl_mtu=1400,
                2=>actual.interfaces[1].forwarding=1,
                3=>actual.persistent_interfaces.as_mut().unwrap()[1].nl_mtu=Some(1500),
                4=>old.persistent_interfaces.as_mut().unwrap()[1].nl_mtu=Some(1280),
                5=>new.persistent_interfaces.as_mut().unwrap()[1].nl_mtu=Some(1280),
                6=>initial.persistent_interfaces.as_mut().unwrap()[1].nl_mtu=Some(1500),
                7=>{old.interfaces[1].router_discovery=0;new.interfaces[1].router_discovery=0;actual.interfaces[1].router_discovery=0;},
                8=>initial.instance_id="OTHER".into(),
                9=>actual.manual_dns[0].domain="administrator.invalid".into(),
                _=>unreachable!(),
            }
            assert_eq!(validate_owned_removal(&old,&new,&actual,&initial).unwrap_err().code,"ECONFLICT","case {case}");
        }
    }
    #[test]
    fn planning_refuses_unrestorable_options_before_native_work(){
        let before=fixture();let mut desired=before.clone();
        desired.persistent_interfaces.as_mut().unwrap()[0].nl_mtu=None;
        assert_eq!(validate_transition(&before,&desired).unwrap_err().code,"EHOSTNETWORK");
        desired=before.clone();desired.legacy.as_mut().unwrap().tcpip_netbios_options=None;
        assert_eq!(validate_transition(&before,&desired).unwrap_err().code,"EHOSTNETWORK");
        assert_eq!(validate_transition(&desired,&before).unwrap_err().code,"EHOSTNETWORK");
        desired=before.clone();desired.legacy.as_mut().unwrap().wins_enable_lm_hosts_lookup=Some(false);
        assert_eq!(validate_transition(&before,&desired).unwrap_err().code,"ECONFLICT");
        desired=before.clone();desired.manual_dns[1].domain="v6-only.invalid".into();
        assert_eq!(validate_transition(&before,&desired).unwrap_err().code,"EHOSTNETWORK");
        desired=before.clone();desired.interfaces[0].nl_mtu=1400;desired.manual_dns[0].name_server="192.0.2.53".into();
        validate_transition(&before,&desired).unwrap();validate_transition(&desired,&before).unwrap();
    }
    #[test]
    fn persistent_conflicts_cannot_hide_behind_matching_active_values(){
        let mut before=fixture();let mut desired=before.clone();
        before.persistent_interfaces.as_mut().unwrap()[0].nl_mtu=Some(1400);
        desired.persistent_interfaces.as_mut().unwrap()[0].nl_mtu=Some(1450);
        desired.interfaces[0].nl_mtu=1450;
        let mut changed=before.clone();changed.persistent_interfaces.as_mut().unwrap()[0].nl_mtu=Some(before.interfaces[0].nl_mtu);
        for late in [false,true] {
            let script=format!(r#"$ErrorActionPreference='Stop';$guid='{}';$before={}|ConvertFrom-Json;$desired={}|ConvertFrom-Json;$changed={}|ConvertFrom-Json
function Read-EinkNetwork {{ {} }}
function Get-NetAdapter {{ [pscustomobject]@{{InterfaceGuid=$guid;PnPDeviceID=$before.instanceId;ifIndex=7}} }}
function Get-NetIPInterface {{ param($InterfaceIndex,$AddressFamily,$PolicyStore); Family-Row $changed.persistentInterfaces $AddressFamily }}
function Get-ItemProperty {{ [pscustomobject]@{{EnableDHCP=1}} }}
function Set-NetIPInterface {{ throw 'Unexpected mutation' }}
{}
"#,before.guid,quoted(&before).unwrap(),quoted(&desired).unwrap(),quoted(&changed).unwrap(),
                if late {"$before"} else {"$changed"},include_str!("ip_options.ps1"));
            let args=["-NoProfile","-NonInteractive","-Command",&script].map(OsStr::new);
            let output=windows_command::run(Tool::PowerShell,&args,Duration::from_secs(120)).unwrap();
            assert_eq!(output.code,73,"late={late}: {:?}",output);
        }
    }
    #[test]
    fn snapshot_text_is_literal_and_fixed_native_script_parses(){
        let value=serde_json::json!({"domain":"x'; throw 'must-not-run'; #\n$(whoami)\"`", "servers":["192.0.2.53"]});
        let script=format!("$ErrorActionPreference='Stop';[Console]::OutputEncoding=New-Object System.Text.UTF8Encoding($false);$text={} | ConvertFrom-Json;$tokens=$null;$errors=$null;[void][System.Management.Automation.Language.Parser]::ParseInput($text,[ref]$tokens,[ref]$errors);if($errors.Count -ne 0){{throw ($errors|Out-String)}};$value={} | ConvertFrom-Json;[Console]::Out.Write(($value|ConvertTo-Json -Compress))",
            quoted(&include_str!("ip_options.ps1")).unwrap(),quoted(&value).unwrap());
        let args=["-NoProfile","-NonInteractive","-Command",&script].map(OsStr::new);
        let output=windows_command::run(Tool::PowerShell,&args,Duration::from_secs(120)).unwrap().checked().unwrap();
        assert_eq!(serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap(),value);
    }
}
