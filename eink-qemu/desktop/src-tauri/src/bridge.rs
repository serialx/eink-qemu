//! Native desktop bridge control. The WebView provides typed intents only;
//! helper authorization, selected resources and launch targets stay in Rust.
use super::App;
use eink_bridge::{model::*, protocol::{HelperRequest, ReplyKind, RequestKind}, transport::{HelperClient, TransportLease}};
use eink_core::{network::{NetworkOverrides, NetworkTarget}, session::WifiMode};
use tauri::{Emitter, State, WebviewWindow};

#[derive(Default)]
pub(crate) struct Control { pub gate: tokio::sync::Mutex<()> }

pub(crate) fn check_window(label: &str) -> Result<(), BridgeError> {
    if label != "main" { return Err(BridgeError::new("EPERM", "Bridge setup is available only in the main window.")); }
    Ok(())
}
fn check_idle(state: &str) -> Result<(), BridgeError> {
    if state != "idle" { return Err(BridgeError::new("EBUSY", "Stop the emulator before changing bridge setup.")); }
    Ok(())
}
fn check_apply(operation: Option<&Operation>, id: &OperationId) -> Result<(), BridgeError> {
    let operation = operation.filter(|operation| operation.id == *id)
        .ok_or_else(|| BridgeError::new("ESTALE", "Review bridge setup again before applying it."))?;
    if operation.state != OperationState::AwaitingAuthorization {
        return Err(BridgeError::new("EBUSY", "This bridge operation has already started."));
    }
    Ok(())
}
fn needs_managed(wifi: bool, mode: WifiMode, overrides: &NetworkOverrides) -> bool {
    wifi && mode == WifiMode::Bridge && overrides.net.is_none() && overrides.bridge.as_ref().is_none_or(String::is_empty)
}
fn local_override() -> Option<String> {
    if std::env::var_os("X4NET").is_some() { Some("X4NET".into()) }
    else if std::env::var_os("X4BR").is_some_and(|v| !v.is_empty()) { Some("X4BR".into()) } else { None }
}
fn helper(request: RequestKind) -> Result<ReplyKind, BridgeError> {
    HelperClient::connect()?.request(HelperRequest { version: VERSION, request }).map(|reply| reply.reply)
}
fn protocol() -> BridgeError { BridgeError::new("EPROTOCOL", "The bridge helper returned an unexpected response.") }
async fn blocking<T: Send + 'static>(action: impl FnOnce() -> Result<T, BridgeError> + Send + 'static) -> Result<T, BridgeError> {
    tauri::async_runtime::spawn_blocking(action).await.map_err(|error| BridgeError::new("EIPC", error.to_string()))?
}
fn inspect(saved: Option<BridgeSelection>) -> Result<Inspection, BridgeError> {
    let mut inspection = match helper(RequestKind::Inspect) {
        Ok(ReplyKind::Inspection { inspection }) => inspection,
        Ok(_) => return Err(protocol()),
        Err(error) if error.code == "ENEEDSSETUP" || error.code == "EPROTOCOL" => {
            let mut inspection = eink_bridge::inspect()?;
            inspection.support = if error.code == "EPROTOCOL" { SupportState::NeedsUpdate } else { SupportState::Missing };
            inspection
        },
        Err(error) => return Err(error),
    };
    inspection.external_override = local_override();
    // A saved selection wins over the helper's default most-recent resource.
    // After removal it may be stale; verification will clear it explicitly.
    if let Some(selection) = saved {
        inspection.readiness = selection_readiness(&selection, inspection.platform, &inspection.adapters);
        if inspection.support == SupportState::Ready && inspection.readiness == Readiness::NeedsSetup {
            inspection.readiness = match helper(RequestKind::Verify { selection: selection.clone() }) {
                Ok(ReplyKind::Verification { readiness, .. }) => readiness,
                Ok(_) => return Err(protocol()),
                Err(error) if error.code == "EDISCONNECTED" => Readiness::Disconnected,
                Err(error) if error.code == "EBUSY" => Readiness::NeedsSetup,
                Err(_) => Readiness::NeedsRepair,
            };
        }
        inspection.selection = Some(selection);
    }
    Ok(inspection)
}
fn emit(app: &App, operation: &Operation) { let _ = app.events.app.emit_to("main", "bridge-operation", operation); }
fn operation(reply: ReplyKind) -> Result<Operation, BridgeError> {
    if let ReplyKind::Operation { operation: Some(operation) } = reply { Ok(operation) } else { Err(protocol()) }
}

