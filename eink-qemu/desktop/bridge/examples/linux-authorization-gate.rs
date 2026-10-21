#[cfg(target_os = "linux")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::{io::{Read, Write}, os::unix::{net::{UnixListener, UnixStream}, fs::PermissionsExt}, process::Command};
    let args = std::env::args().collect::<Vec<_>>();
    if args.len() == 3 && args[1] == "--client" {
        assert_eq!(unsafe { libc::geteuid() }, 1000);
        let mut stream = UnixStream::connect(&args[2])?;
        stream.write_all(b"request")?;
        let mut result = [0]; stream.read_exact(&mut result)?;
        assert_eq!(result, [1]);
        return Ok(());
    }
    if args.len() != 3 || args[1] != "--exercise" || std::fs::read_to_string("/etc/hostname")?.trim() != args[2] {
        return Err("requires --exercise exact-disposable-hostname".into());
    }
    assert_eq!(unsafe { libc::geteuid() }, 0);
    let directory = tempfile::tempdir_in("/var/tmp")?;
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o755))?;
    let socket = directory.path().join("authorization.sock");
    let listener = UnixListener::bind(&socket)?;
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o666))?;
    let mut child = Command::new("/usr/sbin/runuser").args(["-u", "ubuntu", "--"])
        .arg(std::env::current_exe()?).arg("--client").arg(&socket).spawn()?;
    let (mut stream, _) = listener.accept()?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(5)))?;
    let caller = eink_bridge::transport::unix::caller(&stream)?;
    assert_eq!(caller.uid()?, 1000);
    assert_eq!(caller.process().uid(), 1000);
    let mut request = [0; 7]; stream.read_exact(&mut request)?;
    assert_eq!(&request, b"request");
    let result = eink_bridge::linux_authorization::authorize(&caller);
    match result {
        Err(error) if error.code == "EAUTHDENIED" => println!("nativePolkitDeniesUnauthenticatedSession=passed"),
        result => return Err(format!("unexpected authorization result: {result:?}").into()),
    }
    stream.write_all(&[1])?;
    assert!(child.wait()?.success());
    assert!(caller.process().revalidate().is_err(), "exited caller identity must be rejected");
    println!("nativeCallerIdentityAndExitRejection=passed");
    Ok(())
}
#[cfg(not(target_os = "linux"))]
fn main() { panic!("Linux only"); }
