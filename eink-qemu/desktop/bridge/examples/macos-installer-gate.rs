//! Run from a signed test .app in a disposable VirtualMac. System prompts must
//! be approved/cancelled by the operator; no credentials enter this program.
#[cfg(target_os = "macos")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use eink_bridge::{authorization::macos::{authorize_install, Authorization}, model::*, protocol::*, transport::HelperClient};
    let args = std::env::args().collect::<Vec<_>>();
    if args.len() != 3 { return Err("requires --install/--configure/--verify/--verify-removed/--remove exact-hostname".into()); }
    let mut name = [0i8; 256];
    if unsafe { libc::gethostname(name.as_mut_ptr(), name.len()) } != 0 || unsafe { std::ffi::CStr::from_ptr(name.as_ptr()) }.to_str()? != args[2] { return Err("wrong disposable host".into()); }
    let model = std::process::Command::new("/usr/sbin/sysctl").args(["-n", "hw.model"]).output()?;
    if !String::from_utf8(model.stdout)?.starts_with("VirtualMac") || unsafe { libc::geteuid() } == 0 { return Err("requires an ordinary user in a disposable VirtualMac".into()); }
    if args[1] == "--install" {
        let result = authorize_install();
        println!("installation={result:?}");
        result?;
        return Ok(());
    }
    let request = |request| HelperClient::connect()?.request(HelperRequest { version: VERSION, request });
    let ReplyKind::Inspection { inspection } = request(RequestKind::Inspect)?.reply else { panic!("missing inspection"); };
    if args[1] == "--verify-removed" {
        assert!(inspection.selection.is_none());
        assert_eq!(inspection.readiness, Readiness::NeedsSetup);
        println!("removedResourceAbsent=passed");
        return Ok(());
    }
    if args[1] == "--verify" {
        println!("readiness={:?}", inspection.readiness);
        assert_eq!(inspection.readiness, Readiness::Ready);
        return Ok(());
    }
    let intent = match args[1].as_str() {
        "--configure" => PlanRequest::Configure { adapter_id: AdapterId::new("en0")? },
        "--remove" => PlanRequest::Remove { resource_id: inspection.selection.and_then(|selection| selection.resource_id).ok_or("no configured test resource")? },
        _ => return Err("unknown gate operation".into()),
    };
    let ReplyKind::Plan { plan } = request(RequestKind::Plan { request: intent.clone() })?.reply else { panic!("missing plan"); };
    println!("reviewedChanges={:?}", plan.changes);
    let authorization = Authorization::request()?;
    // A second reference cannot borrow the just-approved credential. The
    // transferred original must still pass the exact helper-side right check.
    let empty = Authorization::empty()?;
    assert!(Authorization::verify(&empty.proof()?).is_err());
    println!("unrelatedReferenceDenied=passed");
    // A human can take longer than the review TTL to answer the system prompt.
    // Refresh only this disposable-host test's plan, and require every reviewed
    // field to remain identical. Production expiration and authorization checks
    // remain unchanged.
    let ReplyKind::Plan { plan: refreshed } = request(RequestKind::Plan { request: intent })?.reply else { panic!("missing refreshed plan"); };
    assert_eq!(plan.request, refreshed.request);
    assert_eq!(plan.fingerprint, refreshed.fingerprint);
    assert_eq!(plan.changes, refreshed.changes);
    assert_eq!(plan.requires_authorization, refreshed.requires_authorization);
    assert_eq!(plan.may_disconnect, refreshed.may_disconnect);
    let plan = refreshed;
    let result = request(RequestKind::Apply { plan_id: plan.id, authorization: Some(authorization.proof()?) })?;
    let ReplyKind::Operation { operation: Some(operation) } = result.reply else { panic!("missing operation"); };
    println!("operation={operation:?}");
    assert_eq!(operation.state, OperationState::Ready);
    Ok(())
}
#[cfg(not(target_os = "macos"))]
fn main() { panic!("macOS only"); }
