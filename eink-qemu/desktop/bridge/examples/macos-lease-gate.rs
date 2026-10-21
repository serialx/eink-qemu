//! Restart only the installed test helper while an ordinary client holds its
//! transferred lease. Requires passwordless sudo in the disposable VM.
#[cfg(target_os = "macos")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use eink_bridge::{model::*, protocol::*, transport::HelperClient};
    use std::{process::Command, time::{Duration, Instant}};
    let args = std::env::args().collect::<Vec<_>>();
    let mut name = [0 as libc::c_char; 256];
    if args.len() != 2 || unsafe { libc::geteuid() } != 501 || unsafe { libc::gethostname(name.as_mut_ptr(), name.len()) } != 0
        || unsafe { std::ffi::CStr::from_ptr(name.as_ptr()) }.to_str()? != args[1] { return Err("requires exact disposable hostname and ordinary UID501".into()); }
    let model = Command::new("/usr/sbin/sysctl").args(["-n", "hw.model"]).output()?;
    if !String::from_utf8(model.stdout)?.starts_with("VirtualMac") { return Err("requires VirtualMac".into()); }
    let request = |request| HelperClient::connect()?.request(HelperRequest { version: VERSION, request });
    let ReplyKind::Inspection { inspection } = request(RequestKind::Inspect)?.reply else { panic!("missing inspection"); };
    assert_eq!(inspection.readiness, Readiness::Ready);
    let selection = inspection.selection.ok_or("no installed test bridge")?;
    let id = selection.resource_id.clone().ok_or("no managed identity")?;
    let lease = HelperClient::acquire(selection.clone())?;
    assert_eq!(request(RequestKind::Plan { request: PlanRequest::Remove { resource_id: id.clone() } }).unwrap_err().code, "EBUSY");
    if !Command::new("/usr/bin/sudo").args(["-n", "/bin/launchctl", "kickstart", "-k", "system/net.serialx.eink-qemu.bridge-helper"]).status()?.success() { return Err("helper restart failed".into()); }
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        match request(RequestKind::Inspect) {
            Ok(reply) => { let ReplyKind::Inspection { inspection } = reply.reply else { panic!("missing inspection"); }; assert_eq!(inspection.readiness, Readiness::Ready); break; },
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(100)),
            Err(error) => return Err(error.into()),
        }
    }
    assert_eq!(request(RequestKind::Plan { request: PlanRequest::Remove { resource_id: id.clone() } }).unwrap_err().code, "EBUSY");
    println!("helperRestartRetainsOrdinaryClientLease=passed");
    drop(lease);
    let ReplyKind::Plan { plan } = request(RequestKind::Plan { request: PlanRequest::Remove { resource_id: id } })?.reply else { panic!("missing removal plan"); };
    request(RequestKind::Cancel { operation_id: plan.id })?;
    assert!(matches!(request(RequestKind::Verify { selection })?.reply, ReplyKind::Verification { readiness: Readiness::Ready, .. }));
    println!("clientExitUnblocksRemovalPlanning=passed");
    Ok(())
}
#[cfg(not(target_os = "macos"))]
fn main() { panic!("macOS only"); }
