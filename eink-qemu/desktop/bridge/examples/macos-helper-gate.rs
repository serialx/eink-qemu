//! Opt-in acceptance in a clean disposable VirtualBuddy guest, never the host.
#[cfg(target_os = "macos")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use eink_bridge::{authorization::macos::Authorization, macos_service::*, model::*, protocol::*, transport::HelperClient};
    use std::{path::Path, process::{Command, Child}, os::unix::process::CommandExt, time::{Instant, Duration}};
    let args = std::env::args().collect::<Vec<_>>();
    let request = |request| HelperClient::connect()?.request(HelperRequest { version: VERSION, request });
    if args.len() == 2 && args[1] == "--client" {
        assert_eq!(unsafe { libc::getuid() }, 501);
        assert_eq!(unsafe { libc::geteuid() }, 501);
        assert!(matches!(request(RequestKind::Inspect)?.reply, ReplyKind::Inspection { .. }));
        let ReplyKind::Plan { plan } = request(RequestKind::Plan { request: PlanRequest::Configure { adapter_id: AdapterId::new("en0")? } })?.reply else { panic!("missing plan"); };
        assert_eq!(request(RequestKind::Apply { plan_id: plan.id.clone(), authorization: None }).unwrap_err().code, "EAUTHDENIED");
        let authorization = Authorization::empty()?;
        assert_eq!(request(RequestKind::Apply { plan_id: plan.id.clone(), authorization: Some(authorization.proof()?) }).unwrap_err().code, "EAUTHDENIED");
        let ReplyKind::Operation { operation: Some(operation) } = request(RequestKind::Recover)?.reply else { panic!("missing operation"); };
        assert_eq!(operation.id, plan.id); assert_eq!(operation.state, OperationState::AwaitingAuthorization);
        let ReplyKind::Inspection { inspection } = request(RequestKind::Inspect)?.reply else { panic!("missing inspection"); };
        assert!(inspection.selection.is_none()); assert_ne!(inspection.readiness, Readiness::Ready);
        let ReplyKind::Operation { operation: Some(operation) } = request(RequestKind::Cancel { operation_id: plan.id })?.reply else { panic!("missing cancellation"); };
        assert_eq!(operation.state, OperationState::Cancelled);
        let mut malformed = eink_bridge::transport::unix::connect()?;
        std::io::Write::write_all(&mut malformed, &[0, 16, 0, 1])?;
        let mut byte = [0];
        assert_eq!(std::io::Read::read(&mut malformed, &mut byte)?, 0);
        assert!(matches!(request(RequestKind::Inspect)?.reply, ReplyKind::Inspection { .. }));
        println!("ordinaryCallerInspectionAndPlan=passed\nmissingAuthorizationRefused=passed\nunprivilegedAuthorizationRefused=passed\npollDoesNotApplyOrRecover=passed\nmalformedClientIsolation=passed");
        return Ok(());
    }
    if args.len() != 5 || args[1] != "--exercise" { return Err("requires --exercise exact-hostname helper pinned-payload".into()); }
    let mut name = [0i8; 256];
    if unsafe { libc::gethostname(name.as_mut_ptr(), name.len()) } != 0 || unsafe { std::ffi::CStr::from_ptr(name.as_ptr()) }.to_str()? != args[2] { return Err("wrong disposable host".into()); }
    let model = Command::new("/usr/sbin/sysctl").args(["-n", "hw.model"]).output()?;
    if !String::from_utf8(model.stdout)?.starts_with("VirtualMac") { return Err("requires a disposable VirtualMac".into()); }
    if Path::new(INSTALL_ROOT).exists() || Path::new(SOCKET_ROOT).join("bridge.sock").exists() { return Err("preexisting helper test resources".into()); }
    let native = NativeService::new()?;
    native.install_payload(&std::fs::read(&args[4])?)?;
    struct Service(Child);
    impl Drop for Service { fn drop(&mut self) { let _ = self.0.kill(); let _ = self.0.wait(); } }
    let mut helper = Service(Command::new(&args[3]).arg("--serve").spawn()?);
    let deadline = Instant::now() + Duration::from_secs(15);
    while !Path::new(SOCKET_ROOT).join("bridge.sock").exists() {
        if helper.0.try_wait()?.is_some() || Instant::now() >= deadline { return Err("helper did not become available".into()); }
        std::thread::sleep(Duration::from_millis(50));
    }
    let mut client = Command::new(std::env::current_exe()?);
    client.arg("--client");
    unsafe { client.pre_exec(|| {
        if libc::setgroups(1, &20) != 0 || libc::setgid(20) != 0 || libc::setuid(501) != 0 { return Err(std::io::Error::last_os_error()); }
        Ok(())
    }); }
    if !client.status()?.success() { return Err("ordinary client test failed; evidence preserved".into()); }
    drop(helper);
    let record: serde_json::Value = serde_json::from_slice(&std::fs::read(Path::new(JOURNAL_ROOT).join("operation.json"))?)?;
    assert_eq!(record["undo_required"], false); assert_eq!(record["resources"], serde_json::json!([]));
    let id = record["prepared"]["plan"]["id"].as_str().ok_or("missing plan identity")?;
    assert!(!Path::new("/Library/LaunchDaemons").join(format!("net.serialx.eink-qemu.bridge.u501.{id}.plist")).exists());
    assert!(!Path::new(SOCKET_ROOT).join("u501").exists());
    for name in ["operation.json", "operation.lock", "service.lock"] { std::fs::remove_file(Path::new(JOURNAL_ROOT).join(name))?; }
    std::fs::remove_dir(JOURNAL_ROOT)?;
    std::fs::remove_file(Path::new(INSTALL_ROOT).join("socket_vmnet"))?;
    std::fs::remove_dir(INSTALL_ROOT)?;
    std::fs::remove_file(Path::new(SOCKET_ROOT).join("bridge.sock"))?;
    std::fs::remove_dir(SOCKET_ROOT)?;
    println!("authorizationDenialLeftNoInstalledResource=passed");
    Ok(())
}
#[cfg(not(target_os = "macos"))]
fn main() { panic!("macOS only"); }
