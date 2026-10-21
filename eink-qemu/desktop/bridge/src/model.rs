use serde::{Deserialize, Serialize};

pub const VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BridgeError {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}
impl BridgeError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self { code: code.into(), message: message.into(), detail: None }
    }
}
impl std::fmt::Display for BridgeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "{}", self.message) }
}
impl std::error::Error for BridgeError {}

macro_rules! identity {
    ($name:ident, $valid:expr) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);
        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, BridgeError> {
                let value = value.into();
                if !($valid)(&value) { return Err(BridgeError::new("EINVAL", "Invalid bridge identity.")); }
                Ok(Self(value))
            }
            pub fn as_str(&self) -> &str { &self.0 }
        }
        impl TryFrom<String> for $name {
            type Error = BridgeError;
            fn try_from(value: String) -> Result<Self, Self::Error> { Self::new(value) }
        }
        impl From<$name> for String { fn from(value: $name) -> String { value.0 } }
    };
}
identity!(AdapterId, |s: &str| !s.is_empty() && s.len() <= 512 && !s.chars().any(char::is_control));
identity!(OperationId, |s: &str| s.len() == 32 && s.bytes().all(|c| c.is_ascii_hexdigit()));
identity!(ResourceId, |s: &str| s.len() == 32 && s.bytes().all(|c| c.is_ascii_hexdigit()));

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Platform { Macos, Linux, Windows }
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AdapterKind { Ethernet, Wifi, Bridge, Tap, Other }
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Adapter {
    pub id: AdapterId,
    pub name: String,
    pub kind: AdapterKind,
    pub link_up: bool,
    pub eligible: bool,
    pub reason: Option<String>,
    pub fingerprint: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BridgeSelection {
    pub version: u32,
    pub platform: Platform,
    pub adapter_id: AdapterId,
    pub fingerprint: String,
    pub resource_id: Option<ResourceId>,
    pub display_name: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Readiness { NeedsSetup, Ready, Disconnected, NeedsRepair, Unsupported }

/// This deliberately cannot return Ready: discovery has not opened the transport.
pub fn selection_readiness(selection: &BridgeSelection, platform: Platform, adapters: &[Adapter]) -> Readiness {
    if selection.version != VERSION || selection.platform != platform { return Readiness::NeedsSetup; }
    match adapters.iter().find(|a| a.id == selection.adapter_id) {
        None => Readiness::Disconnected,
        Some(a) if a.fingerprint != selection.fingerprint => Readiness::NeedsRepair,
        Some(a) if !a.eligible => Readiness::Unsupported,
        Some(a) if !a.link_up => Readiness::Disconnected,
        Some(_) => Readiness::NeedsSetup,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum PlanRequest {
    Configure { #[serde(rename = "adapterId")] adapter_id: AdapterId },
    Repair { #[serde(rename = "resourceId")] resource_id: ResourceId },
    Remove { #[serde(rename = "resourceId")] resource_id: ResourceId },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BridgePlan {
    pub id: OperationId,
    pub request: PlanRequest,
    pub fingerprint: String,
    pub changes: Vec<String>,
    pub requires_authorization: bool,
    pub may_disconnect: bool,
    pub expires_at: u64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OperationState {
    Inspecting, AwaitingAuthorization, Applying, Verifying, Ready, RollingBack, Cancelled, Failed,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Operation {
    pub id: OperationId,
    pub revision: u64,
    pub state: OperationState,
    pub stage: String,
    pub confirmation_deadline: Option<u64>,
    pub reboot_required: bool,
    pub error: Option<BridgeError>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Capabilities {
    pub create: bool,
    pub existing_bridge: bool,
    pub reason: Option<String>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SupportState { Missing, Ready, NeedsUpdate }
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Inspection {
    pub support: SupportState,
    pub platform: Platform,
    pub adapters: Vec<Adapter>,
    pub capabilities: Capabilities,
    pub selection: Option<BridgeSelection>,
    pub readiness: Readiness,
    pub active_operation: Option<Operation>,
    pub external_override: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum LaunchTarget {
    LinuxBridge { name: String, helper: std::path::PathBuf },
    MacSocket { path: std::path::PathBuf },
    WindowsTap { name: String },
}
