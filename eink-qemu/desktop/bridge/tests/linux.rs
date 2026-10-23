#![cfg(target_os = "linux")]
use eink_bridge::linux_profiles::{Profile, migrate, same_configuration};
use zbus::zvariant::{OwnedValue, Str, Value};
use std::collections::BTreeMap;

fn string(value: &str) -> OwnedValue { Str::from(value).into() }
fn profile(method: &str) -> Profile {
    BTreeMap::from([
        ("connection".into(), BTreeMap::from([
            ("id".into(), string("Wired connection")),
            ("uuid".into(), string("9a21c334-74ba-4b38-9ebd-c6b6bcbbcc01")),
            ("type".into(), string("802-3-ethernet")),
            ("autoconnect".into(), true.into()),
            ("autoconnect-priority".into(), 10i32.into()),
            ("zone".into(), string("trusted")),
        ])),
        ("802-3-ethernet".into(), BTreeMap::from([("mtu".into(), 1400u32.into())])),
        ("ipv4".into(), BTreeMap::from([("method".into(), string(method)), ("dns".into(), Value::from(vec![0x01010101u32]).try_into().unwrap())])),
        ("ipv6".into(), BTreeMap::from([("method".into(), string("auto")), ("addr-gen-mode".into(), 0i32.into()), ("route-metric".into(), 77i64.into())])),
    ]).into()
}
fn converted(profile: &Profile) -> eink_bridge::linux_profiles::Migration {
    migrate(profile, "enp0s1", "ebr1234567890", "fd77321a-8520-4d05-8075-420b465d0846", "29457488-31f2-47e2-a46a-871e32b69271").unwrap()
}
#[test]
fn conversion_preserves_addressing_routes_dns_mtu_and_autoconnect() {
    for method in ["auto", "manual"] {
        let mut original = profile(method);
        let address = std::collections::HashMap::from([("address", string("192.0.2.20")), ("prefix", 24u32.into())]);
        original.get_mut("ipv4").unwrap().insert("address-data".into(), Value::from(vec![address]).try_into().unwrap());
        let route = std::collections::HashMap::from([("dest", string("2001:db8:1::")), ("prefix", 64u32.into()), ("next-hop", string("2001:db8::1"))]);
        original.get_mut("ipv6").unwrap().insert("route-data".into(), Value::from(vec![route]).try_into().unwrap());
        let migrated = converted(&original);
        assert_eq!(migrated.bridge["ipv4"], original["ipv4"]);
        assert_eq!(migrated.bridge["ipv6"], original["ipv6"]);
        assert_eq!(migrated.bridge["802-3-ethernet"]["mtu"], 1400u32.into());
        assert_eq!(migrated.port["802-3-ethernet"], original["802-3-ethernet"]);
        assert_eq!(migrated.bridge["connection"]["autoconnect-priority"], 10i32.into());
        assert_eq!(migrated.bridge["connection"]["zone"], string("trusted"));
        assert_eq!(migrated.original_disabled["connection"]["autoconnect"], false.into());
        assert!(!migrated.port.contains_key("ipv4") && !migrated.port.contains_key("ipv6"));
        assert_eq!(migrated.port["connection"]["master"], migrated.bridge["connection"]["uuid"]);
        let encoded = serde_json::to_vec(&migrated).unwrap();
        let restored: eink_bridge::linux_profiles::Migration = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(restored, migrated, "journal must preserve every D-Bus variant signature");
    }
}
#[test]
fn unsupported_profiles_are_rejected_before_planning_mutations() {
    let mut unmanaged = profile("auto");
    unmanaged.get_mut("ipv6").unwrap().insert("method".into(), string("ignore"));
    assert!(migrate(&unmanaged, "enp0s1", "ebr1234567890", "fd77321a-8520-4d05-8075-420b465d0846", "29457488-31f2-47e2-a46a-871e32b69271").is_err());
    for kind in ["802-11-wireless", "bond", "vlan", "bridge", "vpn"] {
        let mut value = profile("auto"); value.get_mut("connection").unwrap().insert("type".into(), string(kind));
        assert!(migrate(&value, "enp0s1", "ebr1234567890", "fd77321a-8520-4d05-8075-420b465d0846", "29457488-31f2-47e2-a46a-871e32b69271").is_err());
    }
    for section in ["802-1x", "vlan", "bond", "wireguard", "unknown-setting"] {
        let mut value = profile("auto"); value.insert(section.into(), BTreeMap::new());
        assert!(migrate(&value, "enp0s1", "ebr1234567890", "fd77321a-8520-4d05-8075-420b465d0846", "29457488-31f2-47e2-a46a-871e32b69271").is_err());
    }
    for key in ["master", "controller", "slave-type", "port-type"] {
        let mut value = profile("auto"); value.get_mut("connection").unwrap().insert(key.into(), string("bridge"));
        assert!(migrate(&value, "enp0s1", "ebr1234567890", "fd77321a-8520-4d05-8075-420b465d0846", "29457488-31f2-47e2-a46a-871e32b69271").is_err());
    }
}

