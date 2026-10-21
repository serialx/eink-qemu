//! Developer acceptance harness. This is not installed with the app.
#[cfg(target_os = "macos")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use eink_bridge::{macos_service::*, model::*};
    use std::{ffi::CStr, path::Path};
    let args = std::env::args().collect::<Vec<_>>();
    if args.len() != 4 || !matches!(args[1].as_str(), "--exercise" | "--cleanup") { return Err("requires --exercise/--cleanup exact-hostname pinned-payload".into()); }
    let mut name = [0i8; 256];
    if unsafe { libc::gethostname(name.as_mut_ptr(), name.len()) } != 0 || unsafe { CStr::from_ptr(name.as_ptr()) }.to_str()? != args[2] { return Err("wrong disposable host".into()); }
    let model = std::process::Command::new("/usr/sbin/sysctl").args(["-n", "hw.model"]).output()?;
    if !String::from_utf8(model.stdout)?.starts_with("VirtualMac") { return Err("requires a disposable VirtualMac".into()); }
    let native = NativeService::new()?;
    let user = LocalUser::new(501, 20)?;
    let adapter = eink_bridge::inspect()?.adapters.into_iter().find(|a| a.id.as_str() == "en0").ok_or("no en0")?;
    let spec = ServiceSpec::new(user, ResourceId::new("ce3ba705658f4bb5b29a20bf7e1ab374")?, &adapter)?;
    if args[1] == "--cleanup" {
        native.remove(&spec)?;
        if Path::new(INSTALL_ROOT).exists() {
            native.check_payload()?;
            std::fs::remove_file(Path::new(INSTALL_ROOT).join("socket_vmnet"))?;
            std::fs::remove_dir(INSTALL_ROOT)?;
        }
        let _ = std::fs::remove_dir(SOCKET_ROOT);
        println!("rustNativeCleanup=passed");
        return Ok(());
    }
    if Path::new(INSTALL_ROOT).exists() || spec.plist_path().exists() { return Err("preexisting test resources; refusing to adopt".into()); }
    native.install_payload(&std::fs::read(&args[3])?)?;
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        native.start(&spec)?;
        native.verify(&spec)?;
        println!("rustNativeInstallAndUserOpen=passed");
        native.start(&spec)?;
        native.verify(&spec)?;
        println!("rustNativeRestartAndUserOpen=passed");
        // Root-owned but altered config must survive attempted removal.
        let original = std::fs::read(spec.plist_path())?;
        let mut changed = original.clone(); changed.extend(b"\n<!-- external edit -->\n");
        std::fs::write(spec.plist_path(), &changed)?;
        assert_eq!(native.remove(&spec).unwrap_err().code, "ECONFLICT");
        assert_eq!(std::fs::read(spec.plist_path())?, changed);
        std::fs::write(spec.plist_path(), original)?;
        println!("rustNativeConflictPreserved=passed");
        native.remove(&spec)?;
        assert!(!spec.plist_path().exists() && !spec.socket().exists());
        println!("rustNativeRemoval=passed");
        native.remove(&spec)?;
        Ok(())
    })();
    let cleanup = native.remove(&spec);
    // This harness proved INSTALL_ROOT absent before installing this payload.
    std::fs::remove_file(Path::new(INSTALL_ROOT).join("socket_vmnet"))?;
    std::fs::remove_dir(INSTALL_ROOT)?;
    let _ = std::fs::remove_dir(SOCKET_ROOT);
    cleanup?;
    result
}
#[cfg(not(target_os = "macos"))]
fn main() { panic!("macOS only"); }
