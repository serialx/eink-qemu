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
        value.normalize().unwrap();value.require_restorable().unwrap();value
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
