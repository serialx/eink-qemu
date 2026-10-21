#![cfg(target_os = "linux")]
use eink_bridge::linux_verify::FamilySnapshot;
use std::collections::{BTreeMap, HashMap};
use zbus::zvariant::{OwnedValue, Str, Value};
fn text(value: &str) -> OwnedValue { Str::from(value).into() }
fn properties() -> BTreeMap<String, OwnedValue> {
    BTreeMap::from([
        ("AddressData".into(), Value::from(vec![HashMap::from([("address", text("192.0.2.8")), ("prefix", 24u32.into())])]).try_into().unwrap()),
        ("RouteData".into(), Value::from(vec![HashMap::from([("dest", text("0.0.0.0")), ("prefix", 0u32.into()), ("next-hop", text("192.0.2.1")), ("metric", 100u32.into())])]).try_into().unwrap()),
        ("Gateway".into(), text("192.0.2.1")),
        ("Nameservers".into(), Value::from(vec![0x010200c0u32]).try_into().unwrap()),
        ("Domains".into(), Value::from(Vec::<String>::new()).try_into().unwrap()),
        ("Searches".into(), Value::from(vec!["example.test"]).try_into().unwrap()),
        ("DnsOptions".into(), Value::from(Vec::<String>::new()).try_into().unwrap()),
        ("DnsPriority".into(), 100i32.into()),
    ])
}
#[test]
fn live_host_validation_detects_address_route_and_dns_loss() {
    let before = FamilySnapshot::from_properties(properties()).unwrap();
    assert!(before.preserved_by(&before));
    for key in ["AddressData", "RouteData", "Nameservers", "Gateway", "Searches"] {
        let mut changed = properties();
        changed.remove(key);
        assert!(FamilySnapshot::from_properties(changed).is_err(), "missing {key} must not pass verification");
    }
    let mut changed = properties();
    changed.insert("Gateway".into(), text("192.0.2.254"));
    assert!(!before.preserved_by(&FamilySnapshot::from_properties(changed).unwrap()));
    let mut changed = properties();
    changed.insert("RouteData".into(), Value::from(vec![HashMap::from([("dest", text("0.0.0.0")), ("prefix", 0u32.into()), ("next-hop", text("192.0.2.1")), ("metric", 425u32.into())])]).try_into().unwrap());
    assert!(!before.preserved_by(&FamilySnapshot::from_properties(changed).unwrap()));
    let bytes = serde_json::to_vec(&before).unwrap();
    let restored: FamilySnapshot = serde_json::from_slice(&bytes).unwrap();
    assert!(before.preserved_by(&restored));
}

#[test]
fn conversion_pins_live_mac_mtu_route_and_dns_defaults() {
    use eink_bridge::{linux_profiles::{migrate, Profile}, linux_verify::HostSnapshot};
    let original = Profile(BTreeMap::from([
        ("connection".into(), BTreeMap::from([("type".into(), text("802-3-ethernet")), ("uuid".into(), text("9a21c334-74ba-4b38-9ebd-c6b6bcbbcc01"))])),
        ("ipv4".into(), BTreeMap::from([("method".into(), text("auto")), ("route-metric".into(), (-1i64).into()), ("dns-priority".into(), 0i32.into())])),
        ("ipv6".into(), BTreeMap::from([("method".into(), text("disabled"))])),
    ]));
    let snapshot = HostSnapshot { ipv4: Some(FamilySnapshot::from_properties(properties()).unwrap()), ipv6: None };
    let mut migration = migrate(&original, "enp0s1", "ebrtest0", "fd77321a-8520-4d05-8075-420b465d0846", "29457488-31f2-47e2-a46a-871e32b69271").unwrap();
    migration.preserve_runtime(&snapshot, [0x02, 1, 2, 3, 4, 5], 1400).unwrap();
    assert_eq!(migration.bridge["ipv4"]["route-metric"], 100i64.into());
    assert_eq!(migration.bridge["ipv4"]["dns-priority"], 100i32.into());
    assert_eq!(Vec::<u8>::try_from(migration.bridge["bridge"]["mac-address"].clone()).unwrap(), [0x02, 1, 2, 3, 4, 5]);
    for profile in [&migration.bridge, &migration.port] { assert_eq!(profile["802-3-ethernet"]["mtu"], 1400u32.into()); }
    assert_eq!(migration.original_disabled["ipv4"], original["ipv4"]);
    assert!(migration.preserve_runtime(&snapshot, [0; 6], 1400).is_err());
    assert!(migration.preserve_runtime(&snapshot, [1, 1, 2, 3, 4, 5], 1400).is_err());
}
