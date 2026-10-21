#![cfg(target_os = "linux")]
use eink_bridge::{linux_profiles::Profile, linux_transaction::{ProfileChange, ProfileAction}};
use std::collections::BTreeMap;
use zbus::zvariant::{OwnedValue, Str};
fn profile(id: &str, autoconnect: bool) -> Profile {
    Profile(BTreeMap::from([("connection".into(), BTreeMap::from([
        ("uuid".into(), OwnedValue::from(Str::from(id))), ("autoconnect".into(), autoconnect.into()),
    ]))]))
}
#[test]
fn profile_recovery_handles_each_partial_state_and_preserves_external_edits() {
    let before = profile("8b7a2dfd-bbcb-4533-bc07-f4438419a001", true);
    let after = profile("8b7a2dfd-bbcb-4533-bc07-f4438419a001", false);
    let change = ProfileChange::new(Some(before.clone()), Some(after.clone())).unwrap();
    assert_eq!(change.action(Some(&before)).unwrap(), ProfileAction::Update);
    assert_eq!(change.action(Some(&after)).unwrap(), ProfileAction::Unchanged);
    assert_eq!(change.reverse().action(Some(&after)).unwrap(), ProfileAction::Update);
    assert_eq!(change.reverse().action(Some(&before)).unwrap(), ProfileAction::Unchanged);
    assert!(change.reverse().action(None).is_err(), "external deletion must not be overwritten");
    let mut edited = after.clone();
    edited.get_mut("connection").unwrap().insert("autoconnect-priority".into(), 42i32.into());
    assert!(change.reverse().action(Some(&edited)).is_err());
    let create = ProfileChange::new(None, Some(after.clone())).unwrap();
    assert_eq!(create.action(None).unwrap(), ProfileAction::Create);
    assert_eq!(create.reverse().action(Some(&after)).unwrap(), ProfileAction::Delete);
    assert_eq!(create.reverse().action(None).unwrap(), ProfileAction::Unchanged);
    assert!(create.reverse().action(Some(&edited)).is_err());
    let roundtrip: ProfileChange = serde_json::from_slice(&serde_json::to_vec(&change).unwrap()).unwrap();
    assert_eq!(roundtrip.reverse().action(Some(&after)).unwrap(), ProfileAction::Update);
    assert!(ProfileChange::new(Some(before), Some(profile("8b7a2dfd-bbcb-4533-bc07-f4438419a002", false))).is_err());
}

#[test]
fn expired_checkpoint_paths_are_not_reused_across_service_or_host_restarts() {
    use eink_bridge::linux_transaction::Checkpoint;
    let checkpoint = Checkpoint::record("boot-a::1.4".into(), "/org/freedesktop/NetworkManager/Checkpoint/1".into()).unwrap();
    assert!(checkpoint.belongs_to("boot-a::1.4"));
    assert!(!checkpoint.belongs_to("boot-a::1.8"));
    assert!(!checkpoint.belongs_to("boot-b::1.4"));
    assert!(Checkpoint::record("boot-a::1.4".into(), "/org/freedesktop/NetworkManager/Devices/1".into()).is_err());
}

#[test]
fn managed_network_rejects_ambiguous_restoration_identity() {
    use eink_bridge::linux_network::NetworkIdentity;
    let id = NetworkIdentity::new("enp0s1", "ebrtest0", "8b7a2dfd-bbcb-4533-bc07-f4438419a001", "8b7a2dfd-bbcb-4533-bc07-f4438419a002").unwrap();
    assert_eq!(id.bridge(), "ebrtest0");
    assert!(NetworkIdentity::new("../etc", "ebrtest0", id.bridge_uuid(), id.port_uuid()).is_err());
    assert!(NetworkIdentity::new("enp0s1", "enp0s1", id.bridge_uuid(), id.port_uuid()).is_err());
    assert!(NetworkIdentity::new("enp0s1", "ebrtest0", id.bridge_uuid(), id.bridge_uuid()).is_err());
    let restored: NetworkIdentity = serde_json::from_slice(&serde_json::to_vec(&id).unwrap()).unwrap();
    assert_eq!(restored, id);
}
