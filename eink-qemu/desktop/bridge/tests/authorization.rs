use eink_bridge::{protocol::{HelperRequest, RequestKind}, transport};
use std::io::Cursor;

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
