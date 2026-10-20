use eink_bridge::protocol::{decode_request, decode_frame, encode_frame, HelperRequest, RequestKind};

#[test]
fn unknown_versions_and_arbitrary_commands_are_refused() {
    assert_eq!(decode_request(r#"{"version":99,"kind":"inspect"}"#).unwrap_err().code, "EPROTOCOL");
    for json in [
        r#"{"version":1,"kind":"exec","command":"anything"}"#,
        r#"{"version":1,"kind":"inspect","command":"anything"}"#,
        r#"{"version":1,"kind":"apply","planId":"../../another-user"}"#,
        r#"{"version":1,"kind":"apply","planId":""}"#,
        r#"{"kind":"inspect"}"#,
    ] {
        assert!(decode_request(json).is_err(), "accepted {json}");
    }
    assert!(matches!(decode_request(r#"{"version":1,"kind":"inspect"}"#).unwrap().request, RequestKind::Inspect));
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
    ] { assert!(decode_request(json).is_err(), "accepted {json}"); }
}
