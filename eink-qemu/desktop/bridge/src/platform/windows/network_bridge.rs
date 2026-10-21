//! Fixed native operations on an already identified bridge. Friendly names,
//! executable paths, registry paths, and scripts never come from callers.
use crate::{model::BridgeError, windows_command::{self, Tool}, windows_device::{self, DeviceIdentity}};
use serde::{Deserialize, Serialize};
use std::{ffi::OsStr, time::Duration};

fn conflict() -> BridgeError { BridgeError::new("ECONFLICT", "The Windows bridge address changed. Review its network settings before continuing.") }
/// Twelve uppercase hexadecimal digits; accept factory and locally assigned
/// unicast addresses. Native verification must also confirm acceptance.
fn mac(value: &str) -> Result<String, BridgeError> {
    let compact = if value.len() == 17 && value.bytes().enumerate().all(|(index, c)| {
        if [2,5,8,11,14].contains(&index) { c == b'-' } else { c.is_ascii_hexdigit() }
    }) { value.replace('-', "") } else { value.to_string() };
    if compact.len() != 12 || !compact.bytes().all(|c| c.is_ascii_hexdigit())
        || compact.bytes().all(|c| c == b'0') || u8::from_str_radix(&compact[..2], 16).map_or(true, |byte| byte & 1 != 0) {
        return Err(BridgeError::new("EHOSTNETWORK", "The connection has no supported unicast hardware address."));
    }
    Ok(compact.to_ascii_uppercase())
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all="camelCase", deny_unknown_fields)]
pub struct BridgeAddress { pub current: String, pub configured: Option<String> }
#[derive(Deserialize)]
#[serde(rename_all="camelCase", deny_unknown_fields)]
struct NativeAddress { guid: String, instance_id: String, current: String, configured: Vec<String> }
const READ: &str = r#"
function Read-BridgeAddress($guid) {
 $adapter=@(Get-NetAdapter | Where-Object { $_.InterfaceGuid.ToString() -eq $guid })
 $property=@(Get-CimInstance -Namespace root/StandardCimv2 -ClassName MSFT_NetAdapterAdvancedPropertySettingData | Where-Object { $_.InstanceID -eq ($guid+'::NetworkAddress') })
 if ($adapter.Count -ne 1 -or $property.Count -ne 1 -or $property[0].RegistryKeyword -ne 'NetworkAddress' -or $adapter[0].ComponentID -ne 'COMPOSITEBUS\MS_IMPLAT_MP') { throw 'Bridge address identity changed' }
 $configured=@($property[0].RegistryValue | Where-Object { $null -ne $_ -and $_ -ne '' })
 if ($configured.Count -gt 1) { throw 'Unexpected bridge address configuration' }
 [ordered]@{guid=$adapter[0].InterfaceGuid.ToString();instanceId=$adapter[0].PnPDeviceID;current=$adapter[0].MacAddress;configured=$configured}
}
"#;
fn run(bridge: &DeviceIdentity, action: &str) -> Result<BridgeAddress, BridgeError> {
    if !bridge.is_bridge() || windows_device::enumerate()?.get(&bridge.guid) != Some(bridge) { return Err(conflict()); }
    let guid = windows_device::guid(&bridge.guid)?;
    let script = format!("$ErrorActionPreference='Stop'; [Console]::OutputEncoding=New-Object System.Text.UTF8Encoding($false);\n{READ}\n$guid='{guid}';\n{action}\n[Console]::Out.Write((Read-BridgeAddress $guid | ConvertTo-Json -Depth 3 -Compress))");
    let args = ["-NoProfile", "-NonInteractive", "-Command", &script].map(OsStr::new);
    let output = windows_command::run(Tool::PowerShell, &args, Duration::from_secs(120))?;
    if output.code == 73 { return Err(conflict()); }
    let output = output.checked()?;
    let native: NativeAddress = serde_json::from_slice(&output.stdout).map_err(|_| conflict())?;
    if windows_device::guid(&native.guid)? != guid || !native.instance_id.eq_ignore_ascii_case(&bridge.instance_id)
        || native.configured.len() > 1 || windows_device::enumerate()?.get(&guid) != Some(bridge) { return Err(conflict()); }
    Ok(BridgeAddress { current: mac(&native.current)?, configured: native.configured.first().map(|value| mac(value)).transpose()? })
}
pub fn address(bridge: &DeviceIdentity) -> Result<BridgeAddress, BridgeError> { run(bridge, "") }

