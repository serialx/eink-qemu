//! Durable IP configuration and runtime evidence. Adapter GUIDs are separate
//! from connection settings so conversion can compare the physical adapter
//! with the new bridge without treating DHCP/SLAAC addresses as static input.
use crate::{model::BridgeError, windows_command::{self, Tool}, windows_device::{self, DeviceIdentity}};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, ffi::OsStr, net::IpAddr, time::Duration};

fn invalid() -> BridgeError { BridgeError::new("EHOSTNETWORK", "Windows returned incomplete host network settings.") }
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all="PascalCase", deny_unknown_fields)]
pub struct Interface<T = u32> {
    pub address_family: u32, pub dhcp: T, pub router_discovery: T,
    pub automatic_metric: T, pub interface_metric: T, pub nl_mtu: T,
    pub forwarding: T, pub weak_host_send: T, pub weak_host_receive: T,
    pub advertising: T, pub advertise_default_route: T,
}
impl Interface {
    pub fn sparse(&self) -> Interface<Option<u32>> {
        Interface { address_family:self.address_family, dhcp:Some(self.dhcp),router_discovery:Some(self.router_discovery),
            automatic_metric:Some(self.automatic_metric),interface_metric:Some(self.interface_metric),nl_mtu:Some(self.nl_mtu),
            forwarding:Some(self.forwarding),weak_host_send:Some(self.weak_host_send),weak_host_receive:Some(self.weak_host_receive),
            advertising:Some(self.advertising),advertise_default_route:Some(self.advertise_default_route) }
    }
}
impl Interface<Option<u32>> {
    pub(crate) fn validate(&self)->Result<(),BridgeError>{
        // PersistentStore omits options that Windows has not explicitly saved.
        // Keep those nulls: active values include defaults and RA-derived MTU.
        if self.nl_mtu.is_some_and(|v|v < if self.address_family==2 {68}else{1280} || v>65535)
            || self.dhcp.is_some_and(|v|v>1) || self.automatic_metric.is_some_and(|v|v>1)
            || self.router_discovery.is_some_and(|v|v>2)
            || [self.forwarding,self.weak_host_send,self.weak_host_receive,self.advertising,self.advertise_default_route]
                .into_iter().flatten().any(|v|v>1) {return Err(invalid());}
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all="PascalCase", deny_unknown_fields)]
pub struct Address {
    #[serde(rename="IPAddress")] pub ip_address: String,
    pub prefix_length: u8, pub address_family: u32, pub prefix_origin: u32,
    pub suffix_origin: u32, pub skip_as_source: bool,
    // Persistent entries do not establish runtime usability. Older recorded
    // probes can still deserialize, but cannot pass the new readiness check.
    #[serde(default, skip_serializing_if="Option::is_none")]
    pub address_state: Option<u32>,
    #[serde(default, skip_serializing_if="Option::is_none")]
    pub valid_lifetime_ticks: Option<u64>,
    #[serde(default, skip_serializing_if="Option::is_none")]
    pub preferred_lifetime_ticks: Option<u64>,
}
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all="PascalCase", deny_unknown_fields)]
pub struct Route {
    pub destination_prefix: String, pub next_hop: String, pub route_metric: u32, pub protocol: u32, pub publish: u32,
    #[serde(default, skip_serializing_if="Option::is_none")]
    pub valid_lifetime_ticks: Option<u64>,
    #[serde(default, skip_serializing_if="Option::is_none")]
    pub preferred_lifetime_ticks: Option<u64>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all="PascalCase", deny_unknown_fields)]
