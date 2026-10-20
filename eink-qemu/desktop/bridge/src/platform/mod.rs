use crate::model::*;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(windows)]
mod windows;

pub fn inspect() -> Result<Inspection, BridgeError> {
    #[cfg(target_os = "macos")]
    let (platform, adapters, capabilities) = (Platform::Macos, macos::adapters()?, Capabilities { create: true, existing_bridge: false, reason: None });
    #[cfg(target_os = "linux")]
    let (platform, adapters, capabilities) = { let (adapters, capabilities) = linux::inspect()?; (Platform::Linux, adapters, capabilities) };
    #[cfg(windows)]
    let (platform, adapters, capabilities) = (Platform::Windows, windows::adapters()?, Capabilities { create: false, existing_bridge: true,
        reason: Some("Bridge creation must be checked by the Windows setup helper.".into()) });
    Ok(Inspection { platform, adapters, capabilities, selection: None, readiness: Readiness::NeedsSetup, active_operation: None,
        external_override: if std::env::var_os("X4NET").is_some() { Some("X4NET".into()) }
            else if std::env::var_os("X4BR").is_some_and(|v| !v.is_empty()) { Some("X4BR".into()) } else { None } })
}
