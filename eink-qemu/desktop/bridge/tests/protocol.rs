use eink_bridge::protocol::{decode_request, decode_frame, encode_frame, HelperRequest, RequestKind};

fn current(json: &str) -> String {
    json.replacen('{', &format!(r#"{{"helperVersion":"{}","#, env!("CARGO_PKG_VERSION")), 1)
}

#[test]
fn unknown_versions_and_arbitrary_commands_are_refused() {
    assert_eq!(decode_request(&current(r#"{"version":99,"kind":"inspect"}"#)).unwrap_err().code, "EPROTOCOL");
    for json in [
        r#"{"version":1,"kind":"exec","command":"anything"}"#,
        r#"{"version":1,"kind":"inspect","command":"anything"}"#,
        r#"{"version":1,"kind":"apply","planId":"../../another-user"}"#,
        r#"{"version":1,"kind":"apply","planId":""}"#,
        r#"{"kind":"inspect"}"#,
    ] {
        assert!(decode_request(&current(json)).is_err(), "accepted {json}");
    }
    assert!(matches!(decode_request(&current(r#"{"version":1,"kind":"inspect"}"#)).unwrap().request, RequestKind::Inspect));
}

#[test]
fn framing_rejects_truncated_trailing_and_oversized_messages() {
    let request = HelperRequest::inspect();
    let frame = encode_frame(&request).unwrap();
    assert!(matches!(decode_frame(&frame).unwrap().request, RequestKind::Inspect));
    assert!(decode_frame(&frame[..frame.len() - 1]).is_err());
    assert!(decode_frame(&[0, 0, 0]).is_err());
    assert!(decode_frame(&[0, 16, 0, 1]).is_err());
    let mut trailing = frame.clone(); trailing.push(0);
    assert!(decode_frame(&trailing).is_err());
}

#[test]
fn duplicate_fields_cannot_change_the_reviewed_request() {
    for json in [
        r#"{"version":99,"version":1,"kind":"inspect"}"#,
        r#"{"version":1,"kind":"recover","kind":"inspect"}"#,
        r#"{"version":1,"kind":"plan","request":{"kind":"configure","adapterId":"a","adapterId":"b"}}"#,
    ] { assert!(decode_request(&current(json)).is_err(), "accepted {json}"); }
}

#[test]
fn requests_identify_the_package_before_dispatch() {
    let mut wire = serde_json::to_value(HelperRequest::inspect()).unwrap();
    assert_eq!(wire["helperVersion"], env!("CARGO_PKG_VERSION"));
    for version in ["0.0.0", "999.0.0", "", "0.8.1-extra"] {
        wire["helperVersion"] = version.into();
        assert_eq!(decode_request(&wire.to_string()).unwrap_err().code, "EPROTOCOL");
    }
}

#[test]
fn legacy_requests_cannot_reach_mutation_dispatch() {
    // An older desktop must be refused before a valid operation ID is used.
    for kind in ["inspect", "recover", "keep", "cancel", "apply"] {
        let mut wire = serde_json::json!({"version": 1, "kind": kind});
        if kind == "apply" { wire["planId"] = "0123456789abcdef0123456789abcdef".into(); }
        if kind == "keep" || kind == "cancel" { wire["operationId"] = "0123456789abcdef0123456789abcdef".into(); }
        assert!(decode_request(&wire.to_string()).is_err(), "accepted legacy {kind}");
    }
}

#[test]
fn replies_from_a_different_or_legacy_package_are_rejected() {
    use eink_bridge::protocol::{decode_reply, HelperReply, ReplyKind};
    let reply = HelperReply::new(ReplyKind::Operation { operation: None });
    let mut wire = serde_json::to_value(reply).unwrap();
    assert_eq!(wire["helperVersion"], env!("CARGO_PKG_VERSION"));
    decode_reply(&serde_json::to_vec(&wire).unwrap()).unwrap();
    wire["helperVersion"] = "999.0.0".into();
    assert_eq!(decode_reply(&serde_json::to_vec(&wire).unwrap()).unwrap_err().code, "EPROTOCOL");
    wire.as_object_mut().unwrap().remove("helperVersion");
    assert_eq!(decode_reply(&serde_json::to_vec(&wire).unwrap()).unwrap_err().code, "EPROTOCOL");
}
