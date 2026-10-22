//! Strict, versioned helper requests. Decoding is separate from authorization:
//! callers must also authenticate peers and check ownership before dispatch.
use crate::model::*;
use serde::{Deserialize, Serialize};

pub const MAX_MESSAGE: usize = 1024 * 1024;
pub const HELPER_VERSION: &str = env!("CARGO_PKG_VERSION");
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum RequestKind {
    Inspect,
    Plan { request: PlanRequest },
    BeginAuthorization { #[serde(rename = "planId")] plan_id: OperationId },
    ApproveAuthorization { authorization: crate::authorization::AuthorizationProof },
    Apply {
        #[serde(rename = "planId")] plan_id: OperationId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        authorization: Option<crate::authorization::AuthorizationProof>,
    },
    Verify { selection: BridgeSelection },
    Acquire { selection: BridgeSelection },
    Keep { #[serde(rename = "operationId")] operation_id: OperationId },
    Cancel { #[serde(rename = "operationId")] operation_id: OperationId },
    Recover,
}
#[derive(Debug, Clone)]
pub struct HelperRequest {
    pub version: u32,
    pub request: RequestKind,
}
impl Serialize for HelperRequest {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Wire<'a> {
            version: u32,
            helper_version: &'static str,
            #[serde(flatten)]
            request: &'a RequestKind,
        }
        Wire { version: self.version, helper_version: HELPER_VERSION, request: &self.request }.serialize(serializer)
    }
}
impl HelperRequest {
    pub fn inspect() -> Self { Self { version: VERSION, request: RequestKind::Inspect } }
}
/// Launch targets travel only from the authenticated helper to native code,
/// after verification, with a transport lease. Never accept them from WebView input.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum ReplyKind {
    Inspection { inspection: Inspection },
    Plan { plan: BridgePlan },
    Authorization { authorization: crate::authorization::AuthorizationProof },
    Authorized,
    Operation { operation: Option<Operation> },
    Verification { readiness: Readiness, selection: Option<BridgeSelection> },
    Lease { selection: BridgeSelection, target: LaunchTarget },
    Error { error: BridgeError },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HelperReply {
    pub version: u32,
    #[serde(rename = "helperVersion")]
    helper_version: String,
    pub reply: ReplyKind,
}
impl HelperReply {
    pub fn new(reply: ReplyKind) -> Self { Self { version: VERSION, helper_version: HELPER_VERSION.into(), reply } }
}
pub fn decode_reply(payload: &[u8]) -> Result<HelperReply, BridgeError> {
    if payload.len() > MAX_MESSAGE { return Err(invalid()); }
    let reply: HelperReply = serde_json::from_slice(payload).map_err(|_| invalid())?;
    if reply.version != VERSION || reply.helper_version != HELPER_VERSION { return Err(invalid()); }
    Ok(reply)
}
pub fn encode_reply_frame(reply: &HelperReply) -> Result<Vec<u8>, BridgeError> {
    if reply.version != VERSION || reply.helper_version != HELPER_VERSION { return Err(invalid()); }
    let payload = serde_json::to_vec(reply).map_err(|_| invalid())?;
    if payload.is_empty() || payload.len() > MAX_MESSAGE { return Err(invalid()); }
    let mut frame = (payload.len() as u32).to_be_bytes().to_vec();
    frame.extend(payload);
    Ok(frame)
}
fn invalid() -> BridgeError { BridgeError::new("EPROTOCOL", "Invalid bridge helper request.") }

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Envelope {
    version: u32,
    helper_version: String,
    kind: String,
    request: Option<PlanRequest>,
    plan_id: Option<OperationId>,
    selection: Option<BridgeSelection>,
    operation_id: Option<OperationId>,
    #[serde(default, deserialize_with = "present_authorization")]
    authorization: Option<crate::authorization::AuthorizationProof>,
}

fn present_authorization<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<crate::authorization::AuthorizationProof>, D::Error> {
    // A field explicitly set to null is not an absent authorization field.
    crate::authorization::AuthorizationProof::deserialize(deserializer).map(Some)
}

pub fn decode_request(json: &str) -> Result<HelperRequest, BridgeError> {
    if json.len() > MAX_MESSAGE { return Err(invalid()); }
    // Deserialize typed fields directly: going through Value would discard
    // duplicate keys before the request's meaning can be checked.
    let envelope: Envelope = serde_json::from_str(json).map_err(|_| invalid())?;
    // Check package compatibility before constructing any dispatchable intent.
    // This does not change the persisted selection/journal schema version.
    if envelope.version != VERSION || envelope.helper_version != HELPER_VERSION {
        return Err(BridgeError::new("EPROTOCOL", "Bridge helper version does not match the app."));
    }
    if envelope.authorization.is_some() && !matches!(envelope.kind.as_str(), "apply" | "approveAuthorization") { return Err(invalid()); }
    let present = (envelope.request.is_some(), envelope.plan_id.is_some(),
        envelope.selection.is_some(), envelope.operation_id.is_some());
    let request = match (envelope.kind.as_str(), present) {
        ("inspect", (false, false, false, false)) => RequestKind::Inspect,
        ("recover", (false, false, false, false)) => RequestKind::Recover,
        ("plan", (true, false, false, false)) => RequestKind::Plan { request: envelope.request.unwrap() },
        ("beginAuthorization", (false, true, false, false)) => RequestKind::BeginAuthorization { plan_id: envelope.plan_id.unwrap() },
        ("approveAuthorization", (false, false, false, false)) => RequestKind::ApproveAuthorization { authorization: envelope.authorization.ok_or_else(invalid)? },
        ("apply", (false, true, false, false)) => RequestKind::Apply { plan_id: envelope.plan_id.unwrap(), authorization: envelope.authorization },
        ("verify", (false, false, true, false)) => RequestKind::Verify { selection: envelope.selection.unwrap() },
        ("acquire", (false, false, true, false)) => RequestKind::Acquire { selection: envelope.selection.unwrap() },
        ("keep", (false, false, false, true)) => RequestKind::Keep { operation_id: envelope.operation_id.unwrap() },
        ("cancel", (false, false, false, true)) => RequestKind::Cancel { operation_id: envelope.operation_id.unwrap() },
        _ => return Err(invalid()),
    };
    Ok(HelperRequest { version: VERSION, request })
}

pub fn encode_frame(request: &HelperRequest) -> Result<Vec<u8>, BridgeError> {
    let payload = serde_json::to_vec(request).map_err(|_| invalid())?;
    if payload.len() > MAX_MESSAGE || request.version != VERSION { return Err(invalid()); }
    let mut frame = (payload.len() as u32).to_be_bytes().to_vec();
    frame.extend(payload);
    Ok(frame)
}

pub fn decode_frame(frame: &[u8]) -> Result<HelperRequest, BridgeError> {
    let header: [u8; 4] = frame.get(..4).ok_or_else(invalid)?.try_into().map_err(|_| invalid())?;
    let len = u32::from_be_bytes(header) as usize;
    if len > MAX_MESSAGE || frame.len() - 4 != len { return Err(invalid()); }
    decode_request(std::str::from_utf8(&frame[4..]).map_err(|_| invalid())?)
}
