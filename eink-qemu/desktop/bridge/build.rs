use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rustc-check-cfg=cfg(eink_bridge_embedded_payload)");
    for name in ["EINK_BRIDGE_MACOS_TEAM_ID", "EINK_BRIDGE_MACOS_PAYLOAD"] { println!("cargo:rerun-if-env-changed={name}"); }
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") { return; }
    let team = env::var("EINK_BRIDGE_MACOS_TEAM_ID").ok();
    let payload = env::var_os("EINK_BRIDGE_MACOS_PAYLOAD");
    if team.is_none() && payload.is_none() { return; } // Ordinary unit/development builds.
    let team = team.expect("signed bridge builds require EINK_BRIDGE_MACOS_TEAM_ID");
    assert!(team.len() == 10 && team.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit()), "invalid Apple Team ID");
    let payload = PathBuf::from(payload.expect("signed bridge builds require EINK_BRIDGE_MACOS_PAYLOAD"));
    println!("cargo:rerun-if-changed={}", payload.display());
    let bytes = fs::read(payload).expect("read pinned socket_vmnet payload");
    use sha2::{Digest, Sha256};
    let expected = match env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("aarch64") => "b8a72a62237312f2f756027dea504a844edeb40014702d4a320292c026d282b0",
        Ok("x86_64") => "526086c810df74a2465da6fd841a6e7a9a6a74679029dc76eee9dee2291b4929",
        _ => panic!("unsupported macOS helper architecture"),
    };
    assert_eq!(format!("{:x}", Sha256::digest(&bytes)), expected, "unverified socket_vmnet payload");
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    fs::write(out.join("socket_vmnet"), bytes).unwrap();
    let label = "net.serialx.eink-qemu.bridge-helper";
    let version = env::var("CARGO_PKG_VERSION").unwrap();
    let info = format!(r#"<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>{label}</string>
<key>CFBundleVersion</key><string>{version}</string>
<key>SMAuthorizedClients</key><array><string>anchor apple generic and identifier "net.serialx.eink-qemu" and certificate leaf[subject.OU] = "{team}"</string></array>
</dict></plist>"#);
    let launchd = format!(r#"<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict>
<key>Label</key><string>{label}</string><key>RunAtLoad</key><true/><key>KeepAlive</key><true/>
<key>ThrottleInterval</key><integer>5</integer><key>UserName</key><string>root</string>
</dict></plist>"#);
    for (name, section, contents) in [("helper-info.plist", "__info_plist", info), ("helper-launchd.plist", "__launchd_plist", launchd)] {
        let file = out.join(name); fs::write(&file, contents).unwrap();
        println!("cargo:rustc-link-arg-bin=eink-bridge-helper=-Wl,-sectcreate,__TEXT,{section}");
        println!("cargo:rustc-link-arg-bin=eink-bridge-helper=-Xlinker");
        println!("cargo:rustc-link-arg-bin=eink-bridge-helper={}", file.display());
    }
    println!("cargo:rustc-cfg=eink_bridge_embedded_payload");
}
