use eink_core::{network::{self, NetworkOverrides, NetworkTarget}, session::WifiMode};

#[cfg(unix)]
#[test]
#[ignore = "requires EINK_TEST_QEMU_RISCV32, EINK_TEST_QEMU_XTENSA and EINK_TEST_QEMU_SHARE; opens only a temporary Unix socket"]
fn packaged_qemu_connects_managed_socket_for_both_boards() {
    use std::{io::Read, os::unix::net::UnixListener, process::{Child, Command, Stdio}, time::{Duration, Instant}};
    struct Running(Child);
    impl Drop for Running {
        fn drop(&mut self) { let _ = self.0.kill(); let _ = self.0.wait(); }
    }
    let root = tempfile::tempdir().unwrap();
    let socket = root.path().join("reader's bridge,1.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let args = network::args(WifiMode::Bridge, Some(&NetworkTarget::MacSocket { path: socket }),
        &NetworkOverrides::default()).unwrap().unwrap();
    for (variable, machine) in [("EINK_TEST_QEMU_RISCV32", "x3"), ("EINK_TEST_QEMU_XTENSA", "x4pro")] {
        let mut child = Running(Command::new(std::env::var_os(variable).expect(variable))
            .args(["-machine", machine, "-S", "-display", "none", "-serial", "null", "-monitor", "none", "-L"])
            .arg(std::env::var_os("EINK_TEST_QEMU_SHARE").expect("EINK_TEST_QEMU_SHARE"))
            .args(&args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped()).spawn().unwrap());
        let deadline = Instant::now() + Duration::from_secs(10);
        let peer = loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                let mut error = String::new();
                child.0.stderr.take().unwrap().read_to_string(&mut error).unwrap();
                panic!("{machine} exited before connecting: {status}: {error}");
            }
            match listener.accept() {
                Ok((peer, _)) => break peer,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => (),
                Err(error) => panic!("{error}"),
            }
            assert!(Instant::now() < deadline, "{machine} did not connect to its managed socket");
            std::thread::sleep(Duration::from_millis(20));
        };
        assert!(child.0.try_wait().unwrap().is_none());
        drop(child);
        drop(peer);
    }
}

#[cfg(windows)]
#[test]
fn windows_child_retains_read_only_lease_after_stdin_eof_and_parent_drop() {
    use std::{fs::OpenOptions, io::{BufRead, BufReader, Read, Write}, os::windows::{fs::OpenOptionsExt, io::AsRawHandle}, process::{Command, Stdio}, time::Duration};
    use windows_sys::Win32::Foundation::{GetHandleInformation, HANDLE_FLAG_INHERIT};
    if std::env::var_os("EINK_CORE_LEASE_CHILD").is_some() {
        assert_eq!(std::io::stdin().read(&mut [0]).unwrap(), 0);
        println!("lease-child-read-eof"); std::io::stdout().flush().unwrap();
        std::thread::sleep(Duration::from_secs(2));
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("lease");
    std::fs::write(&path, []).unwrap();
    let file = OpenOptions::new().read(true).share_mode(1).open(&path).unwrap();
    let inherited = network::InheritedLease::new(&file).unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command.args(["--exact", "windows_child_retains_read_only_lease_after_stdin_eof_and_parent_drop", "--nocapture"])
        .env("EINK_CORE_LEASE_CHILD", "1").stdout(Stdio::piped());
    inherited.configure(&mut command);
    let mut flags = 0;
    assert_ne!(unsafe { GetHandleInformation(file.as_raw_handle(), &mut flags) }, 0);
    assert_eq!(flags & HANDLE_FLAG_INHERIT, 0);
    let mut child = command.spawn().unwrap();
    drop(command); drop(file);
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    loop {
        assert_ne!(output.read_line(&mut line).unwrap(), 0, "child failed before EOF");
        if line.contains("lease-child-read-eof") { break; }
        line.clear();
    }
    let exclusive = || OpenOptions::new().read(true).write(true).share_mode(0).open(&path);
    assert_eq!(exclusive().unwrap_err().raw_os_error(), Some(32));
    assert!(child.wait().unwrap().success());
    assert!(exclusive().is_ok());
}

#[cfg(unix)]
#[test]
fn the_emulator_inherits_the_lease_without_making_parent_descriptors_inheritable() {
    use std::{fs::OpenOptions, os::fd::AsRawFd, process::{Command, Stdio}};
    let path = tempfile::NamedTempFile::new().unwrap();
    let file = OpenOptions::new().read(true).open(path.path()).unwrap();
    file.try_lock_shared().unwrap();
    let lease = network::InheritedLease::new(&file).unwrap();
    let args = lease.arguments();
    assert_eq!(args[0], "-add-fd");
    assert!(args[1].contains("opaque=eink-bridge-lease"));
    let mut command = Command::new("/bin/cat");
    command.stdin(Stdio::piped()).stdout(Stdio::null());
    lease.configure(&mut command);
    let mut child = command.spawn().unwrap();
    assert_ne!(unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFD) } & libc::FD_CLOEXEC, 0);
    drop(file); drop(lease); drop(command);
    let contender = OpenOptions::new().read(true).open(path.path()).unwrap();
    assert!(contender.try_lock().is_err(), "the running child must retain its lease after the parent drops it");
    drop(child.stdin.take());
    assert!(child.wait().unwrap().success());
    contender.try_lock().unwrap();
}

