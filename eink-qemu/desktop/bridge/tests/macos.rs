#![cfg(target_os = "macos")]
use eink_bridge::{macos_service::{ServiceSpec, LocalUser, PayloadArchitecture, verify_payload}, model::*};

fn adapter() -> Adapter {
    Adapter { id: AdapterId::new("en0").unwrap(), name: "USB Ethernet \"Test\" & LAN".into(), kind: AdapterKind::Ethernet,
        link_up: true, eligible: true, reason: None, fingerprint: "en0:001122334455".into() }
}
fn spec() -> ServiceSpec {
    ServiceSpec::new(LocalUser::new(501, 20).unwrap(), ResourceId::new("0123456789abcdef0123456789abcdef").unwrap(), &adapter()).unwrap()
}

#[test]
fn launchd_uses_fixed_paths_and_argument_arrays_not_display_names() {
    let spec = spec();
    let xml = spec.plist().unwrap();
    let value = plist::Value::from_reader_xml(xml.as_slice()).unwrap();
    let data = value.as_dictionary().unwrap();
    let arguments = data["ProgramArguments"].as_array().unwrap().iter().map(|v| v.as_string().unwrap()).collect::<Vec<_>>();
    assert_eq!(arguments, vec!["/Library/Application Support/eink-qemu/bridge/socket_vmnet", "--vmnet-mode=bridged", "--vmnet-interface=en0", "--socket-group=staff", "/var/run/eink-qemu/u501/0123456789abcdef0123456789abcdef.sock"]);
    assert_eq!(data["Label"].as_string().unwrap(), "net.serialx.eink-qemu.bridge.u501.0123456789abcdef0123456789abcdef");
    assert!(!String::from_utf8(xml).unwrap().contains("USB Ethernet"));
}

#[test]
fn service_creation_refuses_system_accounts_and_unavailable_adapters() {
    assert!(LocalUser::new(0, 0).is_err());
    assert!(LocalUser::new(1, 1).is_err());
    let mut a = adapter(); a.eligible = false;
    assert_eq!(ServiceSpec::new(LocalUser::new(501, 20).unwrap(), spec().resource_id().clone(), &a).unwrap_err().code, "EUNSUPPORTED");
    a.eligible = true; a.link_up = false;
    assert_eq!(ServiceSpec::new(LocalUser::new(501, 20).unwrap(), spec().resource_id().clone(), &a).unwrap_err().code, "EDISCONNECTED");
}

#[test]
fn resource_files_are_distinct_per_user_and_cannot_alias_other_services() {
    let first = spec();
    let second = ServiceSpec::new(LocalUser::new(502, 20).unwrap(), first.resource_id().clone(), &adapter()).unwrap();
    assert_ne!(first.socket(), second.socket());
    assert_ne!(first.plist_path(), second.plist_path());
    assert_eq!(first.plist_path().parent().unwrap().to_str().unwrap(), "/Library/LaunchDaemons");
    assert!(first.plist_path().file_name().unwrap().to_str().unwrap().starts_with("net.serialx.eink-qemu.bridge."));
    assert!(ResourceId::new("../../io.github.lima-vm.socket_vmnet").is_err());
}

#[test]
fn forged_or_incompatible_payload_is_rejected_before_installation() {
    for architecture in [PayloadArchitecture::Arm64, PayloadArchitecture::X86_64] {
        assert_eq!(verify_payload(b"#!/bin/sh\nexit 0\n", architecture).unwrap_err().code, "EPAYLOAD");
    }
}

#[test]
fn revalidation_rejects_replaced_or_unplugged_adapter() {
    let service = spec(); let mut current = adapter();
    assert!(service.validate_adapter(&[current.clone()]).is_ok());
    current.fingerprint = "en0:replaced".into();
    assert_eq!(service.validate_adapter(&[current]).unwrap_err().code, "ESTALE");
    assert_eq!(service.validate_adapter(&[]).unwrap_err().code, "EDISCONNECTED");
}

#[test]
fn protected_installation_refuses_user_owned_payloads_and_symlinks() {
    use eink_bridge::macos_service::validate_protected_file;
    use std::os::unix::{fs::symlink, fs::PermissionsExt};
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("payload");
    std::fs::write(&file, b"untrusted").unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(validate_protected_file(&file).unwrap_err().code, "EPERM");
    let link = dir.path().join("redirect"); symlink(&file, &link).unwrap();
    assert_eq!(validate_protected_file(&link).unwrap_err().code, "EPERM");
}

#[test]
fn native_mutation_refuses_an_unelevated_caller_before_touching_host() {
    use eink_bridge::macos_service::NativeService;
    assert_eq!(NativeService::new().unwrap_err().code, "EAUTH");
}

#[test]
fn extended_acls_cannot_grant_writes_behind_safe_posix_modes() {
    use eink_bridge::transport::unix::validate_no_acl_writes;
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("protected");
    std::fs::write(&file, b"payload").unwrap();
    let handle = std::fs::File::open(&file).unwrap();
    validate_no_acl_writes(&handle).unwrap();
    let name = std::process::Command::new("/usr/bin/id").arg("-un").output().unwrap();
    let name = String::from_utf8(name.stdout).unwrap();
    let grant = format!("user:{} allow write", name.trim());
    assert!(std::process::Command::new("/bin/chmod").args(["+a", &grant]).arg(&file).status().unwrap().success());
    assert_eq!(validate_no_acl_writes(&handle).unwrap_err().code, "EPERM");
    assert!(std::process::Command::new("/bin/chmod").arg("-N").arg(&file).status().unwrap().success());
    let read = format!("user:{} allow read", name.trim());
    assert!(std::process::Command::new("/bin/chmod").args(["+a", &read]).arg(&file).status().unwrap().success());
    validate_no_acl_writes(&handle).unwrap();
}

#[test]
fn native_acl_grants_only_directory_traversal() {
    use eink_bridge::{macos_service::{set_search_acl, verify_search_acl}, transport::unix::validate_no_acl_writes};
    let dir = tempfile::tempdir().unwrap();
    let handle = std::fs::File::open(dir.path()).unwrap();
    let user = LocalUser::new(unsafe { libc::getuid() }, unsafe { libc::getgid() }).unwrap();
    set_search_acl(&handle, user).unwrap();
    verify_search_acl(&handle, user).unwrap();
    assert!(verify_search_acl(&handle, LocalUser::new(user.uid() + 1, user.gid()).unwrap()).is_err());
    validate_no_acl_writes(&handle).unwrap();
    let listing = std::process::Command::new("/bin/ls").arg("-lde").arg(dir.path()).output().unwrap();
    let listing = String::from_utf8(listing.stdout).unwrap();
    assert!(listing.contains("allow search"), "{listing}");
    assert!(!listing.contains("allow write"), "{listing}");
    assert!(std::process::Command::new("/bin/chmod").args(["+a", "group:staff allow search"]).arg(dir.path()).status().unwrap().success());
    assert!(verify_search_acl(&handle, user).is_err());
}
