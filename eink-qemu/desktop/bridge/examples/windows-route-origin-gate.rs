//! Read-only native route-origin evidence from the disposable Windows VM.
#[cfg(not(windows))]
fn main(){panic!("Requires the disposable Windows VM");}
#[cfg(windows)]
fn main(){if let Err(error)=run(){eprintln!("{error:?}");std::process::exit(1);}}
#[cfg(windows)]
fn run()->Result<(),Box<dyn std::error::Error>>{
    use std::{net::{Ipv4Addr,Ipv6Addr},ptr};
    use windows_sys::Win32::{NetworkManagement::IpHelper::*,Networking::WinSock::*};
    if std::env::var("COMPUTERNAME").as_deref()!=Ok("WIN-SUNSKHPNVMO"){return Err("Wrong disposable host".into());}
    fn address(value:&SOCKADDR_INET)->Result<String,Box<dyn std::error::Error>>{
        unsafe{match value.si_family{
            AF_INET=>Ok(Ipv4Addr::from(value.Ipv4.sin_addr.S_un.S_addr.to_ne_bytes()).to_string()),
            AF_INET6=>Ok(Ipv6Addr::from(value.Ipv6.sin6_addr.u.Byte).to_string()),
            _=>Err("Unsupported address family".into()),
        }}
    }
    struct Table(*mut MIB_IPFORWARD_TABLE2);
    impl Drop for Table{fn drop(&mut self){unsafe{FreeMibTable(self.0.cast())}}}
    let mut value=ptr::null_mut();let status=unsafe{GetIpForwardTable2(AF_UNSPEC,&mut value)};
    if status!=0 || value.is_null(){return Err(format!("GetIpForwardTable2 failed: {status}").into());}
    let table=Table(value);let count=unsafe{(*table.0).NumEntries} as usize;
    if count>65536{return Err("Unexpected route table size".into());}
    let rows=unsafe{std::slice::from_raw_parts(ptr::addr_of!((*table.0).Table).cast::<MIB_IPFORWARD_ROW2>(),count)};
    let mut output=Vec::new();
    for row in rows{
        output.push(serde_json::json!({"index":row.InterfaceIndex,"destination":format!("{}/{}",address(&row.DestinationPrefix.Prefix)?,row.DestinationPrefix.PrefixLength),
            "nextHop":address(&row.NextHop)?,"metric":row.Metric,"protocol":row.Protocol,"origin":row.Origin,"validLifetime":row.ValidLifetime,"preferredLifetime":row.PreferredLifetime,"immortal":row.Immortal}));
    }
    println!("{}",serde_json::to_string_pretty(&output)?);Ok(())
}
