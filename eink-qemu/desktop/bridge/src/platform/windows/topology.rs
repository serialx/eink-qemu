//! Native bridge membership, independent of localized netsh output. The
//! supported conversion owns the only Windows bridge; multiple bridges or
//! extra multiplexor members are conflicts, never inferred ownership.
use crate::{model::BridgeError, windows_command::{self, Tool}, windows_device::{self, DeviceIdentity}};
use serde::Deserialize;
use std::{collections::{BTreeMap, BTreeSet}, ffi::OsStr, time::Duration};

const INSPECT: &str = r#"
$ErrorActionPreference='Stop'
[Console]::OutputEncoding=New-Object System.Text.UTF8Encoding($false)
$bindings=@(Get-CimInstance -Namespace root/StandardCimv2 -ClassName MSFT_NetAdapterBindingSettingData | Select-Object InstanceID,ComponentID,Enabled)
[Console]::Out.Write((ConvertTo-Json -InputObject $bindings -Depth 4 -Compress))
"#;
fn conflict() -> BridgeError { BridgeError::new("ECONFLICT", "Windows bridge membership changed. Review the network configuration before continuing.") }
fn invalid() -> BridgeError { BridgeError::new("EDISCOVERY", "Windows returned incomplete network binding identities.") }
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Binding {
    #[serde(rename = "InstanceID")] instance: String,
    #[serde(rename = "ComponentID")] component: String,
    #[serde(rename = "Enabled")] enabled: bool,
}
#[derive(Debug, Clone)]
pub struct Topology {
    devices: BTreeMap<String, DeviceIdentity>,
    bindings: BTreeMap<String, BTreeMap<String, bool>>,
}
impl Topology {
    fn parse(devices: BTreeMap<String, DeviceIdentity>, json: &[u8]) -> Result<Self, BridgeError> {
        let rows: Vec<Binding> = serde_json::from_slice(json).map_err(|_| invalid())?;
        if rows.len() > 16384 { return Err(invalid()); }
        let mut bindings: BTreeMap<String, BTreeMap<String, bool>> = BTreeMap::new();
        for row in rows {
            let (id, component) = row.instance.split_once("::").ok_or_else(invalid)?;
            let id = windows_device::guid(id).map_err(|_| invalid())?;
            if component != row.component || component.is_empty() || component.len() > 256
                || component.chars().any(char::is_control) { return Err(invalid()); }
            if bindings.entry(id).or_default().insert(component.to_ascii_lowercase(), row.enabled).is_some() { return Err(invalid()); }
        }
        Ok(Self { devices, bindings })
    }
    pub fn read() -> Result<Self, BridgeError> {
        let before = windows_device::enumerate()?;
        let arguments = ["-NoProfile", "-NonInteractive", "-Command", INSPECT].map(OsStr::new);
        let output = windows_command::run(Tool::PowerShell, &arguments, Duration::from_secs(120))?.checked()?;
        let after = windows_device::enumerate()?;
        if before != after { return Err(conflict()); }
        Self::parse(after, &output.stdout)
    }
    pub fn device(&self, id: &str) -> Result<&DeviceIdentity, BridgeError> {
        self.devices.get(&windows_device::guid(id)?).ok_or_else(conflict)
    }
    pub fn bindings(&self, id: &str) -> Result<&BTreeMap<String, bool>, BridgeError> {
        self.bindings.get(&windows_device::guid(id)?).filter(|bindings| bindings.contains_key("ms_implat")).ok_or_else(invalid)
    }
    pub fn bridges(&self) -> Vec<&DeviceIdentity> { self.devices.values().filter(|device| device.is_bridge()).collect() }
    fn members(&self) -> Result<BTreeSet<String>, BridgeError> {
        self.bindings.iter().filter(|(_, bindings)| bindings.get("ms_implat") == Some(&true)).map(|(id, _)| {
            self.devices.get(id).filter(|device| !device.is_bridge()).ok_or_else(conflict)?;
            Ok(id.clone())
        }).collect()
    }
    /// Conversion does not adopt an existing bridge, team, or VPN membership.
    pub fn require_unbridged(&self, adapter: &DeviceIdentity) -> Result<(), BridgeError> {
        if self.device(&adapter.guid)? != adapter || self.bindings(&adapter.guid)?.get("ms_implat") != Some(&false)
            || !self.bridges().is_empty() || !self.members()?.is_empty() { return Err(conflict()); }
        Ok(())
    }
    /// ms_implat is the member-side binding. ms_bridge belongs to the bridge
    /// virtual adapter itself. Neither display labels nor netsh's exit code
    /// can establish that both requested members were attached.
    pub fn require_members(&self, bridge: &DeviceIdentity, members: &[DeviceIdentity]) -> Result<(), BridgeError> {
        if !bridge.is_bridge() || self.bridges() != vec![bridge] || self.device(&bridge.guid)? != bridge
            || self.bindings(&bridge.guid)?.get("ms_bridge") != Some(&true) { return Err(conflict()); }
        let mut expected = BTreeSet::new();
        for member in members {
            if member.is_bridge() || self.device(&member.guid)? != member || !expected.insert(member.guid.clone()) { return Err(conflict()); }
        }
        if expected.len() != 2 || self.members()? != expected { return Err(conflict()); }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn device(index: u8, bridge: bool) -> DeviceIdentity {
        DeviceIdentity { guid: format!("{{00000000-0000-0000-0000-{index:012}}}"), instance_id: format!("DEVICE\\{index}"),
            hardware_ids: vec![if bridge { r"COMPOSITEBUS\MS_IMPLAT_MP" } else { "ETHERNET" }.into()], service: "TEST".into() }
    }
    fn topology(devices: &[DeviceIdentity], members: &[u8]) -> Topology {
        let rows: Vec<_> = devices.iter().enumerate().flat_map(|(index, device)| {
            [("ms_implat", members.contains(&(index as u8))), ("ms_bridge", device.is_bridge())].map(|(component, enabled)|
                serde_json::json!({ "InstanceID": format!("{}::{component}",device.guid), "ComponentID": component, "Enabled": enabled }))
        }).collect();
        Topology::parse(devices.iter().map(|d| (d.guid.clone(),d.clone())).collect(), &serde_json::to_vec(&rows).unwrap()).unwrap()
    }
    #[test]
    fn partial_creation_extra_member_and_multiple_bridges_are_conflicts() {
        let devices = [device(0,false),device(1,false),device(2,true),device(3,false),device(4,true)];
        let members = &devices[..2];
        assert!(topology(&devices[..3], &[0,1]).require_members(&devices[2], members).is_ok());
        for native in [topology(&devices[..3], &[0]), topology(&devices[..4], &[0,1,3]), topology(&devices, &[0,1])] {
            assert_eq!(native.require_members(&devices[2], members).unwrap_err().code,"ECONFLICT");
        }
        assert!(topology(&devices[..2], &[]).require_unbridged(&devices[0]).is_ok());
        assert!(topology(&devices[..3], &[0,1]).require_unbridged(&devices[0]).is_err());
    }
    #[test]
    fn malformed_and_duplicate_binding_identities_are_rejected() {
        for json in [r#"[{"InstanceID":"Ethernet::ms_implat","ComponentID":"ms_implat","Enabled":true}]"#,
            r#"[{"InstanceID":"{00000000-0000-0000-0000-000000000000}::ms_implat","ComponentID":"ms_bridge","Enabled":true}]"#] {
            assert!(Topology::parse(BTreeMap::new(),json.as_bytes()).is_err());
        }
        let row = serde_json::json!({"InstanceID":format!("{}::ms_implat",device(0,false).guid),"ComponentID":"ms_implat","Enabled":true});
        assert!(Topology::parse(BTreeMap::new(),&serde_json::to_vec(&vec![row.clone(),row]).unwrap()).is_err());
        // An unknown enabled member cannot silently disappear from inspection.
        let mut native=topology(&[device(0,false),device(1,false),device(2,true)], &[0,1]);
        native.devices.remove(&device(1,false).guid);
        assert!(native.members().is_err());
    }
    #[test]
    fn native_read_only_topology_uses_machine_identities() {
        let native=Topology::read().unwrap();
        assert!(!native.devices.is_empty());
        let _=native.members().unwrap();
    }
}
