//! Membership changes use exact device identities and native binding checks.
//! The coordinator must journal intent before calling these functions. Neither
//! localized command output nor a zero exit status proves the requested state.
use crate::{model::{BridgeError, ResourceId}, windows_command::{self, Tool},
    windows_device::{self, DeviceIdentity}, windows_topology::Topology};
use std::{ffi::OsStr, time::{Duration, Instant}};

fn conflict() -> BridgeError { BridgeError::new("ECONFLICT", "Windows bridge membership changed. Review the connection before continuing.") }
fn command(arguments: &[&str]) -> Result<(), BridgeError> {
    windows_command::run(Tool::Netsh, &arguments.iter().map(OsStr::new).collect::<Vec<_>>(),
        Duration::from_secs(120))?.checked().map(|_| ())
}
fn members_with(before: &[DeviceIdentity], member: &DeviceIdentity, present: bool) -> Result<Vec<DeviceIdentity>, BridgeError> {
    if !(1..=3).contains(&before.len()) || member.is_bridge() { return Err(conflict()); }
    let mut after = before.to_vec();
    if let Some(existing) = after.iter().find(|item| item.guid == member.guid) {
        if existing != member { return Err(conflict()); }
    }
    if present {
        if !after.contains(member) { after.push(member.clone()); }
    } else { after.retain(|item| item.guid != member.guid); }
    if !(1..=3).contains(&after.len()) { return Err(conflict()); }
    Ok(after)
}
/// Attach or detach one recorded member. Accept the exact completed state on
/// replay; any other membership is a conflict. Never detach the last member.
pub fn set_member(bridge: &DeviceIdentity, before: &[DeviceIdentity], member: &DeviceIdentity,
    present: bool) -> Result<(), BridgeError> {
    let bridge_guid = windows_device::guid(&bridge.guid)?;
    let member_guid = windows_device::guid(&member.guid)?;
    let after = members_with(before, member, present)?;
    let topology = Topology::read()?;
    if topology.device(&member.guid)? != member { return Err(conflict()); }
    if topology.require_transition_members(bridge, &after).is_ok() { return Ok(()); }
    topology.require_transition_members(bridge, before)?;
    let result = if present { command(&["bridge", "add", &member_guid, "to", &bridge_guid]) }
        else { command(&["bridge", "remove", &member_guid, "from", &bridge_guid]) };
    // A command can report an error after changing the binding. Verify once
    // before reporting it; do not blindly repeat a potentially partial change.
    let deadline = Instant::now() + Duration::from_secs(45);
    loop {
        let topology = Topology::read()?;
        if topology.device(&member.guid)? != member { return Err(conflict()); }
        if topology.require_transition_members(bridge, &after).is_ok() { return Ok(()); }
        topology.require_transition_members(bridge, before)?;
        if result.is_err() || Instant::now() >= deadline {
            return result.and(Err(BridgeError::new("EHOSTNETWORK", "The bridge membership change did not become ready.")));
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}
/// Create using two dedicated TAPs while Ethernet remains untouched. Return
/// the bridge even if Windows attaches only the first TAP; the coordinator
/// records its identity before completing membership with `set_member`.
pub fn create_isolated(resource: &ResourceId, tap: &DeviceIdentity,
    bootstrap_resource: &ResourceId, bootstrap: &DeviceIdentity,
    physical: &DeviceIdentity) -> Result<DeviceIdentity, BridgeError> {
    let topology = Topology::read()?;
    if let Some(bridge) = topology.owned_prepared_bridge(resource, tap, bootstrap_resource, bootstrap, physical)? {
        if topology.require_transition_members(&bridge, &[tap.clone()]).is_err() {
            topology.require_members(&bridge, &[tap.clone(), bootstrap.clone()])?;
        }
        return Ok(bridge);
    }
    for device in [tap, bootstrap, physical] { topology.require_unbridged(device)?; }
    let tap_guid = windows_device::guid(&tap.guid)?;
    let bootstrap_guid = windows_device::guid(&bootstrap.guid)?;
    let result = command(&["bridge", "create", &tap_guid, &bootstrap_guid]);
    let deadline = Instant::now() + Duration::from_secs(45);
    loop {
        let topology = Topology::read()?;
        if let Some(bridge) = topology.owned_prepared_bridge(resource, tap, bootstrap_resource, bootstrap, physical)? {
            if topology.require_transition_members(&bridge, &[tap.clone()]).is_err() {
                topology.require_members(&bridge, &[tap.clone(), bootstrap.clone()])?;
            }
            return Ok(bridge);
        }
        if result.is_err() || Instant::now() >= deadline {
            return result.and(Err(BridgeError::new("EHOSTNETWORK", "The new Windows bridge did not become ready.")));
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}
/// Final cleanup after confirmation, or rollback of a newly created bridge.
/// Its recorded members must all still exist; this never removes another
/// bridge that happens to use the same display name.
pub fn destroy(bridge: &DeviceIdentity, members: &[DeviceIdentity]) -> Result<(), BridgeError> {
    if !bridge.is_bridge() || !(1..=3).contains(&members.len()) { return Err(conflict()); }
    let guid = windows_device::guid(&bridge.guid)?;
    let absent = |topology: &Topology| -> Result<bool, BridgeError> {
        if !topology.bridges().is_empty() { return Ok(false); }
        for member in members { topology.require_unbridged(member)?; }
        Ok(true)
    };
    let topology = Topology::read()?;
    if absent(&topology)? { return Ok(()); }
    topology.require_transition_members(bridge, members)?;
    let result = command(&["bridge", "destroy", &guid]);
    let deadline = Instant::now() + Duration::from_secs(45);
    loop {
        let topology = Topology::read()?;
        if absent(&topology)? { return Ok(()); }
        topology.require_transition_members(bridge, members)?;
        if result.is_err() || Instant::now() >= deadline {
            return result.and(Err(BridgeError::new("EHOSTNETWORK", "The Windows bridge could not be removed completely.")));
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}
