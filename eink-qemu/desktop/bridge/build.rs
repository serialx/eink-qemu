use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rustc-check-cfg=cfg(eink_bridge_embedded_payload)");
    let manifest_path = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("../scripts/bridge/dependencies.json");
    println!("cargo:rerun-if-changed={}", manifest_path.display());
    let dependencies: serde_json::Value = serde_json::from_slice(&fs::read(manifest_path).expect("read bridge dependency manifest")).expect("parse bridge dependency manifest");
    assert_eq!(dependencies["version"], 1, "unsupported bridge dependency manifest");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let files = dependencies["targets"]["x86_64-pc-windows-msvc"]["files"].as_array().expect("pinned TAP files");
        let mut generated = String::from("const FILES: &[(&str, u64, &str)] = &[\n");
        for file in files {
            let name = file["path"].as_str().unwrap().strip_prefix("driver/").expect("driver payload path");
            assert!(matches!(name, "OemVista.inf" | "tap0901.cat" | "tap0901.sys"));
            let digest = file["sha256"].as_str().unwrap();
            assert!(digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit()));
            generated.push_str(&format!("({name:?}, {}, {digest:?}),\n", file["size"].as_u64().unwrap()));
        }
        assert_eq!(files.len(), 3);
        generated.push_str("];");
        fs::write(PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("tap-pins.rs"), generated).unwrap();
    }
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
    let target = env::var("TARGET").unwrap();
    let expected = dependencies["targets"][&target]["files"].as_array().expect("supported macOS helper architecture")
        .iter().find(|file| file["path"] == "socket_vmnet").expect("pinned socket_vmnet executable")["sha256"].as_str().unwrap();
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