/// Persist `before` and the intended address in the transaction journal before
/// calling. Requires either the recorded old state or the exact completed state;
/// an administrator's intervening address change is never overwritten.
pub fn preserve_address(bridge: &DeviceIdentity, before: &BridgeAddress, desired: &str) -> Result<BridgeAddress, BridgeError> {
    let desired = mac(desired)?;
    let previous = mac(&before.current)?;
    let configured = before.configured.as_deref().map(mac).transpose()?.unwrap_or_default();
    let result = run(bridge, &change_script(&previous, &configured, &desired))?;
    if result.current != desired || result.configured.as_deref() != Some(&desired) { return Err(conflict()); }
    Ok(result)
}
fn change_script(previous: &str, configured: &str, desired: &str) -> String {
    // All interpolated values have been reduced to hexadecimal digits.
    format!(r#"
$before=Read-BridgeAddress $guid
$current=([string]$before.current).Replace('-','').ToUpperInvariant()
$configured=''
foreach ($value in $before.configured) {{ $configured="$value".Replace('-','').ToUpperInvariant() }}
if ($current -ne '{desired}' -or $configured -ne '{desired}') {{
 if ($current -ne '{previous}' -or $configured -ne '{configured}') {{ exit 73 }}
 $property=@(Get-CimInstance -Namespace root/StandardCimv2 -ClassName MSFT_NetAdapterAdvancedPropertySettingData | Where-Object {{ $_.InstanceID -eq ($guid+'::NetworkAddress') }})
 if ($property.Count -ne 1) {{ throw 'Bridge address property changed' }}
 Set-NetAdapterAdvancedProperty -InputObject $property[0] -RegistryValue '{desired}' -Confirm:$false
}}
# The setter can return before NDIS finishes restarting the virtual adapter.
# Retry reads only; never repeat the mutation or overwrite an intervening edit.
$deadline=[DateTime]::UtcNow.AddSeconds(30)
$ready=$false
do {{
 try {{
  $observed=Read-BridgeAddress $guid
  $live=([string]$observed.current).Replace('-','').ToUpperInvariant()
  $saved=''
  foreach ($value in $observed.configured) {{ $saved="$value".Replace('-','').ToUpperInvariant() }}
  if ($live -eq '{desired}' -and $saved -eq '{desired}') {{ $ready=$true; break }}
 }} catch {{ }}
 Start-Sleep -Milliseconds 250
}} while ([DateTime]::UtcNow -lt $deadline)
if (!$ready) {{ throw 'The bridge address did not become ready' }}
"#)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bridge_addresses_accept_factory_unicast_and_reject_commands_or_multicast() {
        assert_eq!(mac("00-11-22-33-44-55").unwrap(), "001122334455");
        assert_eq!(mac("4ae293973b07").unwrap(), "4AE293973B07");
        for value in ["", "00-00-00-00-00-00", "FF-FF-FF-FF-FF-FF", "01-11-22-33-44-55", "00:11:22:33:44:55", "001122334455;whoami", "$(whoami)", " 001122334455", "00112233445z"] {
            assert!(mac(value).is_err(), "{value}");
        }
    }
    #[test]
    fn powershell_handles_an_unset_override_and_distinguishes_conflicts() {
        // Exercise the actual PowerShell language and production script with
        // in-memory cmdlets. This test never reads or changes real adapters.
        let mock = r#"
$ErrorActionPreference='Stop'
$guid='{00000000-0000-0000-0000-000000000001}'
$script:address='00-FF-11-22-33-44'; $script:override=@(); $script:sets=0
function Get-NetAdapter { [pscustomobject]@{InterfaceGuid=$guid;MacAddress=$script:address;ComponentID='COMPOSITEBUS\MS_IMPLAT_MP';PnPDeviceID='TEST'} }
function Get-CimInstance { [pscustomobject]@{InstanceID=($guid+'::NetworkAddress');RegistryKeyword='NetworkAddress';RegistryValue=$script:override} }
function Set-NetAdapterAdvancedProperty {
 param($InputObject,$RegistryValue,[switch]$Confirm)
 $script:sets++; $script:address=$RegistryValue; $script:override=@($RegistryValue)
 [Console]::Out.Write('mutation;')
}
"#;
        let action = change_script("00FF11223344", "", "4AE293973B07");
        let script = format!("{mock}\n{READ}\n{action}\n{action}\nif ($script:sets -ne 1) {{ throw 'Repeated mutation' }}");
        let args = ["-NoProfile", "-NonInteractive", "-Command", &script].map(OsStr::new);
        let output = windows_command::run(Tool::PowerShell, &args, Duration::from_secs(60)).unwrap().checked().unwrap();
        assert_eq!(String::from_utf8(output.stdout).unwrap(), "mutation;");
        let action = change_script("00FF11223344", "001122334455", "4AE293973B07");
        let script = format!("{mock}\n{READ}\n{action}");
        let args = ["-NoProfile", "-NonInteractive", "-Command", &script].map(OsStr::new);
        let output = windows_command::run(Tool::PowerShell, &args, Duration::from_secs(60)).unwrap();
        assert_eq!(output.code, 73);
        assert!(output.stdout.is_empty(), "conflict must not call the setter");
    }
}
