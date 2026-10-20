use eink_bridge::{inspect, model::*};
#[test]
fn host_inspection_never_claims_transport_readiness() {
    let result = inspect().unwrap();
    assert_ne!(result.readiness, Readiness::Ready);
    for adapter in &result.adapters {
        assert!(!adapter.id.as_str().is_empty());
        assert!(!adapter.fingerprint.is_empty());
        if !adapter.eligible { assert!(adapter.reason.is_some()); }
    }
}
