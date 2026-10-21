//! This executable never runs the desktop or QEMU with its privileges.
fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args == ["--version"] { println!("eink-bridge-helper {} protocol {}", env!("CARGO_PKG_VERSION"), eink_bridge::model::VERSION); return; }
    if !args.is_empty() && args != ["--serve"] { eprintln!("Expected --serve or --version"); std::process::exit(2); }
    #[cfg(target_os = "macos")]
    let result = bootstrap().and_then(|_| eink_bridge::macos_server::serve());
    #[cfg(target_os = "linux")]
    let result = eink_bridge::linux_server::serve();
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let result: Result<(), eink_bridge::model::BridgeError> = Err(eink_bridge::model::BridgeError::new("EUNSUPPORTED", "The native bridge service is not available in this build."));
    if let Err(error) = result { eprintln!("{}: {}", error.code, error.message); std::process::exit(1); }
}

#[cfg(target_os = "macos")]
fn bootstrap() -> Result<(), eink_bridge::model::BridgeError> {
    #[cfg(eink_bridge_embedded_payload)] {
        use eink_bridge::macos_service::{NativeService, INSTALL_ROOT};
        let native = NativeService::new()?;
        let payload = std::path::Path::new(INSTALL_ROOT).join("socket_vmnet");
        // A root-edited payload is a repair conflict, not permission to replace
        // arbitrary bytes each time launchd restarts the helper.
        match std::fs::symlink_metadata(payload) {
            Ok(_) => native.check_payload()?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => native.install_payload(include_bytes!(concat!(env!("OUT_DIR"), "/socket_vmnet")))?,
            Err(error) => return Err(eink_bridge::model::BridgeError::new("EPAYLOAD", error.to_string())),
        }
        eink_bridge::authorization::macos::register_right()?;
    }
    Ok(())
}
