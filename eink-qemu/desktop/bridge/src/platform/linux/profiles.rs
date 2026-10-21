//! Lossless NetworkManager profile migration. Retain D-Bus variant signatures
//! in the durable journal; JSON numbers alone cannot restore native settings.
use crate::model::BridgeError;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use zbus::zvariant::{OwnedValue, Str};

pub type Settings = BTreeMap<String, BTreeMap<String, OwnedValue>>;
#[derive(Debug, Clone, PartialEq)]
pub struct Profile(pub Settings);
impl From<Settings> for Profile { fn from(value: Settings) -> Self { Self(value) } }
impl std::ops::Deref for Profile { type Target = Settings; fn deref(&self) -> &Settings { &self.0 } }
impl std::ops::DerefMut for Profile { fn deref_mut(&mut self) -> &mut Settings { &mut self.0 } }
impl Serialize for Profile {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let context = zbus::zvariant::serialized::Context::new_dbus(zbus::zvariant::LE, 0);
        let bytes = zbus::zvariant::to_bytes(context, &self.0).map_err(serde::ser::Error::custom)?;
        if !bytes.fds().is_empty() { return Err(serde::ser::Error::custom("Network profiles cannot contain file descriptors")); }
        bytes.bytes().serialize(serializer)
    }
}
impl<'de> Deserialize<'de> for Profile {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let bytes = Vec::<u8>::deserialize(deserializer)?;
        let context = zbus::zvariant::serialized::Context::new_dbus(zbus::zvariant::LE, 0);
        let data = zbus::zvariant::serialized::Data::new(bytes, context);
        let (settings, used): (Settings, usize) = data.deserialize().map_err(serde::de::Error::custom)?;
        if used != data.len() { return Err(serde::de::Error::custom("Trailing network profile bytes")); }
        Ok(Self(settings))
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Migration { pub bridge: Profile, pub port: Profile, pub original_disabled: Profile }
impl Migration {
    /// Saved "auto" defaults are not portable from Ethernet to a bridge. Keep
    /// the live link identity and effective priorities, leaving the original
    /// profile untouched for restoration. The caller must still verify the
    /// complete live address/route/DNS snapshot after activation.
    pub fn preserve_runtime(&mut self, runtime: &crate::linux_verify::HostSnapshot, mac: [u8; 6], mtu: u32) -> Result<(), BridgeError> {
        if mac == [0; 6] || mac[0] & 1 != 0 || mtu < 68 || mtu > 65535 {
            return Err(BridgeError::new("EHOSTNETWORK", "Cannot preserve this adapter's hardware address or MTU."));
        }
        let mut updated = self.clone();
        let mac: OwnedValue = zbus::zvariant::Value::from(mac.to_vec()).try_into().map_err(|_| unsupported())?;
        updated.bridge.get_mut("bridge").ok_or_else(unsupported)?.insert("mac-address".into(), mac.clone());
        for profile in [&mut updated.bridge, &mut updated.port] {
            let wired = profile.entry("802-3-ethernet".into()).or_default();
            wired.remove("assigned-mac-address");
            wired.insert("cloned-mac-address".into(), mac.clone());
            wired.insert("mtu".into(), mtu.into());
        }
        for (family, live) in [("ipv4", &runtime.ipv4), ("ipv6", &runtime.ipv6)] {
            let settings = updated.bridge.get_mut(family).ok_or_else(unsupported)?;
            let method = settings.get("method").and_then(|v| <&str>::try_from(v).ok());
            if method == Some("disabled") { continue; }
            let live = live.as_ref().ok_or_else(|| BridgeError::new("EUNSUPPORTED", "Wait until both configured IP families are ready before converting this connection."))?;
            if settings.get("route-metric").map(|v| i64::try_from(v)).transpose().map_err(|_| unsupported())?.unwrap_or(-1) == -1 {
                settings.insert("route-metric".into(), live.route_metric()?.into());
            }
            if settings.get("dns-priority").map(|v| i32::try_from(v)).transpose().map_err(|_| unsupported())?.unwrap_or(0) == 0 {
                settings.insert("dns-priority".into(), live.dns_priority()?.into());
            }
        }
        *self = updated;
        Ok(())
    }
}
fn unsupported() -> BridgeError { BridgeError::new("EUNSUPPORTED", "This connection cannot be converted automatically. Select an existing bridge or use a plain Ethernet connection.") }
fn text(value: &str) -> OwnedValue { Str::from(value).into() }
fn string<'a>(profile: &'a Profile, section: &str, key: &str) -> Option<&'a str> { profile.get(section)?.get(key).and_then(|v| <&str>::try_from(v).ok()) }
fn interface(value: &str) -> bool { !value.is_empty() && value.len() < 16 && value.bytes().all(|v| v.is_ascii_alphanumeric() || b"_.-".contains(&v)) }
fn uuid(value: &str) -> bool { value.len() == 36 && value.bytes().enumerate().all(|(i, b)| if [8, 13, 18, 23].contains(&i) { b == b'-' } else { b.is_ascii_hexdigit() }) }

