//! Bounded messages and OS-authenticated caller identities. Authorization for
//! mutation is separate: possession of a local connection grants no privilege.
use crate::{model::{BridgeError, Readiness}, protocol::{self, HelperReply, HelperRequest, ReplyKind, RequestKind, MAX_MESSAGE}};
use std::io::{Read, Write};

#[cfg(unix)]
pub mod unix;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caller { principal: String }
impl Caller { pub fn principal(&self) -> &str { &self.principal } }

fn invalid() -> BridgeError { BridgeError::new("EPROTOCOL", "Incomplete or invalid bridge helper message.") }
fn read_payload(reader: &mut impl Read) -> Result<Vec<u8>, BridgeError> {
    let mut header = [0; 4];
    reader.read_exact(&mut header).map_err(|_| invalid())?;
    let length = u32::from_be_bytes(header) as usize;
    // Reject before allocating or waiting for the claimed body.
    if length == 0 || length > MAX_MESSAGE { return Err(invalid()); }
    let mut payload = vec![0; length];
    reader.read_exact(&mut payload).map_err(|_| invalid())?;
    Ok(payload)
}
pub fn read_request(reader: &mut impl Read) -> Result<HelperRequest, BridgeError> {
    protocol::decode_request(std::str::from_utf8(&read_payload(reader)?).map_err(|_| invalid())?)
}
pub fn read_reply(reader: &mut impl Read) -> Result<HelperReply, BridgeError> {
    protocol::decode_reply(&read_payload(reader)?)
}
pub fn write_reply(writer: &mut impl Write, reply: &HelperReply) -> Result<(), BridgeError> {
    writer.write_all(&protocol::encode_reply_frame(reply)?).and_then(|_| writer.flush())
        .map_err(|e| BridgeError::new("EIPC", e.to_string()))
}

/// A request uses one authenticated connection. Consuming the client prevents
/// reuse of a stream after a truncated or otherwise invalid response.
pub struct HelperClient { stream: Box<dyn Channel + Send> }
trait Channel: Read + Write {}
impl<T: Read + Write> Channel for T {}
impl HelperClient {
    pub fn connect() -> Result<Self, BridgeError> {
        #[cfg(unix)]
        { Ok(Self { stream: Box::new(unix::connect()?) }) }
        #[cfg(not(unix))]
        { Err(BridgeError::new("ENEEDSSETUP", "The native bridge helper connection is not available.")) }
    }
    pub fn request(mut self, request: HelperRequest) -> Result<HelperReply, BridgeError> {
        exchange(&mut self.stream, &request)
    }
}

pub fn exchange(channel: &mut (impl Read + Write), request: &HelperRequest) -> Result<HelperReply, BridgeError> {
    write_request(channel, request)?;
    let reply = read_reply(channel)?;
    if let ReplyKind::Error { error } = &reply.reply { return Err(error.clone()); }
    let matches = match (&request.request, &reply.reply) {
        (RequestKind::Inspect, ReplyKind::Inspection { .. }) => true,
        (RequestKind::Plan { request }, ReplyKind::Plan { plan }) => plan.request == *request,
        (RequestKind::Apply { plan_id: id }, ReplyKind::Operation { operation: Some(operation) })
        | (RequestKind::Keep { operation_id: id }, ReplyKind::Operation { operation: Some(operation) })
        | (RequestKind::Cancel { operation_id: id }, ReplyKind::Operation { operation: Some(operation) }) => operation.id == *id,
        (RequestKind::Recover, ReplyKind::Operation { .. }) => true,
        (RequestKind::Verify { selection: expected }, ReplyKind::Verification { selection, readiness }) => {
            selection.as_ref().is_none_or(|selection| selection == expected)
                && (*readiness != Readiness::Ready || selection.is_some())
        },
        _ => false,
    };
    if !matches { return Err(invalid()); }
    Ok(reply)
}
pub fn write_request(writer: &mut impl Write, request: &HelperRequest) -> Result<(), BridgeError> {
    writer.write_all(&protocol::encode_frame(request)?).and_then(|_| writer.flush())
        .map_err(|e| BridgeError::new("EIPC", e.to_string()))
}
