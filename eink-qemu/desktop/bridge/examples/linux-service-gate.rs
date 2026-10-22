//! Ordinary-user IPC gate for the installed service in the disposable VM.
#[cfg(target_os = "linux")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use eink_bridge::{model::*, protocol::*, transport::HelperClient};
    let args = std::env::args().collect::<Vec<_>>();
    if args.len() != 3 || !matches!(args[2].as_str(), "eink-bridge-test" | "eink-bridge-noble-x64") || unsafe { libc::geteuid() } != 1000
        || std::fs::read_to_string("/etc/hostname")?.trim() != args[2] { return Err("requires the ordinary disposable VM account".into()); }
    let request = |request| HelperClient::connect()?.request(HelperRequest { version: VERSION, request });
    let ReplyKind::Inspection { inspection } = request(RequestKind::Inspect)?.reply else { panic!("missing inspection"); };
    assert_eq!(inspection.support, SupportState::Ready);
    if args[1] == "--inspect" { println!("ordinaryServiceInspection=passed"); return Ok(()); }
    if args[1] == "--verify-removed" || args[1] == "--verify-restored" {
        assert!(inspection.selection.is_none()); assert_eq!(inspection.readiness, Readiness::NeedsSetup);
        if args[1] == "--verify-restored" {
            let ReplyKind::Operation { operation: Some(operation) } = request(RequestKind::Recover)?.reply else { panic!("missing recovered operation"); };
            assert_eq!(operation.state, OperationState::Cancelled, "{operation:?}");
            assert!(operation.error.is_none());
            println!("serviceRestoredUnconfirmedOperation=passed");
        }
        println!("serviceHasNoConfiguredResource=passed"); return Ok(());
    }
    if args[1] == "--busy-remove" {
        let resource_id = inspection.selection.and_then(|s| s.resource_id).ok_or("missing resource")?;
        let error = request(RequestKind::Plan { request: PlanRequest::Remove { resource_id } }).expect_err("leased resource must refuse removal");
        assert_eq!(error.code, "EBUSY");
        println!("survivingClientLeasePreventsRemoval=passed"); return Ok(());
    }
    if args[1] == "--verify" || args[1] == "--hold-lease" {
        let selection = inspection.selection.ok_or("missing resource")?;
        assert!(matches!(request(RequestKind::Verify { selection: selection.clone() })?.reply, ReplyKind::Verification { readiness: Readiness::Ready, .. }));
        let lease = HelperClient::acquire(selection)?;
        assert!(matches!(lease.target, LaunchTarget::LinuxBridge { .. }));
        if args[1] == "--hold-lease" {
            use std::io::Write;
            println!("leaseHolderReady={}", std::process::id());
            std::io::stdout().flush()?;
            std::thread::sleep(std::time::Duration::from_secs(90));
            drop(lease);
            println!("leaseHolderReleased=passed");
        }
        println!("ordinaryServiceVerificationAndTransferredLease=passed"); return Ok(());
    }
    let intent = match args[1].as_str() {
        "--denied" | "--configure" | "--unconfirmed" => PlanRequest::Configure { adapter_id: AdapterId::new("enp0s1")? },
        "--remove" => PlanRequest::Remove { resource_id: inspection.selection.and_then(|s| s.resource_id).ok_or("missing resource")? },
        _ => return Err("unknown service gate mode".into()),
    };
    let ReplyKind::Plan { plan } = request(RequestKind::Plan { request: intent })?.reply else { panic!("missing plan"); };
    let result = request(RequestKind::Apply { plan_id: plan.id.clone(), authorization: None });
    if args[1] == "--denied" {
        let error = result.expect_err("missing desktop authorization must be denied");
        assert!(matches!(error.code.as_str(), "EAUTHDENIED" | "EAUTHUNAVAILABLE" | "EAUTHCANCELLED"));
        request(RequestKind::Cancel { operation_id: plan.id })?;
        println!("unauthorizedApplyDenied={}", error.code); return Ok(());
    }
    let ReplyKind::Operation { operation: Some(operation) } = result?.reply else { panic!("missing operation"); };
    if operation.state != OperationState::Verifying { return Err(format!("unexpected result: {operation:?}").into()); }
    if args[1] == "--unconfirmed" { println!("ordinaryClientExitedBeforeKeep=passed"); return Ok(()); }
    let ReplyKind::Operation { operation: Some(operation) } = request(RequestKind::Keep { operation_id: plan.id })?.reply else { panic!("missing Keep result"); };
    if operation.state != OperationState::Ready { return Err(format!("Keep failed: {operation:?}").into()); }
    println!("ordinaryAuthorizedServiceOperation=passed");
    Ok(())
}
#[cfg(not(target_os = "linux"))]
fn main() { panic!("Linux only"); }
