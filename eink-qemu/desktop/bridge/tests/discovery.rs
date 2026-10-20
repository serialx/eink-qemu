use eink_bridge::model::*;

fn adapter(id: &str, fingerprint: &str, link_up: bool) -> Adapter {
    Adapter { id: AdapterId::new(id).unwrap(), name: "USB Ethernet".into(), kind: AdapterKind::Ethernet,
        link_up, eligible: true, reason: None, fingerprint: fingerprint.into() }
}

#[test]
fn a_replacement_with_the_same_display_name_is_not_the_saved_adapter() {
    let selected = BridgeSelection { version: 1, platform: Platform::Macos,
        adapter_id: AdapterId::new("en7").unwrap(), fingerprint: "original".into(), resource_id: None, display_name: "USB Ethernet".into() };
    assert_eq!(selection_readiness(&selected, Platform::Macos, &[adapter("en7", "replacement", true)]), Readiness::NeedsRepair);
    assert_eq!(selection_readiness(&selected, Platform::Macos, &[]), Readiness::Disconnected);
    assert_eq!(selection_readiness(&selected, Platform::Windows, &[adapter("en7", "original", true)]), Readiness::NeedsSetup);
    // Discovery alone is insufficient to claim backend readiness.
    assert_eq!(selection_readiness(&selected, Platform::Macos, &[adapter("en7", "original", true)]), Readiness::NeedsSetup);
}

#[test]
fn identities_and_labels_are_separate() {
    let a = adapter("adapter-a", "one", true); let b = adapter("adapter-b", "two", true);
    assert_eq!(a.name, b.name); assert_ne!(a.id, b.id);
    assert!(AdapterId::new("name\0injection").is_err());
    assert!(AdapterId::new("").is_err());
    assert!(serde_json::from_str::<AdapterId>(r#"""#).is_err());
}
