//! Bounded messages and OS-authenticated caller identities. Authorization for
//! mutation is separate: possession of a local connection grants no privilege.
use crate::{model::BridgeError, protocol::{self, HelperReply, HelperRequest, MAX_MESSAGE}};
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
pub fn write_request(writer: &mut impl Write, request: &HelperRequest) -> Result<(), BridgeError> {
    writer.write_all(&protocol::encode_frame(request)?).and_then(|_| writer.flush())
        .map_err(|e| BridgeError::new("EIPC", e.to_string()))
}
