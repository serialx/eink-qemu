//! Fixed installed-service composition. No command-line or IPC field selects
//! the journal, driver package, backend, or service account.
use crate::{model::BridgeError, windows_driver::VerifiedPackage, windows_elevation,
    windows_platform::WindowsPlatform, windows_scm, windows_server};

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