#[tauri::command]
pub(crate) async fn bridge_inspect(window: WebviewWindow, app: State<'_, App>) -> Result<Inspection, BridgeError> {
    check_window(window.label())?;
    let saved = app.settings.lock().unwrap().bridge.clone();
    blocking(move || inspect(saved)).await
}
#[tauri::command]
pub(crate) async fn bridge_install(window: WebviewWindow, app: State<'_, App>) -> Result<Inspection, BridgeError> {
    check_window(window.label())?;
    let _gate = app.bridge.gate.lock().await;
    check_idle(app.session.info().state)?;
    blocking(|| {
        #[cfg(target_os = "macos")]
        eink_bridge::authorization::macos::authorize_install()?;
        #[cfg(not(target_os = "macos"))]
        return Err(BridgeError::new("EINSTALL", "Install bridge support from the desktop package for your operating system."));
        #[cfg(target_os = "macos")] {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
            loop {
                match inspect(None) {
                    Ok(inspection) if inspection.support == SupportState::Ready => return Ok(inspection),
                    _ if std::time::Instant::now() < deadline => std::thread::sleep(std::time::Duration::from_millis(200)),
                    _ => return Err(BridgeError::new("EINSTALL", "Bridge support was installed but did not start. Reopen setup to retry.")),
                }
            }
        }
    }).await
}
#[tauri::command]
pub(crate) async fn bridge_plan(window: WebviewWindow, app: State<'_, App>, request: PlanRequest) -> Result<BridgePlan, BridgeError> {
    check_window(window.label())?;
    let _gate = app.bridge.gate.lock().await;
    check_idle(app.session.info().state)?;
    if !matches!(request, PlanRequest::Remove { .. }) && local_override().is_some() {
        return Err(BridgeError::new("EOVERRIDE", "An inherited network override controls this launch. Remove it before configuring bridge setup."));
    }
    blocking(move || match helper(RequestKind::Plan { request })? { ReplyKind::Plan { plan } => Ok(plan), _ => Err(protocol()) }).await
}
#[tauri::command]
pub(crate) async fn bridge_apply(window: WebviewWindow, app: State<'_, App>, plan_id: OperationId) -> Result<Operation, BridgeError> {
    check_window(window.label())?;
    let _gate = app.bridge.gate.lock().await;
    check_idle(app.session.info().state)?;
    let result = blocking(move || {
        let ReplyKind::Operation { operation: current } = helper(RequestKind::Recover)? else { return Err(protocol()); };
        check_apply(current.as_ref(), &plan_id)?;
        #[cfg(target_os = "macos")]
        let grant = eink_bridge::authorization::macos::Authorization::request()?;
        #[cfg(target_os = "macos")]
        let authorization = Some(grant.proof()?);
        #[cfg(not(target_os = "macos"))]
        let authorization = None;
        operation(helper(RequestKind::Apply { plan_id, authorization })?)
    }).await?;
    emit(&app, &result); Ok(result)
}
#[tauri::command]
pub(crate) async fn bridge_keep(window: WebviewWindow, app: State<'_, App>, operation_id: OperationId) -> Result<Operation, BridgeError> {
    check_window(window.label())?;
    let _gate = app.bridge.gate.lock().await;
    check_idle(app.session.info().state)?;
    let result = blocking(move || operation(helper(RequestKind::Keep { operation_id })?)).await?;
    emit(&app, &result); Ok(result)
}
#[tauri::command]
pub(crate) async fn bridge_cancel(window: WebviewWindow, app: State<'_, App>, operation_id: OperationId) -> Result<Operation, BridgeError> {
    check_window(window.label())?;
    // Cancellation/restoration remains available while an Apply request is in
    // flight. The helper serializes mutations and keeps the recovery watchdog.
    let result = blocking(move || operation(helper(RequestKind::Cancel { operation_id })?)).await?;
    emit(&app, &result); Ok(result)
}
#[tauri::command]
pub(crate) async fn bridge_operation(window: WebviewWindow) -> Result<Option<Operation>, BridgeError> {
    check_window(window.label())?;
    blocking(|| match helper(RequestKind::Recover) {
        Ok(ReplyKind::Operation { operation }) => Ok(operation),
        Ok(_) => Err(protocol()),
        Err(error) if error.code == "ENEEDSSETUP" => Ok(None),
        Err(error) => Err(error),
    }).await
}
#[tauri::command]
pub(crate) async fn bridge_verify(window: WebviewWindow, app: State<'_, App>) -> Result<Inspection, BridgeError> {
    check_window(window.label())?;
    let _gate = app.bridge.gate.lock().await;
    check_idle(app.session.info().state)?;
    // Use the helper's protected registry after configuration/removal, not the
    // stale saved selection or a selection supplied by the WebView.
    let inspection = blocking(|| inspect(None)).await?;
    if inspection.active_operation.as_ref().is_some_and(|operation| !matches!(operation.state, OperationState::Ready | OperationState::Cancelled)) {
        return Err(BridgeError::new("EVERIFY", "Finish or restore the current bridge operation before saving."));
    }
    if inspection.selection.is_some() && inspection.readiness != Readiness::Ready {
        return Err(BridgeError::new("EVERIFY", "The bridge connection is not ready. Inspect or repair it before saving."));
    }
    if inspection.support != SupportState::Ready { return Err(BridgeError::new("ENEEDSSETUP", "Install bridge support first.")); }
    let selection = inspection.selection.clone();
    app.update(|settings| { settings.bridge = selection; Ok(()) })
        .map_err(|error| BridgeError::new(&error.code, error.message))?;
    Ok(inspection)
}

