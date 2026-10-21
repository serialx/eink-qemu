//! Live host addressing and routing checks, separate from saved profiles.
//! Retain route attributes and DNS ordering; ignore address lifetimes and list
//! enumeration order, which can change without a networking change.
use crate::{linux_profiles::Profile, model::BridgeError};
use serde::{Deserialize, Serialize};
use std::{collections::{BTreeMap, HashMap}, net::IpAddr};
use zbus::zvariant::OwnedValue;
fn invalid() -> BridgeError { BridgeError::new("EHOSTNETWORK", "Cannot verify the host's current addresses, routes and DNS settings.") }
type Fields = BTreeMap<String, OwnedValue>;
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FamilySnapshot { addresses: Vec<Profile>, routes: Vec<Profile>, properties: Profile }
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostSnapshot { pub ipv4: Option<FamilySnapshot>, pub ipv6: Option<FamilySnapshot> }
fn take(properties: &mut Fields, key: &str) -> Result<OwnedValue, BridgeError> { properties.remove(key).ok_or_else(invalid) }
fn list(value: OwnedValue) -> Result<Vec<Fields>, BridgeError> {
    Vec::<HashMap<String, OwnedValue>>::try_from(value).map(|list| list.into_iter().map(|value| value.into_iter().collect()).collect()).map_err(|_| invalid())
}
fn ip(fields: &Fields, address: &str) -> Result<(), BridgeError> {
    let address = fields.get(address).and_then(|v| <&str>::try_from(v).ok()).ok_or_else(invalid)?.parse::<IpAddr>().map_err(|_| invalid())?;
    let prefix = fields.get("prefix").and_then(|v| u32::try_from(v).ok()).ok_or_else(invalid)?;
    if prefix > if address.is_ipv4() { 32 } else { 128 } { return Err(invalid()); }
    Ok(())
}
fn wrapped(fields: Fields) -> Profile { Profile(BTreeMap::from([("data".into(), fields)])) }
fn same_set(left: &[Profile], right: &[Profile]) -> bool {
    left.len() == right.len() && left.iter().all(|entry| left.iter().filter(|v| *v == entry).count() == right.iter().filter(|v| *v == entry).count())
}
impl FamilySnapshot {
    /// An implicit route metric depends on the device type and global NM
    /// configuration. Pin the observed metric only when it is unambiguous.
    /// A kernel IPv6 link-local route has its own metric; ignore it if other
    /// routes identify the connection's effective metric.
    pub(crate) fn route_metric(&self) -> Result<i64, BridgeError> {
        let collect = |skip_link_local: bool| -> Result<std::collections::BTreeSet<u32>, BridgeError> {
            self.routes.iter().filter(|route| {
                let fields = &route["data"];
                !skip_link_local || !(<&str>::try_from(&fields["dest"]).ok() == Some("fe80::")
                    && u32::try_from(&fields["prefix"]).ok() == Some(64)
                    && fields.get("metric").and_then(|v| u32::try_from(v).ok()) == Some(256)
                    && !fields.contains_key("next-hop"))
            }).map(|route| route["data"].get("metric").and_then(|v| u32::try_from(v).ok()).ok_or_else(invalid)).collect()
        };
        let mut metrics = collect(true)?;
        if metrics.is_empty() { metrics = collect(false)?; }
        if metrics.len() != 1 { return Err(BridgeError::new("EUNSUPPORTED", "The connection's automatic route priority is ambiguous. Use an existing bridge or configure an explicit route metric.")); }
        Ok(*metrics.first().unwrap() as i64)
    }
    pub(crate) fn dns_priority(&self) -> Result<i32, BridgeError> {
        i32::try_from(&self.properties["data"]["DnsPriority"]).map_err(|_| invalid())
    }
    pub fn from_properties(mut properties: Fields) -> Result<Self, BridgeError> {
        let addresses = list(take(&mut properties, "AddressData")?)?.into_iter().map(|mut fields| {
            ip(&fields, "address")?;
            fields.retain(|key, _| key == "address" || key == "prefix");
            Ok(wrapped(fields))
        }).collect::<Result<Vec<_>, BridgeError>>()?;
        let routes = list(take(&mut properties, "RouteData")?)?.into_iter().map(|fields| {
            ip(&fields, "dest")?;
            for key in ["metric", "table"] {
                if fields.get(key).is_some_and(|v| u32::try_from(v).is_err()) { return Err(invalid()); }
            }
            if let Some(next) = fields.get("next-hop") {
                <&str>::try_from(next).map_err(|_| invalid())?.parse::<IpAddr>().map_err(|_| invalid())?;
            }
            Ok(wrapped(fields))
        }).collect::<Result<Vec<_>, BridgeError>>()?;
        let mut settings = Fields::new();
        for key in ["Gateway", "Nameservers", "Domains", "Searches", "DnsOptions", "DnsPriority"] {
            settings.insert(key.into(), take(&mut properties, key)?);
        }
        let gateway = <&str>::try_from(&settings["Gateway"]).map_err(|_| invalid())?;
        if !gateway.is_empty() { gateway.parse::<IpAddr>().map_err(|_| invalid())?; }
        let nameservers = settings["Nameservers"].clone();
        if Vec::<u32>::try_from(nameservers.clone()).is_err()
            && !Vec::<Vec<u8>>::try_from(nameservers).is_ok_and(|servers| servers.iter().all(|value| value.len() == 16)) { return Err(invalid()); }
        for key in ["Domains", "Searches", "DnsOptions"] { Vec::<String>::try_from(settings[key].clone()).map_err(|_| invalid())?; }
        i32::try_from(&settings["DnsPriority"]).map_err(|_| invalid())?;
        // Retain modern DNS metadata and Windows name-service settings when
        // exposed. IPv6 has no NameserverData property on supported NM 1.36.
        for key in ["NameserverData", "WinsServerData"] {
            if let Some(value) = properties.remove(key) { settings.insert(key.into(), value); }
        }
        Ok(Self { addresses, routes, properties: wrapped(settings) })
    }
    pub fn preserved_by(&self, current: &Self) -> bool {
        same_set(&self.addresses, &current.addresses) && same_set(&self.routes, &current.routes) && self.properties == current.properties
    }
}
impl HostSnapshot {
    pub fn verify_preserved(&self, current: &Self) -> Result<(), BridgeError> {
        let same = |left: &Option<FamilySnapshot>, right: &Option<FamilySnapshot>| match (left, right) {
            (None, None) => true,
            (Some(left), Some(right)) => left.preserved_by(right),
            _ => false,
        };
        if same(&self.ipv4, &current.ipv4) && same(&self.ipv6, &current.ipv6) { Ok(()) }
        else { Err(BridgeError::new("EHOSTNETWORK", "The host's addresses, routes or DNS settings changed during setup. Restore the previous connection.")) }
    }
}