#[test]
fn interface_dependent_ipv6_identity_is_refused_before_conversion() {
    for mode in [None, Some(1i32), Some(2i32), Some(3i32)] {
        let mut original = profile("auto");
        let ipv6 = original.get_mut("ipv6").unwrap();
        match mode {
            Some(mode) => { ipv6.insert("addr-gen-mode".into(), mode.into()); },
            None => { ipv6.remove("addr-gen-mode"); },
        }
        let before = original.clone();
        let error = migrate(&original, "enp0s1", "ebr1234567890", "fd77321a-8520-4d05-8075-420b465d0846", "29457488-31f2-47e2-a46a-871e32b69271").unwrap_err();
        assert_eq!(error.code, "EUNSUPPORTED", "mode {mode:?}");
        assert_eq!(original, before);
        original.get_mut("ipv6").unwrap().insert("method".into(), string("disabled"));
        converted(&original); // An unused address generator cannot change addresses.
    }
    let mut portable = profile("auto");
    portable.get_mut("ipv6").unwrap().insert("addr-gen-mode".into(), 0i32.into());
    converted(&portable);
}
#[test]
fn user_edits_are_conflicts_but_network_manager_timestamp_is_not() {
    let original = profile("auto"); let mut current = original.clone();
    current.get_mut("connection").unwrap().insert("timestamp".into(), 123456u64.into());
    assert!(same_configuration(&original, &current));
    current.get_mut("ipv4").unwrap().insert("route-metric".into(), 18i64.into());
    assert!(!same_configuration(&original, &current));
}

#[test]
fn network_manager_bridge_defaults_do_not_hide_user_edits() {
    let migration = converted(&profile("auto"));
    let mut saved_port = migration.port.clone();
    saved_port.insert("bridge-port".into(), BTreeMap::from([("vlans".into(), Value::from(Vec::<std::collections::HashMap<&str, OwnedValue>>::new()).try_into().unwrap())]));
    assert!(same_configuration(&migration.port, &saved_port));
    saved_port.get_mut("bridge-port").unwrap().insert("priority".into(), 12u32.into());
    assert!(!same_configuration(&migration.port, &saved_port));
    let mut saved = migration.bridge.clone();
    let bridge = saved.get_mut("bridge").unwrap();
    bridge.insert("interface-name".into(), string("ebr1234567890"));
    bridge.insert("vlans".into(), Value::from(Vec::<std::collections::HashMap<&str, OwnedValue>>::new()).try_into().unwrap());
    assert!(same_configuration(&migration.bridge, &saved));
    saved.get_mut("bridge").unwrap().insert("interface-name".into(), string("someone-else"));
    assert!(!same_configuration(&migration.bridge, &saved));
    saved.get_mut("bridge").unwrap().insert("interface-name".into(), string("ebr1234567890"));
    saved.get_mut("bridge").unwrap().insert("vlans".into(), Value::from(vec![std::collections::HashMap::from([("vid-start", OwnedValue::from(10u16)), ("vid-end", OwnedValue::from(20u16))])]).try_into().unwrap());
    assert!(!same_configuration(&migration.bridge, &saved));
}

#[test]
fn wired_default_normalization_keeps_mac_and_nondefault_edits_visible() {
    let mut expected = converted(&profile("auto")).bridge;
    expected.get_mut("802-3-ethernet").unwrap().insert("cloned-mac-address".into(), Value::from(vec![2u8, 1, 2, 3, 4, 5]).try_into().unwrap());
    let mut actual = expected.clone();
    let wired = actual.get_mut("802-3-ethernet").unwrap();
    wired.insert("assigned-mac-address".into(), string("02:01:02:03:04:05"));
    wired.insert("auto-negotiate".into(), false.into());
    wired.insert("mac-address-blacklist".into(), Value::from(Vec::<String>::new()).try_into().unwrap());
    wired.insert("s390-options".into(), Value::from(std::collections::HashMap::<String, String>::new()).try_into().unwrap());
    assert!(same_configuration(&expected, &actual));
    for (key, value) in [("assigned-mac-address", string("02:01:02:03:04:06")), ("auto-negotiate", true.into()),
        ("mac-address-blacklist", Value::from(vec!["02:01:02:03:04:07"]).try_into().unwrap()),
        ("s390-options", Value::from(std::collections::HashMap::from([("layer2", "1")])).try_into().unwrap())] {
        let mut changed = actual.clone(); changed.get_mut("802-3-ethernet").unwrap().insert(key.into(), value);
        assert!(!same_configuration(&expected, &changed), "{key} edit must remain a conflict");
    }
}