pub struct Dns { pub address_family: u32, pub server_addresses: Vec<String> }
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all="PascalCase", deny_unknown_fields)]
pub struct DnsClient { pub connection_specific_suffix: String, pub register_this_connections_address: bool, pub use_suffix_when_registering: bool }
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all="camelCase", deny_unknown_fields)]
pub struct ManualDns { pub protocol: String, pub name_server: String, pub domain: String }
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all="PascalCase", deny_unknown_fields)]
pub struct Legacy {
    pub tcpip_netbios_options: Option<u32>,
    #[serde(rename="WINSEnableLMHostsLookup")] pub wins_enable_lm_hosts_lookup: Option<bool>,
    #[serde(rename="WINSPrimaryServer")] pub wins_primary_server: Option<String>,
    #[serde(rename="WINSSecondaryServer")] pub wins_secondary_server: Option<String>,
    #[serde(rename="WINSScopeID")] pub wins_scope_id: Option<String>,
    #[serde(default, skip_serializing_if="Option::is_none")]
    pub manual_servers: Option<Vec<String>>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all="PascalCase", deny_unknown_fields)]
pub struct ConfiguredDhcp {pub address_family:u32,pub enabled:u32}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all="camelCase", deny_unknown_fields)]
pub struct Snapshot {
    pub guid: String, pub instance_id: String, pub mac: String,
    pub interfaces: Vec<Interface>, pub addresses: Vec<Address>, pub routes: Vec<Route>,
    #[serde(default, skip_serializing_if="Option::is_none")]
    pub persistent_interfaces: Option<Vec<Interface<Option<u32>>>>,
    #[serde(default, skip_serializing_if="Option::is_none")]
    pub configured_dhcp: Option<Vec<ConfiguredDhcp>>,
    pub active_addresses: Vec<Address>, pub active_routes: Vec<Route>,
    pub dns: Vec<Dns>, pub dns_client: Vec<DnsClient>, pub manual_dns: Vec<ManualDns>,
    #[serde(default, skip_serializing_if="Option::is_none")]
    pub legacy: Option<Legacy>,
}
pub(crate) fn ip(value: &str) -> Result<IpAddr, BridgeError> {
    let bare = if let Some((address, zone)) = value.split_once('%') {
        if !address.contains(':') || zone.parse::<u32>().is_err() { return Err(invalid()); }
        address
    } else { value };
    bare.parse().map_err(|_| invalid())
}
pub(crate) fn prefix(value: &str) -> Result<(IpAddr, u8), BridgeError> {
    let (address, length)=value.split_once('/').ok_or_else(invalid)?;
    let address=ip(address)?; let length: u8=length.parse().map_err(|_| invalid())?;
    if length > if address.is_ipv4() {32} else {128} {return Err(invalid());}
    Ok((address,length))
}
fn family(address: &IpAddr) -> u32 { if address.is_ipv4() {2} else {23} }
pub(crate) fn lifetimes(valid: Option<u64>, preferred: Option<u64>) -> Result<(),BridgeError> {
    match (valid,preferred) {
        (None,None) => Ok(()),
        (Some(valid),Some(preferred)) if valid <= i64::MAX as u64 && preferred <= valid => Ok(()),
        _ => Err(invalid()),
    }
}
impl Snapshot {
    pub fn read(expected: &DeviceIdentity) -> Result<Self, BridgeError> {
        let id=windows_device::guid(&expected.guid)?;
        if windows_device::enumerate()?.get(&id) != Some(expected) { return Err(invalid()); }
        // Only the validated GUID is substituted; names and registry strings
        // never become executable PowerShell source.
        let script=format!("$ErrorActionPreference='Stop'; [Console]::OutputEncoding=New-Object System.Text.UTF8Encoding($false);\n{}\n[Console]::Out.Write((Read-EinkNetwork '{}' | ConvertTo-Json -Depth 8 -Compress))",include_str!("ip_snapshot.ps1"),id);
        let arguments=["-NoProfile","-NonInteractive","-Command",&script].map(OsStr::new);
        let output=windows_command::run(Tool::PowerShell,&arguments,Duration::from_secs(120))
            .and_then(|output|output.checked()).map_err(|error|error.context("Read Windows connection settings"))?;
        let mut snapshot: Self=serde_json::from_slice(&output.stdout).map_err(|_| invalid())?;
        snapshot.normalize()?;
        if snapshot.active_addresses.iter().any(|address| address.address_state.is_none()) { return Err(invalid()); }
        snapshot.require_restorable()?;
        if snapshot.guid != id || !snapshot.instance_id.eq_ignore_ascii_case(&expected.instance_id)
            || windows_device::enumerate()?.get(&id) != Some(expected) { return Err(invalid()); }
        Ok(snapshot)
    }
    pub fn normalize(&mut self) -> Result<(), BridgeError> {
        self.guid=windows_device::guid(&self.guid)?;
        if self.interfaces.len()!=2 || self.dns.len()!=2 || self.dns_client.len()!=1 || self.manual_dns.len()!=2 { return Err(invalid()); }
        self.dns.sort_by_key(|i|i.address_family);
        if self.dns.iter().map(|i|i.address_family).collect::<Vec<_>>()!=[2,23] {return Err(invalid());}
        self.interfaces.sort_by_key(|i|i.address_family);
        if self.interfaces.iter().map(|i|i.address_family).collect::<Vec<_>>()!=[2,23] {return Err(invalid());}
        for row in &self.interfaces { row.sparse().validate()?; }
        if let Some(rows)=&mut self.persistent_interfaces {
            rows.sort_by_key(|i|i.address_family);
            if rows.iter().map(|i|i.address_family).collect::<Vec<_>>()!=[2,23] {return Err(invalid());}
            for row in rows { row.validate()?; }
        }
        if let Some(rows)=&mut self.configured_dhcp {
            rows.sort_by_key(|row|row.address_family);
            if rows.iter().map(|r|r.address_family).collect::<Vec<_>>()!=[2,23] || rows.iter().any(|r|r.enabled>1){return Err(invalid());}
        }
        for addresses in [&mut self.addresses,&mut self.active_addresses] {
            for address in addresses.iter_mut() {
                let value=ip(&address.ip_address)?;
                lifetimes(address.valid_lifetime_ticks,address.preferred_lifetime_ticks)?;
                if address.address_state.is_some_and(|state| state>4) || family(&value)!=address.address_family || address.prefix_length>if value.is_ipv4(){32}else{128} {return Err(invalid());}
                address.ip_address=value.to_string();
            }
            addresses.sort();
            if addresses.windows(2).any(|w|w[0]==w[1]) {return Err(invalid());}
        }
        for routes in [&mut self.routes,&mut self.active_routes] {
            for route in routes.iter_mut() {
                lifetimes(route.valid_lifetime_ticks,route.preferred_lifetime_ticks)?;
                let (network,length)=prefix(&route.destination_prefix)?; let next=ip(&route.next_hop)?;
                if family(&network)!=family(&next) {return Err(invalid());}
                route.destination_prefix=format!("{network}/{length}"); route.next_hop=next.to_string();
            }
            routes.sort();
        }
        for dns in &mut self.dns {
            for server in &mut dns.server_addresses { let value=ip(server)?; if family(&value)!=dns.address_family {return Err(invalid());} *server=value.to_string(); }
        }
        self.manual_dns.sort_by(|a,b|a.protocol.cmp(&b.protocol));
        if self.manual_dns.iter().map(|dns|dns.protocol.as_str()).collect::<Vec<_>>()!=["Tcpip","Tcpip6"] {return Err(invalid());}
        for dns in &mut self.manual_dns {
            let expected_family=if dns.protocol=="Tcpip" {2} else {23};
            let servers=dns.name_server.split(|c:char|c.is_ascii_whitespace()||c==','||c==';').filter(|s|!s.is_empty())
                .map(|server|{let value=ip(server)?;if family(&value)!=expected_family {return Err(invalid());} Ok(value.to_string())}).collect::<Result<Vec<_>,BridgeError>>()?;
            dns.name_server=servers.join(",");
        }
        if let Some(legacy)=&mut self.legacy {
            if legacy.tcpip_netbios_options.is_some_and(|value|value>2) {return Err(invalid());}
            for server in [&mut legacy.wins_primary_server,&mut legacy.wins_secondary_server] {
                if let Some(value)=server {
                    if value.is_empty() {*server=None;} else {
                        let address=ip(value)?;
                        if !address.is_ipv4() {return Err(invalid());}
                        *value=address.to_string();
                    }
                }
            }
            if let Some(servers)=&mut legacy.manual_servers {
                for value in servers {
                    let address=ip(value)?;if !address.is_ipv4(){return Err(invalid());}*value=address.to_string();
                }
            }
        }
        Ok(())
    }
    /// Older QA snapshots remain readable for historical comparisons, but
    /// must never authorize migration without restoration metadata.
    pub fn require_restorable(&self) -> Result<(), BridgeError> {
        if self.legacy.as_ref().is_none_or(|legacy|legacy.manual_servers.is_none()) || self.persistent_interfaces.is_none() || self.configured_dhcp.is_none()
            || self.addresses.iter().any(|value|value.valid_lifetime_ticks.is_none() || value.preferred_lifetime_ticks.is_none())
            || self.routes.iter().any(|value|value.valid_lifetime_ticks.is_none() || value.preferred_lifetime_ticks.is_none()) {
            return Err(invalid());
        }
        Ok(())
    }
    /// Fixed settings, excluding lease values and an automatically supplied DNS
    /// suffix. These comparisons remain meaningful after DHCP/SLAAC renewal.
    pub fn configuration_changes(&self, current: &Self) -> Vec<&'static str> {
        let mut changes=Vec::new();
        // Windows calculates this metric from link speed when automatic mode
        // is enabled. Preserve the mode; do not turn its runtime value into a
        // manual metric when moving settings to a different interface.
        let known_dhcp=self.configured_dhcp.is_some() && current.configured_dhcp.is_some();
        let interfaces=|rows:&[Interface]|rows.iter().map(|row|{
            let mut row=row.clone();if row.automatic_metric==1{row.interface_metric=0;}
            // Effective DHCP can be disabled while the saved mode is enabled
            // (for example an isolated bridge without router advertisements).
            if known_dhcp{row.dhcp=0;}row
        }).collect::<Vec<_>>();
        if interfaces(&self.interfaces)!=interfaces(&current.interfaces) {changes.push("IP interface settings");}
        if self.persistent_interfaces!=current.persistent_interfaces {changes.push("persistent IP interface settings");}
        if self.configured_dhcp!=current.configured_dhcp {changes.push("configured DHCP settings");}
        if self.addresses!=current.addresses {changes.push("static addresses");}
        if self.routes!=current.routes {changes.push("persistent routes");}
        if self.manual_dns!=current.manual_dns {changes.push("configured DNS servers or domain");}
        let legacy=|value:&Option<Legacy>|value.as_ref().map(|value|{
            let mut value=value.clone();value.wins_primary_server=None;value.wins_secondary_server=None;value
        });
        if legacy(&self.legacy)!=legacy(&current.legacy) {changes.push("NetBIOS or WINS settings");}
        if self.dns_client.len()!=1 || current.dns_client.len()!=1
            || self.dns_client[0].register_this_connections_address!=current.dns_client[0].register_this_connections_address
            || self.dns_client[0].use_suffix_when_registering!=current.dns_client[0].use_suffix_when_registering {changes.push("DNS registration");}
        changes
    }
    /// Immediate conversion verification. IPv6 privacy addresses and scope IDs
    /// may change with the interface; preserve their routed prefixes, manual
    /// addresses, and default routes rather than freezing temporary addresses.
    pub fn runtime_changes(&self, current: &Self) -> Result<Vec<&'static str>,BridgeError> {
        let mut changes=Vec::new();
        let addresses=|snapshot:&Self| -> Result<BTreeSet<String>,BridgeError> {
            if snapshot.active_addresses.iter().any(|address| address.address_state.is_none()) { return Err(invalid()); }
            // Tentative, duplicate, invalid, or deprecated addresses must not
            // make a connection look ready. Privacy-address rotation may leave
            // deprecated entries alongside a usable preferred replacement.
            snapshot.active_addresses.iter().filter(|address| address.address_state==Some(4)).map(|address| {
                let value=ip(&address.ip_address)?;
                if let IpAddr::V6(value)=value {
                    if value.is_unicast_link_local() {return Ok("ipv6-link-local".into());}
                    if address.prefix_origin==4 {
                        // Temporary SLAAC addresses can be reported with /128;
                        // the associated RA route carries the actual prefix.
                        return Ok("ipv6-slaac".into());
                    }
                }
                Ok(format!("{}/{}:{}",address.ip_address,address.prefix_length,address.skip_as_source))
            }).collect()
        };
        if addresses(self)?!=addresses(current)? {changes.push("active addresses");}
        let routes=|snapshot:&Self| snapshot.active_routes.iter().filter(|route|route.protocol!=2).cloned().collect::<BTreeSet<_>>();
        if routes(self)!=routes(current) {changes.push("active routes");}
        let dns=|snapshot:&Self| snapshot.dns.iter().flat_map(|dns|dns.server_addresses.iter().cloned()).collect::<BTreeSet<_>>();
        if dns(self)!=dns(current) {changes.push("active DNS servers");}
        if self.dns_client!=current.dns_client {changes.push("active DNS suffix or registration");}
        let wins=|value:&Option<Legacy>|value.as_ref().map(|value|(value.wins_primary_server.clone(),value.wins_secondary_server.clone()));
        if wins(&self.legacy)!=wins(&current.legacy) {changes.push("active WINS servers");}
        if self.mac!=current.mac {changes.push("link hardware address");}
        Ok(changes)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn address_scopes_and_prefixes_are_validated_without_shell_interpretation() {
        assert_eq!(ip("fe80::1%4").unwrap().to_string(),"fe80::1");
        for bad in ["fe80::1%4;whoami","1.2.3.4%2","$(whoami)","1.2.3.999"] {assert!(ip(bad).is_err());}
        assert!(prefix("::/129").is_err()); assert!(prefix("1.2.3.4/33").is_err());
    }
    fn fixture() -> Snapshot {
        let mut snapshot: Snapshot=serde_json::from_str(include_str!("../../../tests/fixtures/windows-dhcp-profile.json")).unwrap();
        snapshot.normalize().unwrap(); snapshot
    }
    #[test]
    fn migration_allows_ipv6_privacy_rotation_but_checks_prefix_routes_and_dns() {
        let original=fixture(); let mut migrated=original.clone();
        for address in &mut migrated.active_addresses {
            if address.address_family==23 && address.prefix_origin==4 {address.ip_address="2001:db8:abcd:1234::1234".into();}
        }
        assert!(original.runtime_changes(&migrated).unwrap().is_empty());
        migrated.active_routes.iter_mut().find(|r|r.destination_prefix=="::/0").unwrap().next_hop="fe80::1234".into();
        assert_eq!(original.runtime_changes(&migrated).unwrap(),vec!["active routes"]);
        migrated.dns[0].server_addresses=vec!["192.0.2.53".into()];
        assert!(original.runtime_changes(&migrated).unwrap().contains(&"active DNS servers"));
        assert!(original.configuration_changes(&migrated).is_empty());
    }
    #[test]
    fn configuration_checks_include_static_dns_metrics_and_mtu() {
        let original=fixture(); let mut edited=original.clone();
        edited.manual_dns[0].name_server="192.0.2.53".into();
        edited.interfaces[0].interface_metric+=1; edited.interfaces[1].nl_mtu=1400;
        assert_eq!(original.configuration_changes(&edited),vec!["IP interface settings","configured DNS servers or domain"]);
        edited=original.clone(); edited.interfaces.pop();
        assert!(edited.normalize().is_err());
        edited=original.clone(); edited.active_addresses[0].address_family=99;
        assert!(edited.normalize().is_err());
    }
    #[test]
    fn restoration_requires_lifetimes_and_tracks_legacy_configuration() {
        let mut original=fixture();
        assert!(original.require_restorable().is_err());
        original.legacy=Some(Legacy {tcpip_netbios_options:Some(0),wins_enable_lm_hosts_lookup:Some(true),
            wins_primary_server:None,wins_secondary_server:None,wins_scope_id:Some(String::new()),manual_servers:Some(Vec::new())});
        original.persistent_interfaces=Some(original.interfaces.iter().map(Interface::sparse).collect());
        for address in &mut original.addresses {
            address.valid_lifetime_ticks=Some(i64::MAX as u64);address.preferred_lifetime_ticks=Some(i64::MAX as u64);
        }
        for route in &mut original.routes {
            route.valid_lifetime_ticks=Some(i64::MAX as u64);route.preferred_lifetime_ticks=Some(i64::MAX as u64);
        }
        original.configured_dhcp=Some(original.interfaces.iter().map(|i|ConfiguredDhcp{address_family:i.address_family,enabled:i.dhcp}).collect());
        original.require_restorable().unwrap();original.normalize().unwrap();
        let mut changed=original.clone();
        changed.legacy.as_mut().unwrap().manual_servers=Some(vec!["192.0.2.53".into()]);
        assert_eq!(original.configuration_changes(&changed),vec!["NetBIOS or WINS settings"]);
        changed=original.clone();changed.legacy.as_mut().unwrap().wins_primary_server=Some("192.0.2.53".into());
        assert!(original.configuration_changes(&changed).is_empty());
        assert_eq!(original.runtime_changes(&changed).unwrap(),vec!["active WINS servers"]);
        changed.legacy.as_mut().unwrap().tcpip_netbios_options=Some(3);
        assert!(changed.normalize().is_err());
        for pair in [(Some(1),Some(2)),(Some(u64::MAX),Some(0)),(None,Some(0)),(Some(0),None)] {
            assert!(lifetimes(pair.0,pair.1).is_err());
        }
    }
    #[test]
    fn configured_dhcp_is_independent_of_sparse_interface_defaults(){
        let mut source=fixture();source.configured_dhcp=Some(vec![ConfiguredDhcp{address_family:2,enabled:1},ConfiguredDhcp{address_family:23,enabled:1}]);
        let mut inactive=source.clone();inactive.interfaces[1].dhcp=0;
        assert!(source.configuration_changes(&inactive).is_empty());
        let mut changed=source.clone();changed.configured_dhcp.as_mut().unwrap()[0].enabled=0;
        assert_eq!(source.configuration_changes(&changed),vec!["configured DHCP settings"]);
        changed.configured_dhcp.as_mut().unwrap()[0].enabled=2;assert!(changed.normalize().is_err());
    }
    #[test]
    fn automatic_metrics_are_not_frozen_and_persistent_interfaces_remain_distinct(){
        let original=fixture();let mut current=original.clone();
        current.interfaces[0].interface_metric+=10;
        assert!(original.configuration_changes(&current).is_empty());
        current.interfaces[0].automatic_metric=0;
        assert_eq!(original.configuration_changes(&current),vec!["IP interface settings"]);
        let mut original=original;original.persistent_interfaces=Some(original.interfaces.iter().map(Interface::sparse).collect());
        let mut current=original.clone();current.persistent_interfaces.as_mut().unwrap()[0].dhcp=Some(0);
        assert_eq!(original.configuration_changes(&current),vec!["persistent IP interface settings"]);
    }
    #[test]
    fn persistent_defaults_are_distinct_from_explicit_active_values() {
        let mut snapshot=fixture();
        let rows:Vec<Interface<Option<u32>>>=serde_json::from_value(serde_json::json!([
            {"AddressFamily":2,"Dhcp":null,"RouterDiscovery":null,"AutomaticMetric":null,"InterfaceMetric":null,"NlMtu":null,"Forwarding":null,"WeakHostSend":null,"WeakHostReceive":null,"Advertising":null,"AdvertiseDefaultRoute":null},
            {"AddressFamily":23,"Dhcp":null,"RouterDiscovery":null,"AutomaticMetric":null,"InterfaceMetric":null,"NlMtu":null,"Forwarding":null,"WeakHostSend":null,"WeakHostReceive":null,"Advertising":null,"AdvertiseDefaultRoute":null}
        ])).unwrap();
        snapshot.persistent_interfaces=Some(rows);snapshot.normalize().unwrap();
        let mut explicit=snapshot.clone();explicit.persistent_interfaces.as_mut().unwrap()[0].dhcp=Some(1);
        assert_eq!(snapshot.configuration_changes(&explicit),vec!["persistent IP interface settings"]);
        explicit.persistent_interfaces.as_mut().unwrap()[1].nl_mtu=Some(1000);
        assert!(explicit.normalize().is_err());
    }
    #[test]
    fn duplicate_or_tentative_addresses_do_not_satisfy_runtime_verification() {
        let original=fixture();
        for state in [0,1,2,3] {
            let mut unavailable=original.clone();
            unavailable.active_addresses.iter_mut().find(|address|address.address_family==2).unwrap().address_state=Some(state);
            assert!(original.runtime_changes(&unavailable).unwrap().contains(&"active addresses"));
        }
        let mut missing=original.clone();missing.active_addresses[0].address_state=None;
        assert!(original.runtime_changes(&missing).is_err());
        let mut rotating=original.clone();
        let mut old=rotating.active_addresses.iter().find(|address|address.prefix_origin==4).unwrap().clone();
        old.address_state=Some(3);old.ip_address="2001:db8:abcd:1234::1111".into();
        rotating.active_addresses.push(old);
        assert!(original.runtime_changes(&rotating).unwrap().is_empty());
    }
    #[test]
    #[ignore = "requires a native adapter with complete Windows network settings"]
    fn native_read_only_profile_contains_both_address_families() {
        let adapter=crate::inspect().unwrap().adapters.into_iter().find(|a|a.eligible).unwrap();
        let devices=windows_device::enumerate().unwrap();
        let snapshot=Snapshot::read(devices.get(adapter.id.as_str()).unwrap()).unwrap();
        assert!(snapshot.configuration_changes(&snapshot).is_empty());
        assert!(snapshot.runtime_changes(&snapshot).unwrap().is_empty());
    }
    #[test]
    #[ignore = "requires recorded native before/during/after bridge snapshots"]
    fn recorded_native_conversion_and_restoration_preserve_host_configuration() {
        let root=std::path::PathBuf::from(std::env::var_os("EINK_BRIDGE_NETWORK_FIXTURE_DIR").unwrap());
        let read=|name:&str| {
            let text=std::fs::read_to_string(root.join(name)).unwrap();
            let mut snapshot: Snapshot=serde_json::from_str(text.trim_start_matches('\u{feff}')).unwrap();
            snapshot.normalize().unwrap(); snapshot
        };
        let before=read("ip-before.json");
        for name in ["ip-during.json","ip-after.json"] {
            let current=read(name);
            assert_eq!(before.configuration_changes(&current),Vec::<&str>::new(),"{name}");
            assert_eq!(before.runtime_changes(&current).unwrap(),Vec::<&str>::new(),"{name}");
        }
    }
}
