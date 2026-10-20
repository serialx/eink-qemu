use eink_core::{network::{self, NetworkOverrides, NetworkTarget}, session::WifiMode};

#[test]
fn managed_targets_preserve_qemu_argument_boundaries() {
    let overrides = NetworkOverrides::default();
    let targets = [
        (NetworkTarget::MacSocket { path: "/var/run/reader's bridge,1/socket".into() },
         "stream,server=off,addr.type=unix,addr.path=/var/run/reader's bridge,,1/socket,model=esp32_wifi"),
        (NetworkTarget::WindowsTap { name: "Reader LAN,2".into() }, "tap,ifname=Reader LAN,,2,model=esp32_wifi"),
        (NetworkTarget::LinuxBridge { name: "br-reader".into(), helper: "/opt/reader's bridge/helper".into() },
         "bridge,br=br-reader,model=esp32_wifi,helper='/opt/reader'\\''s bridge/helper'"),
    ];
    for (target, expected) in targets {
        assert_eq!(network::args(WifiMode::Bridge, Some(&target), &overrides).unwrap(), Some(vec!["-nic".into(), expected.into()]));
    }
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
    let target = NetworkTarget::LinuxBridge { name: "br0".into(), helper: "/opt/$(touch x);helper".into() };
    let result = network::args(WifiMode::Bridge, Some(&target), &NetworkOverrides::default()).unwrap().unwrap();
    // QEMU runs helpers through a shell only when the helper value contains whitespace.
    // Quote the entire path, including shell syntax in its filename.
    assert_eq!(result[1], "bridge,br=br0,model=esp32_wifi,helper='/opt/$(touch x);helper'");
}