#[test]
fn managed_targets_preserve_qemu_argument_boundaries() {
    let overrides = NetworkOverrides::default();
    let targets = [
        (NetworkTarget::WindowsTap { name: "Reader LAN,2".into() }, "tap,ifname=Reader LAN,,2,model=esp32_wifi"),
        (NetworkTarget::WindowsTap { name: "guid:{12345678-1234-5678-9abc-1234567890ab}".into() },
         "tap,ifname=guid:{12345678-1234-5678-9abc-1234567890ab},model=esp32_wifi"),
        #[cfg(unix)]
        (NetworkTarget::LinuxBridge { name: "br-reader".into(), helper: "/opt/reader's bridge/helper".into() },
         "bridge,br=br-reader,model=esp32_wifi,helper='/opt/reader'\\''s bridge/helper'"),
    ];
    for (target, expected) in targets {
        assert_eq!(network::args(WifiMode::Bridge, Some(&target), &overrides).unwrap(), Some(vec!["-nic".into(), expected.into()]));
    }
}

#[cfg(unix)]
#[test]
fn managed_socket_uses_structured_netdev_and_the_board_nic() {
    let path = "/var/run/reader's \"bridge\",1/socket";
    let args = network::args(WifiMode::Bridge, Some(&NetworkTarget::MacSocket { path: path.into() }),
        &NetworkOverrides::default()).unwrap().unwrap();
    assert_eq!(args.len(), 4);
    assert_eq!(args[0], "-netdev");
    let backend: serde_json::Value = serde_json::from_str(&args[1]).unwrap();
    assert_eq!(backend, serde_json::json!({"type":"stream", "id":"eink-managed-wifi", "server":false,
        "addr":{"type":"unix", "path":path}}));
    assert_eq!(&args[2..], ["-net", "nic,netdev=eink-managed-wifi,model=esp32_wifi"]);
}

#[test]
fn external_overrides_preserve_empty_and_legacy_semantics() {
    let target = NetworkTarget::WindowsTap { name: "managed".into() };
    assert_eq!(network::args(WifiMode::Bridge, Some(&target), &NetworkOverrides { net: Some("".into()), bridge: None }).unwrap(), Some(vec![]));
    assert_eq!(network::args(WifiMode::Bridge, Some(&target), &NetworkOverrides { net: Some("-nic none".into()), bridge: Some("custom".into()) }).unwrap(), Some(vec!["-nic".into(), "none".into()]));
    assert_eq!(network::args(WifiMode::Bridge, Some(&target), &NetworkOverrides { net: None, bridge: Some("custom".into()) }).unwrap(), None);
    assert_eq!(network::args(WifiMode::Nat, Some(&target), &NetworkOverrides::default()).unwrap(), Some(vec!["-nic".into(), "user,model=esp32_wifi".into()]));
    assert_eq!(network::args(WifiMode::Bridge, None, &NetworkOverrides::default()).unwrap(), None);
}

#[test]
fn malformed_managed_targets_cannot_inject_options_or_shell_commands() {
    for target in [
        NetworkTarget::WindowsTap { name: "".into() },
        NetworkTarget::WindowsTap { name: "adapter\nother".into() },
        NetworkTarget::MacSocket { path: "relative".into() },
        NetworkTarget::LinuxBridge { name: "br0".into(), helper: "relative".into() },
    ] {
        assert!(network::args(WifiMode::Bridge, Some(&target), &NetworkOverrides::default()).is_err());
    }
    #[cfg(unix)] {
        let target = NetworkTarget::LinuxBridge { name: "br0".into(), helper: "/opt/$(touch x);helper".into() };
        let result = network::args(WifiMode::Bridge, Some(&target), &NetworkOverrides::default()).unwrap().unwrap();
        // QEMU runs helpers through a shell only when the helper value contains whitespace.
        // Quote the entire path, including shell syntax in its filename.
        assert_eq!(result[1], "bridge,br=br0,model=esp32_wifi,helper='/opt/$(touch x);helper'");
    }
}