pub(crate) async fn launch_lease(app: &App, wifi: bool, mode: WifiMode) -> Result<Option<(NetworkTarget, std::fs::File)>, BridgeError> {
    if !needs_managed(wifi, mode, &NetworkOverrides::from_env()) { return Ok(None); }
    let selection = app.settings.lock().unwrap().bridge.clone()
        .ok_or_else(|| BridgeError::new("ENEEDSSETUP", "Choose and verify a bridge connection before starting."))?;
    let lease: TransportLease = blocking(move || HelperClient::acquire(selection)).await?;
    let (target, file) = lease.into_parts();
    let target = match target {
        LaunchTarget::MacSocket { path } => NetworkTarget::MacSocket { path },
        LaunchTarget::LinuxBridge { name, helper } => NetworkTarget::LinuxBridge { name, helper },
        LaunchTarget::WindowsTap { name } => NetworkTarget::WindowsTap { name },
    };
    Ok(Some((target, file)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use eink_core::{network::NetworkOverrides, session::WifiMode};
    #[test]
    fn bridge_commands_reject_secondary_windows_and_active_emulators() {
        assert_eq!(check_window("heap").unwrap_err().code, "EPERM");
        assert!(check_window("main").is_ok());
        for state in ["starting", "running", "stopping"] { assert_eq!(check_idle(state).unwrap_err().code, "EBUSY"); }
        assert!(check_idle("idle").is_ok());
    }
    #[test]
    fn managed_launch_is_skipped_for_wifi_off_nat_and_inherited_overrides() {
        let none = NetworkOverrides::default();
        assert!(needs_managed(true, WifiMode::Bridge, &none));
        assert!(!needs_managed(false, WifiMode::Bridge, &none));
        assert!(!needs_managed(true, WifiMode::Nat, &none));
        for net in ["", "-nic none"] {
            assert!(!needs_managed(true, WifiMode::Bridge, &NetworkOverrides { net: Some(net.into()), bridge: None }));
        }
        assert!(!needs_managed(true, WifiMode::Bridge, &NetworkOverrides { net: None, bridge: Some("custom".into()) }));
    }
    #[test]
    fn repeated_or_forged_apply_is_rejected_before_an_authorization_prompt() {
        let id = OperationId::new("a".repeat(32)).unwrap();
        let mut op = Operation { id: id.clone(), revision: 1, state: OperationState::AwaitingAuthorization,
            stage: "Review".into(), confirmation_deadline: None, reboot_required: false, error: None };
        assert!(check_apply(Some(&op), &id).is_ok());
        assert_eq!(check_apply(None, &id).unwrap_err().code, "ESTALE");
        assert_eq!(check_apply(Some(&op), &OperationId::new("b".repeat(32)).unwrap()).unwrap_err().code, "ESTALE");
        op.state = OperationState::Ready;
        assert_eq!(check_apply(Some(&op), &id).unwrap_err().code, "EBUSY");
    }
}
