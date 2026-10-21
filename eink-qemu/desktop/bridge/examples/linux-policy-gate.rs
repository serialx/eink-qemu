//! Run only inside the disposable helper gate after it stages the packet helper.
#[cfg(target_os = "linux")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use eink_bridge::linux_helper::{self as access, Grant, POLICY};
    let mut name = [0 as libc::c_char; 256];
    if unsafe { libc::geteuid() } != 0 || unsafe { libc::gethostname(name.as_mut_ptr(), name.len()) } != 0
        || unsafe { std::ffi::CStr::from_ptr(name.as_ptr()) }.to_str()? != "eink-bridge-test" { return Err("requires disposable root test host".into()); }
    if std::path::Path::new(POLICY).symlink_metadata().is_ok() { return Err("preexisting policy must be preserved".into()); }
    let alice = Grant::new(1000, "einkgate0")?;
    let bob = Grant::new(1001, "br-other")?;
    let first = access::prepare(&alice, true)?;
    access::apply(&first)?;
    access::verify_grant(&alice)?;
    let second = access::prepare(&bob, true)?;
    access::apply(&second)?;
    access::verify_grant(&bob)?;
    let remove = access::prepare(&alice, false)?;
    access::apply(&remove)?;
    assert!(access::verify_grant(&alice).is_err());
    access::verify_grant(&bob)?;
    access::restore(&remove)?;
    access::verify_grant(&alice)?;
    access::restore(&second)?;
    access::verify_grant(&alice)?;
    assert!(access::verify_grant(&bob).is_err());
    // Restore from the same serialized change a restarted service would read.
    let saved = serde_json::to_vec(&first)?;
    let restored = serde_json::from_slice(&saved)?;
    access::restore(&restored)?;
    access::restore(&restored)?;
    assert!(!std::path::Path::new(POLICY).exists());
    println!("nativePolicyPreservesOtherUsersAndRestoresOriginalBytes=passed");
    Ok(())
}
#[cfg(not(target_os = "linux"))]
fn main() { panic!("Linux only"); }