pub fn migrate(original: &Profile, adapter: &str, bridge_name: &str, bridge_uuid: &str, port_uuid: &str) -> Result<Migration, BridgeError> {
    if !interface(adapter) || !interface(bridge_name) || !uuid(bridge_uuid) || !uuid(port_uuid) || bridge_uuid == port_uuid {
        return Err(BridgeError::new("EINVAL", "Invalid managed bridge identity."));
    }
    if string(original, "connection", "type") != Some("802-3-ethernet")
        || !string(original, "connection", "uuid").is_some_and(uuid)
        || original.keys().any(|key| !["connection", "802-3-ethernet", "ipv4", "ipv6", "proxy", "match"].contains(&key.as_str())) {
        return Err(unsupported());
    }
    let connection = &original["connection"];
    if ["master", "controller", "slave-type", "port-type"].iter().any(|key| connection.contains_key(*key)) { return Err(unsupported()); }
    if connection.get("secondaries").is_some_and(|v| Vec::<String>::try_from(v.clone()).map_or(true, |v| !v.is_empty())) { return Err(unsupported()); }
    for section in ["ipv4", "ipv6"] {
        if string(original, section, "method") == Some("ignore") {
            return Err(BridgeError::new("EUNSUPPORTED", "This connection leaves IP configuration outside NetworkManager. Select an existing bridge or configure NetworkManager to manage both IP families first."));
        }
        if !matches!(string(original, section, "method"), None | Some("auto" | "manual" | "disabled" | "ignore" | "link-local" | "dhcp")) { return Err(unsupported()); }
    }
    let mut bridge = original.clone();
    bridge.remove("match");
    bridge.remove("802-3-ethernet");
    // MTU belongs on both the bridge and its physical port. Other wired
    // properties (hardware matching, negotiation, wake-on-LAN) stay on the port.
    if let Some(mtu) = original.get("802-3-ethernet").and_then(|wired| wired.get("mtu")) {
        bridge.insert("802-3-ethernet".into(), BTreeMap::from([("mtu".into(), mtu.clone())]));
    }
    let settings = bridge.get_mut("connection").unwrap();
    settings.insert("id".into(), text(&format!("eink-qemu {bridge_name}")));
    settings.insert("uuid".into(), text(bridge_uuid));
    settings.insert("type".into(), text("bridge"));
    settings.insert("interface-name".into(), text(bridge_name));
    settings.insert("autoconnect-slaves".into(), 1i32.into());
    settings.remove("timestamp");
    bridge.insert("bridge".into(), BTreeMap::from([("stp".into(), false.into())]));
    let mut port = original.clone();
    port.remove("ipv4"); port.remove("ipv6"); port.remove("proxy");
    let settings = port.get_mut("connection").unwrap();
    settings.insert("id".into(), text(&format!("eink-qemu {bridge_name} port")));
    settings.insert("uuid".into(), text(port_uuid));
    settings.insert("interface-name".into(), text(adapter));
    settings.insert("master".into(), text(bridge_uuid));
    settings.insert("slave-type".into(), text("bridge"));
    settings.remove("zone"); settings.remove("timestamp");
    let mut original_disabled = original.clone();
    original_disabled.get_mut("connection").unwrap().insert("autoconnect".into(), false.into());
    Ok(Migration { bridge, port, original_disabled })
}

/// NetworkManager updates timestamp when activating a profile. Every other
/// difference is retained as a conflict, including routes/DNS and unknown keys.
pub fn same_configuration(left: &Profile, right: &Profile) -> bool {
    let normalize = |value: &Profile| {
        let mut value = value.clone();
        if let Some(connection) = value.get_mut("connection") { connection.remove("timestamp"); }
        if let Some(wired) = value.get_mut("802-3-ethernet") {
            // GetSettings emits both the deprecated byte-array property and
            // its string alias. Ignore the alias only when it agrees exactly.
            let assigned = wired.get("assigned-mac-address").and_then(|v| <&str>::try_from(v).ok())
                .and_then(|value| value.split(':').map(|part| u8::from_str_radix(part, 16)).collect::<Result<Vec<_>, _>>().ok());
            let cloned = wired.get("cloned-mac-address").and_then(|v| Vec::<u8>::try_from(v.clone()).ok());
            if assigned.as_ref().is_some_and(|mac| mac.len() == 6) && assigned == cloned { wired.remove("assigned-mac-address"); }
            if wired.get("auto-negotiate").and_then(|v| bool::try_from(v).ok()) == Some(false) { wired.remove("auto-negotiate"); }
            if wired.get("mac-address-blacklist").is_some_and(|v| Vec::<String>::try_from(v.clone()).is_ok_and(|v| v.is_empty())) { wired.remove("mac-address-blacklist"); }
            if wired.get("s390-options").is_some_and(|v| std::collections::HashMap::<String, String>::try_from(v.clone()).is_ok_and(|v| v.is_empty())) { wired.remove("s390-options"); }
        }
        let interface = string(&value, "connection", "interface-name").map(str::to_owned);
        for section in ["bridge", "bridge-port"] {
          if let Some(bridge) = value.get_mut(section) {
            // NetworkManager 1.36 materializes its deprecated interface-name
            // alias and empty VLAN list. Only these equivalent defaults are
            // ignored; a mismatched alias or any configured VLAN is a conflict.
            if section == "bridge" && bridge.get("interface-name").and_then(|v| <&str>::try_from(v).ok()) == interface.as_deref() { bridge.remove("interface-name"); }
            if bridge.get("vlans").is_some_and(|v| Vec::<std::collections::HashMap<String, OwnedValue>>::try_from(v.clone()).is_ok_and(|v| v.is_empty())) { bridge.remove("vlans"); }
          }
          if value.get(section).is_some_and(BTreeMap::is_empty) { value.remove(section); }
        }
        value
    };
    normalize(left) == normalize(right)
}
