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
