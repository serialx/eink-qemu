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
fn committed_dynamic_addresses_can_renew_but_setup_and_static_checks_stay_exact() {
    use eink_bridge::{linux_profiles::Profile, linux_verify::HostSnapshot};
    let before = HostSnapshot { ipv4: Some(FamilySnapshot::from_properties(properties()).unwrap()), ipv6: None };
    let mut renewed = properties();
    renewed.insert("AddressData".into(), Value::from(vec![HashMap::from([("address", text("192.0.2.99")), ("prefix", 24u32.into())])]).try_into().unwrap());
    let current = HostSnapshot { ipv4: Some(FamilySnapshot::from_properties(renewed).unwrap()), ipv6: None };
    let mut profile = Profile(BTreeMap::from([
        ("ipv4".into(), BTreeMap::from([("method".into(), text("auto"))])),
        ("ipv6".into(), BTreeMap::from([("method".into(), text("disabled"))])),
    ]));
    assert!(before.verify_preserved(&current).is_err());
    assert!(before.verify_committed(&current, &profile).is_ok());
    assert!(before.verify_committed(&HostSnapshot { ipv4: None, ipv6: None }, &profile).is_err());
    let mut empty = properties();
    empty.insert("AddressData".into(), Value::from(Vec::<HashMap<String, OwnedValue>>::new()).try_into().unwrap());
    assert!(before.verify_committed(&HostSnapshot { ipv4: Some(FamilySnapshot::from_properties(empty).unwrap()), ipv6: None }, &profile).is_err());
    profile.get_mut("ipv4").unwrap().insert("method".into(), text("manual"));
    assert!(before.verify_committed(&current, &profile).is_err());
}

fn dynamic_snapshot(properties: BTreeMap<String, OwnedValue>, ipv6: bool) -> (eink_bridge::linux_verify::HostSnapshot, eink_bridge::linux_profiles::Profile) {
    use eink_bridge::{linux_profiles::Profile, linux_verify::HostSnapshot};
    let family = Some(FamilySnapshot::from_properties(properties).unwrap());
    let snapshot = if ipv6 { HostSnapshot { ipv4: None, ipv6: family } }
        else { HostSnapshot { ipv4: family, ipv6: None } };
    (snapshot, Profile(BTreeMap::from([
        ("ipv4".into(), BTreeMap::from([("method".into(), text(if ipv6 { "disabled" } else { "auto" }))])),
        ("ipv6".into(), BTreeMap::from([("method".into(), text(if ipv6 { "auto" } else { "disabled" }))])),
    ])))
}
fn dynamic_properties(ipv6: bool, address: &str, gateway: &str, dns: &str) -> BTreeMap<String, OwnedValue> {
    let mut fields = properties();
    fields.insert("AddressData".into(), Value::from(vec![HashMap::from([
        ("address", text(address)), ("prefix", (if ipv6 { 64u32 } else { 24u32 }).into()),
    ])]).try_into().unwrap());
    fields.insert("RouteData".into(), Value::from(vec![HashMap::from([
        ("dest", text(if ipv6 { "::" } else { "0.0.0.0" })), ("prefix", 0u32.into()),
        ("next-hop", text(gateway)), ("metric", 100u32.into()),
    ])]).try_into().unwrap());
    fields.insert("Gateway".into(), text(gateway));
    fields.insert("Nameservers".into(), if ipv6 {
        Value::from(vec![dns.parse::<std::net::Ipv6Addr>().unwrap().octets().to_vec()]).try_into().unwrap()
    } else {
        Value::from(vec![u32::from_ne_bytes(dns.parse::<std::net::Ipv4Addr>().unwrap().octets())]).try_into().unwrap()
    });
    fields
}

#[test]
fn committed_dynamic_readiness_requires_original_address_scope_routes_and_dns() {
    for (ipv6, address, gateway, dns, degraded) in [
        (false, "192.0.2.8", "192.0.2.1", "192.0.2.53", vec!["169.254.1.2", "0.0.0.0", "127.0.0.1", "224.0.0.1", "2001:db8::8"]),
        (true, "2001:db8::8", "fe80::1", "2001:db8::53", vec!["fe80::8", "::", "::1", "ff02::1", "192.0.2.8"]),
    ] {
        let original = dynamic_properties(ipv6, address, gateway, dns);
        let (before, profile) = dynamic_snapshot(original.clone(), ipv6);
        for address in degraded {
            let changed = dynamic_properties(ipv6, address, gateway, dns);
            // A mismatched IPv4 address with an IPv6 prefix is rejected at parsing.
            if let Ok(family) = FamilySnapshot::from_properties(changed) {
                let mut current = before.clone();
                if ipv6 { current.ipv6 = Some(family); } else { current.ipv4 = Some(family); }
                assert!(before.verify_committed(&current, &profile).is_err(), "accepted degraded address {address}");
            }
        }
        for missing in ["RouteData", "Nameservers"] {
            let mut changed = original.clone();
            changed.insert(missing.into(), if missing == "RouteData" {
                Value::from(Vec::<HashMap<String, OwnedValue>>::new()).try_into().unwrap()
            } else if ipv6 {
                Value::from(Vec::<Vec<u8>>::new()).try_into().unwrap()
            } else { Value::from(Vec::<u32>::new()).try_into().unwrap() });
            let (current, _) = dynamic_snapshot(changed, ipv6);
            assert!(before.verify_committed(&current, &profile).is_err(), "accepted lost {missing} for ipv6={ipv6}");
        }
    }
}

#[test]
fn committed_dynamic_readiness_allows_renewed_values_and_local_only_networks() {
    for (ipv6, before_values, after_values) in [
        (false, ("192.0.2.8", "192.0.2.1", "192.0.2.53"), ("198.51.100.8", "198.51.100.1", "198.51.100.53")),
        (true, ("2001:db8::8", "fe80::1", "2001:db8::53"), ("fd00::8", "fe80::2", "fd00::53")),
    ] {
        let (before, profile) = dynamic_snapshot(dynamic_properties(ipv6, before_values.0, before_values.1, before_values.2), ipv6);
        let (current, _) = dynamic_snapshot(dynamic_properties(ipv6, after_values.0, after_values.1, after_values.2), ipv6);
        assert!(before.verify_committed(&current, &profile).is_ok());
        let mut local = dynamic_properties(ipv6, if ipv6 { "fe80::8" } else { "169.254.1.8" }, "", after_values.2);
        local.insert("RouteData".into(), Value::from(Vec::<HashMap<String, OwnedValue>>::new()).try_into().unwrap());
        local.insert("Nameservers".into(), if ipv6 { Value::from(Vec::<Vec<u8>>::new()).try_into().unwrap() }
            else { Value::from(Vec::<u32>::new()).try_into().unwrap() });
        let (local, _) = dynamic_snapshot(local, ipv6);
        assert!(local.verify_committed(&local, &profile).is_ok());
    }
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
