//! Fixed installed-service composition. No command-line or IPC field selects
//! the journal, driver package, backend, or service account.
use crate::{model::BridgeError, windows_driver::VerifiedPackage, windows_elevation,
    windows_platform::WindowsPlatform, windows_scm, windows_server};
use std::time::{SystemTime,UNIX_EPOCH};

pub fn serve() -> Result<(), BridgeError> {
    windows_scm::dispatch(|stop, ready| {
        let root = windows_elevation::journal_root()?;
        let package = windows_elevation::installation_root()?.join("driver");
        // Keep the signed, pinned package and its ancestors protected against
        // replacement for the entire service lifetime. Updates stop SCM first.
        let _package = VerifiedPackage::open(&package)?;
        let recovery_package = package.clone();
        windows_server::serve(&root,
            move |peer| WindowsPlatform::for_user(peer.caller().principal(), &package),
            move || WindowsPlatform::for_recovery(&recovery_package),
            stop, move || ready.running())
    })
}

/// Installer-only SCM mode. The package controller stops the ordinary service
/// before selecting this fixed command. No IPC request can remove other users'
/// resources, select a journal, or replace the native platform.
pub fn uninstall() -> Result<(), BridgeError> {
    windows_scm::dispatch(|_stop, ready| {
        let root = windows_elevation::journal_root()?;
        let package = windows_elevation::installation_root()?.join("driver");
        let _package = VerifiedPackage::open(&package)?;
        let _installation = windows_server::InstallationLock::open(&root)?;
        let mut coordinator = crate::coordinator::Coordinator::open(&root,"service",WindowsPlatform::for_recovery(&package)?)?;
        ready.running()?;
        coordinator.uninstall(|resource|WindowsPlatform::for_user(&resource.owner,&package),
            ||SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs())
    })
}
