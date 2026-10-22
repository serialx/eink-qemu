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
        let output = windows_command::run(Tool::PowerShell, &arguments, Duration::from_secs(120))
            .and_then(|output| output.checked()).map_err(|error| error.context("Read Windows bridge membership"))?;
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
    /// Resolve a bridge after an interrupted TAP-first create. A new bridge's
    /// GUID alone does not prove ownership. Require the dedicated resource's
    /// exact TAP to be a member, with at most the recorded physical adapter.
    /// Never use this to adopt a physical-only or otherwise unrelated bridge.
    pub fn owned_bridge(&self, resource: &crate::model::ResourceId, tap: &DeviceIdentity,
        physical: &DeviceIdentity) -> Result<Option<DeviceIdentity>, BridgeError> {
        if !tap.is_tap() || tap.instance_id != crate::windows_driver::instance_id(resource)
            || physical.is_bridge() || physical.is_tap() || physical.guid == tap.guid
            || self.device(&tap.guid)? != tap || self.device(&physical.guid)? != physical { return Err(conflict()); }
        let bridges = self.bridges();
        let members = self.members()?;
        if bridges.is_empty() && members.is_empty() { return Ok(None); }
        if bridges.len() != 1 || self.bindings(&bridges[0].guid)?.get("ms_bridge") != Some(&true)
            || !members.contains(&tap.guid) || members.iter().any(|id| id != &tap.guid && id != &physical.guid) {
            return Err(conflict());
        }
        Ok(Some(bridges[0].clone()))
    }
    /// During isolated preparation, both TAP identities are journaled before
    /// bridge creation. The permanent TAP must establish ownership even when
    /// native creation attaches only one of the requested adapters.
    pub fn owned_prepared_bridge(&self, resource: &crate::model::ResourceId, tap: &DeviceIdentity,
        bootstrap_resource: &crate::model::ResourceId, bootstrap: &DeviceIdentity,
        physical: &DeviceIdentity) -> Result<Option<DeviceIdentity>, BridgeError> {
        if resource == bootstrap_resource || tap.guid == bootstrap.guid
            || !tap.is_tap() || tap.instance_id != crate::windows_driver::instance_id(resource)
            || !bootstrap.is_tap() || bootstrap.instance_id != crate::windows_driver::instance_id(bootstrap_resource)
            || physical.is_bridge() || physical.is_tap() || physical.guid == tap.guid || physical.guid == bootstrap.guid
            || self.device(&tap.guid)? != tap || self.device(&bootstrap.guid)? != bootstrap
            || self.device(&physical.guid)? != physical { return Err(conflict()); }
        let bridges = self.bridges();
        let members = self.members()?;
        if bridges.is_empty() && members.is_empty() { return Ok(None); }
        if bridges.len() != 1 || self.bindings(&bridges[0].guid)?.get("ms_bridge") != Some(&true)
            || !members.contains(&tap.guid)
            || members.iter().any(|id| id != &tap.guid && id != &bootstrap.guid && id != &physical.guid) {
            return Err(conflict());
        }
        Ok(Some(bridges[0].clone()))
    }
    /// ms_implat is the member-side binding. ms_bridge belongs to the bridge
    /// virtual adapter itself. Neither display labels nor netsh's exit code
    /// can establish that both requested members were attached.
    pub fn require_members(&self, bridge: &DeviceIdentity, members: &[DeviceIdentity]) -> Result<(), BridgeError> {
        if members.len() != 2 { return Err(conflict()); }
        self.require_transition_members(bridge, members)
    }
    /// Exact intermediate membership, including a one-TAP bridge while removal
    /// awaits confirmation or three members while replacing the bootstrap TAP.
    /// Callers must establish ownership from the protected transaction first.
    pub fn require_transition_members(&self, bridge: &DeviceIdentity, members: &[DeviceIdentity]) -> Result<(), BridgeError> {
        if !(1..=3).contains(&members.len()) { return Err(conflict()); }
        if !bridge.is_bridge() || self.bridges() != vec![bridge] || self.device(&bridge.guid)? != bridge
            || self.bindings(&bridge.guid)?.get("ms_bridge") != Some(&true) { return Err(conflict()); }
        let mut expected = BTreeSet::new();
        for member in members {
            if member.is_bridge() || self.device(&member.guid)? != member || !expected.insert(member.guid.clone()) { return Err(conflict()); }
        }
        if self.members()? != expected { return Err(conflict()); }
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
    fn interrupted_creation_requires_the_dedicated_tap_membership() {
        let resource = crate::model::ResourceId::new("12".repeat(16)).unwrap();
        let physical = device(0, false);
        let tap = DeviceIdentity { instance_id: crate::windows_driver::instance_id(&resource),
            hardware_ids: vec!["TAP0901".into()], service: "TAP0901".into(), ..device(1, false) };
        let devices = [physical.clone(), tap.clone(), device(2, true), device(3, false), device(4, true)];
        assert_eq!(topology(&devices[..2], &[]).owned_bridge(&resource, &tap, &physical).unwrap(), None);
        for members in [&[1][..], &[0,1][..]] {
            assert_eq!(topology(&devices[..3], members).owned_bridge(&resource, &tap, &physical).unwrap(), Some(devices[2].clone()));
        }
        for native in [topology(&devices[..3], &[]), topology(&devices[..3], &[0]),
            topology(&devices[..4], &[0,1,3]), topology(&devices, &[0,1])] {
            assert_eq!(native.owned_bridge(&resource, &tap, &physical).unwrap_err().code, "ECONFLICT");
        }
        let native = topology(&devices[..3], &[0,1]);
        let wrong_resource = crate::model::ResourceId::new("34".repeat(16)).unwrap();
        assert!(native.owned_bridge(&wrong_resource, &tap, &physical).is_err());
        let replacement = DeviceIdentity { instance_id: "REPLACEMENT".into(), ..physical.clone() };
        assert!(native.owned_bridge(&resource, &tap, &replacement).is_err());
    }
    #[test]
    fn isolated_preparation_requires_both_owned_identities_and_exact_transitions() {
        let resource = crate::model::ResourceId::new("12".repeat(16)).unwrap();
        let bootstrap_resource = crate::model::ResourceId::new("34".repeat(16)).unwrap();
        let physical = device(0, false);
        let tap = DeviceIdentity { instance_id: crate::windows_driver::instance_id(&resource),
            hardware_ids: vec!["TAP0901".into()], service: "TAP0901".into(), ..device(1, false) };
        let bootstrap = DeviceIdentity { instance_id: crate::windows_driver::instance_id(&bootstrap_resource),
            ..DeviceIdentity { guid: device(3, false).guid, ..tap.clone() } };
        let bridge = device(2, true);
        let devices = [physical.clone(), tap.clone(), bridge.clone(), bootstrap.clone(), device(4, false)];
        for indices in [&[1][..], &[1,3][..], &[0,1,3][..], &[0,1][..]] {
            let native = topology(&devices, indices);
            assert_eq!(native.owned_prepared_bridge(&resource, &tap, &bootstrap_resource, &bootstrap, &physical).unwrap(), Some(bridge.clone()));
            let expected = indices.iter().map(|index| devices[*index as usize].clone()).collect::<Vec<_>>();
            native.require_transition_members(&bridge, &expected).unwrap();
            if indices.len() != 2 { assert!(native.require_members(&bridge, &expected).is_err()); }
        }
        for indices in [&[0][..], &[3][..], &[1,4][..], &[0,1,3,4][..]] {
            assert!(topology(&devices, indices).owned_prepared_bridge(&resource, &tap, &bootstrap_resource, &bootstrap, &physical).is_err());
        }
        let native = topology(&devices, &[1,3]);
        assert!(native.owned_prepared_bridge(&resource, &tap, &resource, &bootstrap, &physical).is_err());
        let replacement = DeviceIdentity { instance_id: "REPLACEMENT".into(), ..bootstrap.clone() };
        assert!(native.owned_prepared_bridge(&resource, &tap, &bootstrap_resource, &replacement, &physical).is_err());
        assert!(native.require_transition_members(&bridge, &[tap.clone(), tap]).is_err());
        assert!(native.require_transition_members(&bridge, &[]).is_err());
    }
    #[test]
    fn native_read_only_topology_uses_machine_identities() {
        let native=Topology::read().unwrap();
        assert!(!native.devices.is_empty());
        let _=native.members().unwrap();
    }
}
