//! Bounded messages and OS-authenticated caller identities. Authorization for
//! mutation is separate: possession of a local connection grants no privilege.
use crate::{model::{BridgeError, BridgeSelection, LaunchTarget, Platform, Readiness}, protocol::{self, HelperReply, HelperRequest, ReplyKind, RequestKind, MAX_MESSAGE}};
use std::io::{Read, Write};

#[cfg(unix)]
pub mod unix;
#[cfg(windows)]
pub mod windows;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caller {
    principal: String,
    #[cfg(target_os = "linux")]
    process: unix::ProcessIdentity,
}
impl Caller {
    pub fn principal(&self) -> &str { &self.principal }
    #[cfg(target_os = "linux")]
    pub fn process(&self) -> &unix::ProcessIdentity { &self.process }
    #[cfg(unix)]
    pub fn uid(&self) -> Result<u32, BridgeError> {
        self.principal.strip_prefix("uid:").and_then(|uid| uid.parse().ok())
            .ok_or_else(|| BridgeError::new("EPERM", "Missing authenticated Unix caller."))
    }
}

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
/// Native session ownership keeps the lease alive even if the helper restarts.
pub struct TransportLease { pub target: LaunchTarget, _file: std::fs::File }
impl TransportLease {
    pub fn into_parts(self) -> (LaunchTarget, std::fs::File) { (self.target, self._file) }
}
trait Channel: Read + Write {
    fn response_timeout(&self, timeout: std::time::Duration) -> std::io::Result<()>;
    fn acknowledge(&mut self) -> Result<(), BridgeError> { Ok(()) }
}
#[cfg(unix)]
impl Channel for std::os::unix::net::UnixStream {
    fn response_timeout(&self, timeout: std::time::Duration) -> std::io::Result<()> { self.set_read_timeout(Some(timeout)) }
}
impl HelperClient {
    pub fn acquire(selection: BridgeSelection) -> Result<TransportLease, BridgeError> {
        #[cfg(unix)] {
            use std::os::unix::fs::MetadataExt;
            let mut stream = unix::connect()?;
            let reply = exchange(&mut stream, &HelperRequest { version: crate::model::VERSION, request: RequestKind::Acquire { selection } })?;
            let ReplyKind::Lease { target, .. } = reply.reply else { return Err(invalid()); };
            let file = unix::receive_lease_file(&stream)?;
            if file.metadata().map_err(|_| invalid())?.uid() != 0 { return Err(invalid()); }
            Ok(TransportLease { target, _file: file })
        }
        #[cfg(windows)] {
            let mut stream = windows::connect()?;
            stream.timeout(std::time::Duration::from_secs(30));
            let reply = exchange(&mut stream, &HelperRequest { version: crate::model::VERSION, request: RequestKind::Acquire { selection } })?;
            let ReplyKind::Lease { target, .. } = reply.reply else { return Err(invalid()); };
            let file = stream.receive_lease()?;
            stream.acknowledge()?;
            Ok(TransportLease { target, _file: file })
        }
        #[cfg(not(any(unix, windows)))] {
            let _ = selection;
            Err(BridgeError::new("ENEEDSSETUP", "The native bridge lease connection is not available."))
        }
    }
    pub fn connect() -> Result<Self, BridgeError> {
        #[cfg(unix)]
        { Ok(Self { stream: Box::new(unix::connect()?) }) }
        #[cfg(windows)]
        { Ok(Self { stream: Box::new(windows::connect()?) }) }
        #[cfg(not(any(unix, windows)))]
        { Err(BridgeError::new("ENEEDSSETUP", "The native bridge helper connection is not available.")) }
    }
    pub fn request(mut self, request: HelperRequest) -> Result<HelperReply, BridgeError> {
        // Linux authorization runs in the helper and can wait for its native
        // desktop prompt. Keep inspection responsive while allowing a reviewed
        // mutation to finish authentication and bounded native setup.
        let seconds = match request.request {
            RequestKind::Apply { .. } => 300,
            RequestKind::Keep { .. } | RequestKind::Cancel { .. } => 180,
            _ => 30,
        };
        self.stream.response_timeout(std::time::Duration::from_secs(seconds)).map_err(|error| BridgeError::new("EIPC", error.to_string()))?;
        let result = exchange(&mut self.stream, &request);
        let acknowledged = self.stream.acknowledge();
        // Preserve a useful native error even if its sender closed early.
        match result { Ok(reply) => { acknowledged?; Ok(reply) }, Err(error) => Err(error) }
    }
}

pub fn exchange(channel: &mut (impl Read + Write), request: &HelperRequest) -> Result<HelperReply, BridgeError> {
    write_request(channel, request)?;
    let reply = read_reply(channel)?;
    if let ReplyKind::Error { error } = &reply.reply { return Err(error.clone()); }
    let matches = match (&request.request, &reply.reply) {
        (RequestKind::Inspect, ReplyKind::Inspection { .. }) => true,
        (RequestKind::Plan { request }, ReplyKind::Plan { plan }) => plan.request == *request,
        (RequestKind::Apply { plan_id: id, .. }, ReplyKind::Operation { operation: Some(operation) })
        | (RequestKind::Keep { operation_id: id }, ReplyKind::Operation { operation: Some(operation) })
        | (RequestKind::Cancel { operation_id: id }, ReplyKind::Operation { operation: Some(operation) }) => operation.id == *id,
        (RequestKind::Recover, ReplyKind::Operation { .. }) => true,
        (RequestKind::Acquire { selection: expected }, ReplyKind::Lease { selection, target }) => {
            expected == selection && matches!((selection.platform, target),
                (Platform::Macos, LaunchTarget::MacSocket { .. }) | (Platform::Linux, LaunchTarget::LinuxBridge { .. })
                | (Platform::Windows, LaunchTarget::WindowsTap { .. }))
        },
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
