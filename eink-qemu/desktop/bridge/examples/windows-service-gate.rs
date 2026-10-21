//! Client-side smoke and UAC gates for the real installed helper in the VM.
#[cfg(not(windows))]
fn main() { panic!("Requires the disposable Windows VM"); }
#[cfg(windows)]
fn main() { if let Err(error)=run(){ eprintln!("{error:?}"); std::process::exit(1); } }
#[cfg(windows)]
fn run()->Result<(),Box<dyn std::error::Error>> {
    use eink_bridge::{model::*,protocol::*,transport::HelperClient};
    let args=std::env::args().collect::<Vec<_>>();
    if std::env::var("COMPUTERNAME").as_deref()!=Ok("WIN-SUNSKHPNVMO") || args.len()!=2 {
        return Err("Requires the disposable Windows test VM".into());
    }
    if args[1]!="--inspect" {
        use std::os::windows::io::{AsRawHandle,FromRawHandle,OwnedHandle};
        use windows_sys::Win32::{Security::TOKEN_QUERY,System::Threading::{OpenProcessToken,GetCurrentProcess}};
        let mut token=std::ptr::null_mut();
        if unsafe{OpenProcessToken(GetCurrentProcess(),TOKEN_QUERY,&mut token)}==0{return Err(std::io::Error::last_os_error().into());}
        let token=unsafe{OwnedHandle::from_raw_handle(token)};
        assert_ne!(eink_bridge::windows_security::current_sid()?,"S-1-5-18");
        assert!(!eink_bridge::windows_security::token_is_elevated_admin(token.as_raw_handle())?,"Approval gates require an unelevated user token");
        println!("unelevatedClientToken=passed");
    }
    let request=|request|HelperClient::connect()?.request(HelperRequest{version:VERSION,request});
    let ReplyKind::Inspection{inspection}=request(RequestKind::Inspect)?.reply else{return Err("Missing inspection".into())};
    assert_eq!(inspection.support,SupportState::Ready);
    assert!(inspection.capabilities.create);
    assert!(!inspection.capabilities.existing_bridge);
    assert!(inspection.selection.is_none());
    println!("installedServiceInspection=passed");
    if args[1]=="--inspect" {return Ok(());}
    if !matches!(args[1].as_str(),"--denied"|"--authorize-only"|"--cancel-approval") {return Err("Unknown mode".into());}
    let adapter_id=AdapterId::new("{5238c509-ecda-49fb-bfdc-fd0be1922ff5}")?;
    let ReplyKind::Plan{plan}=request(RequestKind::Plan{request:PlanRequest::Configure{adapter_id}})?.reply else{return Err("Missing plan".into())};
    let result=(||->Result<(),Box<dyn std::error::Error>>{
        let error=request(RequestKind::Apply{plan_id:plan.id.clone(),authorization:None}).expect_err("Missing approval must be refused");
        assert_eq!(error.code,"EAUTHDENIED");
        println!("installedServiceUnauthorizedApply=passed");
        if args[1]!="--denied" {
            let proof=eink_bridge::windows_elevation::request(plan.id.clone());
            if args[1]=="--cancel-approval" {
                assert_eq!(proof.expect_err("UAC cancellation must fail").code,"EAUTHCANCELLED");
                println!("installedServiceUacCancellation=passed");
            } else {
                proof?;
                println!("installedServiceUacRoundTrip=passed");
            }
        }
        Ok(())
    })();
    // No gate applies an authorized plan. This validates the real service and
    // approval flow without changing host network state.
    let deadline=std::time::Instant::now()+std::time::Duration::from_secs(15);
    loop {
        match request(RequestKind::Cancel{operation_id:plan.id.clone()}) {
            Ok(_)=>break,
            Err(error) if error.code=="EBUSY" && std::time::Instant::now()<deadline=>
                std::thread::sleep(std::time::Duration::from_millis(250)),
            Err(error)=>return Err(error.into()),
        }
    }
    result
}
