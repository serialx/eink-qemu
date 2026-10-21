#![cfg(target_os = "linux")]
use eink_bridge::linux_helper::{Grant, Policy};

#[test]
fn policy_updates_preserve_other_users_and_never_grant_all_bridges() {
    let alice = Grant::new(1000, "ebr123").unwrap();
    let bob = Grant::new(1001, "br-existing").unwrap();
    let mut policy = Policy::parse(b"# App bridge grants\nallow 1000 ebr123\nallow 1001 br-existing\n").unwrap();
    assert!(policy.contains(&alice) && policy.contains(&bob));
    policy.remove(&alice);
    assert!(!policy.contains(&alice) && policy.contains(&bob));
    assert_eq!(Policy::parse(&policy.encode()).unwrap(), policy);
    policy.insert(alice.clone());
    assert!(policy.contains(&alice));
    for bytes in [b"allow all\n".as_slice(), b"allow 1000 all\n", b"include /tmp/policy\n", b"allow -1 br0\n", b"allow 4294967296 br0\n", b"allow 1000 br0 extra\n", b"allow 1000 br0\0\n"] {
        assert!(Policy::parse(bytes).is_err());
    }
    for name in ["", "all", "../bridge", "a b", "0123456789abcdef"] { assert!(Grant::new(1000, name).is_err()); }
    assert!(Grant::new(0, "br0").is_err());
}
