//! Read-only DNS configuration evidence, including adapters with no IP binding.
#[cfg(not(windows))]
fn main(){panic!("Requires the disposable Windows VM");}
#[cfg(windows)]
fn main(){if let Err(error)=run(){eprintln!("{error:?}");std::process::exit(1);}}
#[cfg(windows)]
fn run()->Result<(),Box<dyn std::error::Error>>{
    use eink_bridge::windows_device;
    use windows_sys::{core::GUID,Win32::NetworkManagement::IpHelper::*};
    if std::env::var("COMPUTERNAME").as_deref()!=Ok("WIN-SUNSKHPNVMO"){return Err("Wrong disposable host".into());}
    fn string(value:*const u16)->Result<Option<String>,Box<dyn std::error::Error>>{
        if value.is_null(){return Ok(None);}
        for length in 0..65536{
            if unsafe{*value.add(length)}==0{return Ok(Some(String::from_utf16(unsafe{std::slice::from_raw_parts(value,length)})?));}
        }
        Err("DNS setting exceeds the supported length".into())
    }
    struct Settings(DNS_INTERFACE_SETTINGS);
    impl Drop for Settings{fn drop(&mut self){unsafe{FreeInterfaceDnsSettings(&mut self.0)}}}
    let mut output=Vec::new();
    for device in windows_device::enumerate()?.into_values(){
        let guid=GUID::from_u128(u128::from_str_radix(&device.guid[1..37].replace('-',""),16)?);
        for flags in [0,DNS_SETTING_IPV6 as u64]{
            let mut settings=DNS_INTERFACE_SETTINGS{Version:DNS_INTERFACE_SETTINGS_VERSION1,Flags:flags,..Default::default()};
            let status=unsafe{GetInterfaceDnsSettings(guid,&mut settings)};
            if status!=0{output.push(serde_json::json!({"guid":device.guid,"family":if flags==0{2}else{23},"status":status}));continue;}
            let settings=Settings(settings);let s=&settings.0;
            output.push(serde_json::json!({"guid":device.guid,"family":if flags==0{2}else{23},"status":status,"flags":s.Flags,
                "domain":string(s.Domain)?,"nameServer":string(s.NameServer)?,"searchList":string(s.SearchList)?,"profileNameServer":string(s.ProfileNameServer)?,
                "registration":s.RegistrationEnabled,"registerAdapterName":s.RegisterAdapterName,"llmnr":s.EnableLLMNR,"queryAdapterName":s.QueryAdapterName}));
        }
    }
    println!("{}",serde_json::to_string_pretty(&output)?);Ok(())
}
