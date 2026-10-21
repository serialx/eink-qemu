use eink_bridge::{protocol::{HelperRequest, RequestKind}, transport};
use std::io::Cursor;

#[test]
fn authorization_proofs_are_bounded_redacted_and_only_accepted_for_approval_or_apply() {
    use eink_bridge::authorization::AuthorizationProof;
    let secret = "ab".repeat(32);
    let proof: AuthorizationProof = serde_json::from_str(&format!("\"{secret}\"")).unwrap();
    assert!(!format!("{proof:?}").contains(&secret));
    for value in ["a".repeat(63), "a".repeat(65), "gg".repeat(32), "AB".repeat(32)] {
        assert!(serde_json::from_str::<AuthorizationProof>(&format!("\"{value}\"")).is_err());
    }
    let id = "a".repeat(32);
    let request = eink_bridge::protocol::decode_request(&format!(
        r#"{{"version":1,"kind":"apply","planId":"{id}","authorization":"{secret}"}}"#)).unwrap();
    assert!(!format!("{request:?}").contains(&secret));
    for kind in ["inspect", "recover", "keep", "cancel"] {
        let operation = if kind == "keep" || kind == "cancel" { format!(r#", "operationId":"{id}""#) } else { String::new() };
        assert!(eink_bridge::protocol::decode_request(&format!(
            r#"{{"version":1,"kind":"{kind}"{operation},"authorization":"{secret}"}}"#)).is_err());
    }
}

#[test]
fn windows_approval_messages_have_distinct_strict_shapes() {
    use eink_bridge::protocol::{decode_request, encode_frame, decode_frame};
    let id = "12".repeat(16); let proof = "ab".repeat(32);
    for json in [format!(r#"{{"version":1,"kind":"beginAuthorization","planId":"{id}"}}"#),
        format!(r#"{{"version":1,"kind":"approveAuthorization","authorization":"{proof}"}}"#)] {
        let request = decode_request(&json).unwrap();
        decode_frame(&encode_frame(&request).unwrap()).unwrap();
        assert!(!format!("{request:?}").contains(&proof));
    }
    for json in [format!(r#"{{"version":1,"kind":"beginAuthorization","planId":"{id}","authorization":"{proof}"}}"#),
        format!(r#"{{"version":1,"kind":"approveAuthorization","planId":"{id}","authorization":"{proof}"}}"#),
        format!(r#"{{"version":1,"kind":"approveAuthorization","owner":"sid:S-1-5-18","authorization":"{proof}"}}"#),
        format!(r#"{{"version":1,"kind":"approveAuthorization","authorization":"{proof}","authorization":"{proof}"}}"#),
        r#"{"version":1,"kind":"approveAuthorization","authorization":null}"#.into(),
        r#"{"version":1,"kind":"approveAuthorization"}"#.into()] {
        assert!(decode_request(&json).is_err());
    }
}

#[cfg(target_os = "macos")]
#[test]
fn a_real_authorization_reference_without_rights_cannot_authorize_mutation() {
    use eink_bridge::authorization::macos::Authorization;
    let reference = Authorization::empty().unwrap();
    let proof = reference.proof().unwrap();
    assert_eq!(Authorization::verify(&proof).unwrap_err().code, "EAUTHDENIED");
    drop(reference);
    assert_eq!(Authorization::verify(&proof).unwrap_err().code, "EAUTHDENIED");
    let forged = serde_json::from_str(&format!("\"{}\"", "00".repeat(32))).unwrap();
    assert_eq!(Authorization::verify(&forged).unwrap_err().code, "EAUTHDENIED");
}

#[cfg(target_os = "macos")]
#[test]
fn installation_refuses_an_unbundled_executable_before_requesting_authorization() {
    assert_eq!(eink_bridge::authorization::macos::authorize_install().unwrap_err().code, "EPACKAGE");
    if unsafe { libc::geteuid() } != 0 {
        assert_eq!(eink_bridge::authorization::macos::register_right().unwrap_err().code, "EPERM");
    }
}

#[test]
fn streaming_requests_reject_oversized_and_truncated_frames() {
    let frame = eink_bridge::protocol::encode_frame(&HelperRequest::inspect()).unwrap();
    assert!(matches!(transport::read_request(&mut Cursor::new(&frame)).unwrap().request, RequestKind::Inspect));
    assert_eq!(transport::read_request(&mut Cursor::new([0, 16, 0, 1])).unwrap_err().code, "EPROTOCOL");
    assert_eq!(transport::read_request(&mut Cursor::new(&frame[..frame.len()-1])).unwrap_err().code, "EPROTOCOL");
    assert!(eink_bridge::protocol::decode_request(r#"{"version":1,"kind":"inspect","uid":0}"#).is_err());
}

#[test]
fn helper_replies_are_versioned_bounded_and_preserve_native_errors() {
    use eink_bridge::{model::BridgeError, protocol::{HelperReply, ReplyKind}};
    let reply = HelperReply::new(ReplyKind::Error {
        error: BridgeError::new("EAUTHCANCELLED", "Authorization was cancelled."),
    });
    let mut frame = Vec::new();
    transport::write_reply(&mut frame, &reply).unwrap();
    let decoded = transport::read_reply(&mut Cursor::new(&frame)).unwrap();
    assert!(matches!(decoded.reply, ReplyKind::Error { error } if error.code == "EAUTHCANCELLED"));
    assert!(transport::read_reply(&mut Cursor::new(&frame[..frame.len()-1])).is_err());
    assert!(transport::read_reply(&mut Cursor::new([0, 16, 0, 1])).is_err());
    for json in [
        r#"{"version":99,"reply":{"kind":"error","error":{"code":"EIPC","message":"bad"}}}"#,
        r#"{"version":99,"version":1,"reply":{"kind":"error","error":{"code":"EIPC","message":"bad"}}}"#,
        r#"{"version":1,"reply":{"kind":"error","error":{"code":"EIPC","message":"bad"}},"target":"/tmp/untrusted"}"#,
    ] {
        let mut frame = (json.len() as u32).to_be_bytes().to_vec();
        frame.extend(json.as_bytes());
        assert!(transport::read_reply(&mut Cursor::new(frame)).is_err());
    }
    let oversized = HelperReply::new(ReplyKind::Error {
        error: BridgeError::new("EIPC", "a".repeat(eink_bridge::protocol::MAX_MESSAGE)),
    });
    let mut output = Vec::new();
    assert!(transport::write_reply(&mut output, &oversized).is_err());
    assert!(output.is_empty(), "invalid replies must not partially write a frame");
}

#[cfg(unix)]
#[test]
fn unix_peer_identity_is_taken_from_the_kernel() {
    let (client, server) = std::os::unix::net::UnixStream::pair().unwrap();
    let caller = transport::unix::caller(&server).unwrap();
    assert_eq!(caller.principal(), format!("uid:{}", unsafe { libc::geteuid() }));
    assert!(transport::unix::verify_peer(&client, unsafe { libc::geteuid() }).is_ok());
    assert_eq!(transport::unix::verify_peer(&client, unsafe { libc::geteuid() } + 1).unwrap_err().code, "EPERM");
}

#[test]
fn client_accepts_only_the_reply_for_the_requested_operation() {
    use eink_bridge::{model::*, protocol::{HelperReply, ReplyKind}};
    struct Channel { input: Cursor<Vec<u8>>, output: Vec<u8> }
    impl std::io::Read for Channel {
        fn read(&mut self, data: &mut [u8]) -> std::io::Result<usize> { std::io::Read::read(&mut self.input, data) }
    }
    impl std::io::Write for Channel {
        fn write(&mut self, data: &[u8]) -> std::io::Result<usize> { self.output.extend_from_slice(data); Ok(data.len()) }
        fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
    }
    let id = OperationId::new("a".repeat(32)).unwrap();
    let request = HelperRequest { version: VERSION, request: RequestKind::Keep { operation_id: id.clone() } };
    let operation = Operation { id, revision: 4, state: OperationState::Ready, stage: "Ready".into(),
        confirmation_deadline: None, reboot_required: false, error: None };
    let cases = [
        (ReplyKind::Operation { operation: Some(operation.clone()) }, None),
        (ReplyKind::Operation { operation: Some(Operation { id: OperationId::new("b".repeat(32)).unwrap(), ..operation }) }, Some("EPROTOCOL")),
        (ReplyKind::Operation { operation: None }, Some("EPROTOCOL")),
        (ReplyKind::Verification { readiness: Readiness::Ready, selection: None }, Some("EPROTOCOL")),
        (ReplyKind::Error { error: BridgeError::new("EAUTHCANCELLED", "Cancelled") }, Some("EAUTHCANCELLED")),
    ];
    for (reply, expected_error) in cases {
        let mut channel = Channel { input: Cursor::new(eink_bridge::protocol::encode_reply_frame(&HelperReply::new(reply)).unwrap()), output: Vec::new() };
        let result = transport::exchange(&mut channel, &request);
        assert_eq!(result.err().map(|e| e.code), expected_error.map(str::to_owned));
        assert!(matches!(eink_bridge::protocol::decode_frame(&channel.output).unwrap().request, RequestKind::Keep { .. }));
    }
}

#[cfg(target_os = "macos")]
#[test]
fn native_macos_runtime_parent_is_accepted_but_user_directories_are_not() {
    use eink_bridge::transport::unix::validate_system_directory;
    validate_system_directory(std::path::Path::new("/var/run")).unwrap();
    let user_directory = tempfile::tempdir().unwrap();
    assert_eq!(validate_system_directory(user_directory.path()).unwrap_err().code, "EPERM");
}

#[cfg(target_os = "linux")]
#[test]
fn linux_authorization_uses_the_kernel_process_and_start_time() {
    use std::os::unix::net::UnixStream;
    let (socket, _) = UnixStream::pair().unwrap();
    let caller = eink_bridge::transport::unix::caller(&socket).unwrap();
    let process = caller.process();
    assert_eq!(process.pid(), std::process::id());
    assert_eq!(process.uid(), unsafe { libc::geteuid() });
    assert!(process.start_time() > 0);
    process.revalidate().unwrap();
}
