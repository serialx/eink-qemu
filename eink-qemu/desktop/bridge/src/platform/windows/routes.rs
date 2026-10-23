//! Reject active-only routes that setup cannot restore. Protocol 3 includes
//! manual, DHCP and RA routes; it is never sufficient evidence by itself.
use crate::{model::BridgeError, windows_device::{self, DeviceIdentity}, windows_profile::{Route, Snapshot}};
use std::{net::{IpAddr, Ipv4Addr, Ipv6Addr}, ptr};
use windows_sys::{core::GUID, Win32::{Foundation::*, NetworkManagement::{IpHelper::*, Ndis::NET_LUID_LH}, Networking::WinSock::*, System::Registry::*}};
fn invalid()->BridgeError{BridgeError::new("EHOSTNETWORK","Windows route sources could not be verified. Refresh bridge setup.")}
fn unsupported()->BridgeError{BridgeError::new("EHOSTNETWORK","This connection has temporary or externally managed routes that automatic bridge setup cannot restore.")}
#[derive(Clone,Debug)]
struct NativeRoute { destination:String, next_hop:String, metric:u32, protocol:u32, origin:i32 }
impl NativeRoute {
    fn matches(&self,row:&Route)->bool{self.destination==row.destination_prefix && self.next_hop==row.next_hop && self.metric==row.route_metric && self.protocol==row.protocol}
}
#[derive(Debug)]
struct Dhcp { enabled:bool, gateways:Vec<String>, manual_gateways:Vec<String> }
fn wide(value:&str)->Vec<u16>{value.encode_utf16().chain(Some(0)).collect()}
fn gateways(key:&[u16],name:&str)->Result<Vec<String>,BridgeError>{
    let mut data=vec![0u16;2048];let mut size=(data.len()*2) as u32;
    let status=unsafe{RegGetValueW(HKEY_LOCAL_MACHINE,key.as_ptr(),wide(name).as_ptr(),RRF_RT_REG_MULTI_SZ,ptr::null_mut(),data.as_mut_ptr().cast(),&mut size)};
    if status==ERROR_FILE_NOT_FOUND{return Ok(Vec::new());}
    if status!=ERROR_SUCCESS || size%2!=0 || size<4 || size as usize>data.len()*2{return Err(invalid());}
    let data=&data[..size as usize/2];if !data.ends_with(&[0,0]){return Err(invalid());}
    data.split(|c|*c==0).filter(|v|!v.is_empty()).map(|v|{
        let value=String::from_utf16(v).map_err(|_|invalid())?;
        let ip:Ipv4Addr=value.parse().map_err(|_|invalid())?;
        if ip.is_unspecified() || ip.is_multicast() || ip.is_broadcast(){return Err(invalid());}
        Ok(ip.to_string())
    }).collect()
}
fn dhcp(guid:&str)->Result<Dhcp,BridgeError>{
    let key=wide(&format!(r"SYSTEM\CurrentControlSet\Services\Tcpip\Parameters\Interfaces\{}",windows_device::guid(guid)?));
    let mut enabled=0u32;let mut size=4;
    let status=unsafe{RegGetValueW(HKEY_LOCAL_MACHINE,key.as_ptr(),wide("EnableDHCP").as_ptr(),RRF_RT_REG_DWORD,ptr::null_mut(),(&mut enabled as *mut u32).cast(),&mut size)};
    if status!=ERROR_SUCCESS || size!=4 || enabled>1{return Err(invalid());}
    Ok(Dhcp{enabled:enabled==1,gateways:gateways(&key,"DhcpDefaultGateway")?,manual_gateways:gateways(&key,"DefaultGateway")?})
}
fn address(value:&SOCKADDR_INET)->Result<IpAddr,BridgeError>{
    unsafe{match value.si_family{
        AF_INET=>Ok(Ipv4Addr::from(value.Ipv4.sin_addr.S_un.S_addr.to_ne_bytes()).into()),
        AF_INET6=>Ok(Ipv6Addr::from(value.Ipv6.sin6_addr.u.Byte).into()),
        _=>Err(invalid()),
    }}
}
fn native_routes(guid:&str)->Result<Vec<NativeRoute>,BridgeError>{
    let canonical=windows_device::guid(guid)?;
    let raw=u128::from_str_radix(&canonical[1..37].replace('-',""),16).map_err(|_|invalid())?;
    let native=GUID::from_u128(raw);let mut luid=NET_LUID_LH::default();
    if unsafe{ConvertInterfaceGuidToLuid(&native,&mut luid)}!=NO_ERROR{return Err(invalid());}
    struct Table(*mut MIB_IPFORWARD_TABLE2);
    impl Drop for Table{fn drop(&mut self){unsafe{FreeMibTable(self.0.cast())}}}
    let mut table=ptr::null_mut();
    if unsafe{GetIpForwardTable2(AF_UNSPEC,&mut table)}!=NO_ERROR || table.is_null(){return Err(invalid());}
    let table=Table(table);let count=unsafe{(*table.0).NumEntries} as usize;
    if count>65536{return Err(invalid());}
    let rows=unsafe{std::slice::from_raw_parts(ptr::addr_of!((*table.0).Table).cast::<MIB_IPFORWARD_ROW2>(),count)};
    rows.iter().filter(|row|unsafe{row.InterfaceLuid.Value==luid.Value}).map(|row|Ok(NativeRoute{
        destination:format!("{}/{}",address(&row.DestinationPrefix.Prefix)?,row.DestinationPrefix.PrefixLength),
        next_hop:address(&row.NextHop)?.to_string(),metric:row.Metric,protocol:row.Protocol as u32,origin:row.Origin,
    })).collect()
}
fn validate(snapshot:&Snapshot,rows:&[NativeRoute],dhcp:&Dhcp)->Result<(),BridgeError>{
    // Require the same route set as the snapshot. A route added during the
    // independent native reads cannot be silently omitted from the plan.
    if rows.len()!=snapshot.active_routes.len() || rows.iter().any(|row|snapshot.active_routes.iter().filter(|r|row.matches(r)).count()!=1){return Err(invalid());}
    for row in rows{
        if snapshot.routes.iter().any(|saved|row.matches(saved)){continue;}
        let v4=row.next_hop.parse::<IpAddr>().map_err(|_|invalid())?.is_ipv4();
        let family=if v4{2}else{23};
        let interface=snapshot.interfaces.iter().find(|i|i.address_family==family).ok_or_else(invalid)?;
        let configured=snapshot.configured_dhcp.as_ref().and_then(|rows|rows.iter().find(|i|i.address_family==family)).ok_or_else(invalid)?;
        // Protocol 2 is the native local/connected route. Dynamic entries are
        // reconstructed by their enabled protocol, never persisted as manual.
        if row.protocol==2 && matches!(row.origin,0|1){continue;}
        if row.origin==2 && configured.enabled==1 {continue;}
        if row.origin==3 && !v4 && interface.router_discovery!=0 {continue;}
        // Some Windows DHCP clients label the IPv4 default route as manual.
        // Require independent lease gateway evidence and no configured gateway.
        if row.origin==0 && row.protocol==3 && row.destination=="0.0.0.0/0" && configured.enabled==1
            && row.metric==0 && dhcp.enabled && dhcp.manual_gateways.is_empty() && dhcp.gateways.contains(&row.next_hop){continue;}
        return Err(unsupported());
    }
    Ok(())
}
/// Read-only preflight, repeated immediately before membership conversion.
/// Does not convert runtime routes into saved configuration. Unsupported
/// active-only routes cause a refusal before touching the physical adapter.
pub fn validate_source(expected:&DeviceIdentity,source:&Snapshot)->Result<(),BridgeError>{
    let mut source=source.clone();source.normalize()?;source.require_restorable()?;
    if source.guid!=expected.guid || !source.instance_id.eq_ignore_ascii_case(&expected.instance_id)
        || windows_device::enumerate()?.get(&expected.guid)!=Some(expected){return Err(invalid());}
    let settings=dhcp(&expected.guid)?;let rows=native_routes(&expected.guid)?;
    validate(&source,&rows,&settings)?;
    if windows_device::enumerate()?.get(&expected.guid)!=Some(expected){return Err(invalid());}
    Ok(())
}
#[cfg(test)]
mod tests{
    use super::*;
    fn fixture()->(Snapshot,Vec<NativeRoute>,Dhcp){
        let mut source:Snapshot=serde_json::from_str(include_str!("../../../tests/fixtures/windows-dhcp-profile.json")).unwrap();source.normalize().unwrap();
        source.configured_dhcp=Some(source.interfaces.iter().map(|i|crate::windows_profile::ConfiguredDhcp{address_family:i.address_family,enabled:i.dhcp}).collect());
        let rows=source.active_routes.iter().map(|row|NativeRoute{destination:row.destination_prefix.clone(),next_hop:row.next_hop.clone(),metric:row.route_metric,protocol:row.protocol,
            origin:if row.protocol==2 || !row.next_hop.contains(':'){0}else{3}}).collect();
        let gateway=source.active_routes.iter().find(|r|r.destination_prefix=="0.0.0.0/0").unwrap().next_hop.clone();
        (source,rows,Dhcp{enabled:true,gateways:vec![gateway],manual_gateways:Vec::new()})
    }
    #[test]
    fn native_manual_origin_does_not_make_a_dhcp_default_persistent(){
        let (source,rows,mut dhcp)=fixture();validate(&source,&rows,&dhcp).unwrap();
        let mut edited=source.clone();let mut edited_rows=rows.clone();
        edited.active_routes.iter_mut().find(|r|r.destination_prefix=="0.0.0.0/0").unwrap().route_metric=42;
        edited_rows.iter_mut().find(|r|r.destination=="0.0.0.0/0").unwrap().metric=42;
        assert!(validate(&edited,&edited_rows,&dhcp).is_err());
        dhcp.enabled=false;assert!(validate(&source,&rows,&dhcp).is_err());
        dhcp.enabled=true;dhcp.manual_gateways=dhcp.gateways.clone();assert!(validate(&source,&rows,&dhcp).is_err());
        dhcp.manual_gateways.clear();dhcp.gateways.clear();assert!(validate(&source,&rows,&dhcp).is_err());
    }
    #[test]
    fn temporary_routes_and_changed_native_evidence_are_rejected(){
        let (mut source,mut rows,dhcp)=fixture();
        let row=Route{destination_prefix:"198.51.100.0/24".into(),next_hop:dhcp.gateways[0].clone(),route_metric:42,protocol:3,publish:0,valid_lifetime_ticks:None,preferred_lifetime_ticks:None};
        rows.push(NativeRoute{destination:row.destination_prefix.clone(),next_hop:row.next_hop.clone(),metric:row.route_metric,protocol:3,origin:0});
        assert!(validate(&source,&rows,&dhcp).is_err());source.active_routes.push(row.clone());
        assert!(validate(&source,&rows,&dhcp).is_err());source.routes.push(row);validate(&source,&rows,&dhcp).unwrap();
        rows.last_mut().unwrap().metric+=1;assert!(validate(&source,&rows,&dhcp).is_err());
    }
    #[test]
    fn advertised_routes_require_enabled_discovery(){
        let (mut source,rows,dhcp)=fixture();source.interfaces.iter_mut().find(|i|i.address_family==23).unwrap().router_discovery=0;
        assert!(validate(&source,&rows,&dhcp).is_err());
    }
    #[test]
    #[ignore = "requires a native adapter with only restorable route sources"]
    fn live_physical_routes_have_verified_sources(){
        let adapter=crate::inspect().unwrap().adapters.into_iter().find(|a|a.eligible).unwrap();
        let devices=windows_device::enumerate().unwrap();let device=devices.get(adapter.id.as_str()).unwrap();
        let source=Snapshot::read(device).unwrap();validate_source(device,&source).unwrap();
    }
}
