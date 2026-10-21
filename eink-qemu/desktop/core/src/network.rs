//! Network targets resolved by native setup. These are not WebView launch inputs.
use crate::session::{AppError, WifiMode};
use std::path::PathBuf;

/// QEMU owns a duplicate in its fdset for its entire process lifetime, even if
/// the desktop is killed. Only the child's descriptor loses close-on-exec.
#[cfg(unix)]
pub struct InheritedLease { file: std::sync::Arc<std::fs::File> }
#[cfg(unix)]
impl InheritedLease {
    pub fn new(file: &std::fs::File) -> Result<Self, AppError> {
        use std::os::fd::{AsRawFd, FromRawFd};
        let fd = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 3) };
        if fd < 0 { return Err(AppError::new("EBRIDGELEASE", "Cannot retain the bridge transport lease.")); }
        Ok(Self { file: std::sync::Arc::new(unsafe { std::fs::File::from_raw_fd(fd) }) })
    }
    pub fn arguments(&self) -> [String; 2] {
        use std::os::fd::AsRawFd;
        ["-add-fd".into(), format!("fd={},set=19472,opaque=eink-bridge-lease", self.file.as_raw_fd())]
    }
    pub fn configure(&self, command: &mut std::process::Command) {
        use std::os::{fd::AsRawFd, unix::process::CommandExt};
        let file = self.file.clone();
        unsafe { command.pre_exec(move || {
            // fcntl is async-signal-safe; this executes only in the forked child.
            if libc::fcntl(file.as_raw_fd(), libc::F_SETFD, 0) < 0 { return Err(std::io::Error::last_os_error()); }
            Ok(())
        }); }
    }
}

/// Windows QEMU retains its standard input handle until process exit. The
/// desktop supplies input through QMP, so an empty read-only lease can occupy
/// stdin without consuming a device-input channel. Command duplicates only the
/// child's standard handle; the parent's original remains non-inheritable.
#[cfg(windows)]
pub struct InheritedLease { file: std::fs::File }
#[cfg(windows)]
impl InheritedLease {
    pub fn new(file: &std::fs::File) -> Result<Self, AppError> {
        file.try_clone().map(|file| Self { file })
            .map_err(|_| AppError::new("EBRIDGELEASE", "Cannot retain the bridge transport lease."))
    }
    pub fn configure(self, command: &mut std::process::Command) {
        command.stdin(std::process::Stdio::from(self.file));
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetworkTarget {
    LinuxBridge { name: String, helper: PathBuf },
    MacSocket { path: PathBuf },
    WindowsTap { name: String },
}
#[derive(Debug, Clone, Default)]
pub struct NetworkOverrides { pub net: Option<String>, pub bridge: Option<String> }
impl NetworkOverrides {
    pub fn from_env() -> Self {
        Self { net: std::env::var("X4NET").ok(), bridge: std::env::var("X4BR").ok().filter(|s| !s.is_empty()) }
    }
}
fn escape(value: &str) -> String { value.replace(',', ",,") }
fn checked(value: &str) -> Result<&str, AppError> {
    if value.is_empty() || value.chars().any(char::is_control) {
        Err(AppError::new("EINVAL", "Invalid bridge network target."))
    } else { Ok(value) }
}
fn path_text(path: &std::path::Path) -> Result<&str, AppError> {
    if !path.is_absolute() { return Err(AppError::new("EINVAL", "Bridge paths must be absolute.")); }
    checked(path.to_str().ok_or_else(|| AppError::new("EINVAL", "Bridge paths must be valid UTF-8."))?)
}

/// None requests legacy platform selection (including bundled helper lookup).
/// Some(empty) is deliberately different: X4NET="" supplies no network arguments.
pub fn args(mode: WifiMode, target: Option<&NetworkTarget>, overrides: &NetworkOverrides) -> Result<Option<Vec<String>>, AppError> {
    if let Some(net) = &overrides.net { return Ok(Some(net.split_whitespace().map(String::from).collect())); }
    if mode == WifiMode::Nat { return Ok(Some(vec!["-nic".into(), "user,model=esp32_wifi".into()])); }
    if overrides.bridge.as_ref().is_some_and(|s| !s.is_empty()) { return Ok(None); }
    let Some(target) = target else { return Ok(None); };
    let nic = match target {
        NetworkTarget::MacSocket { path } => format!("stream,server=off,addr.type=unix,addr.path={},model=esp32_wifi", escape(path_text(path)?)),
        NetworkTarget::WindowsTap { name } => format!("tap,ifname={},model=esp32_wifi", escape(checked(name)?)),
        NetworkTarget::LinuxBridge { name, helper } => {
            // QEMU interpolates bridge names into its helper shell command.
            if name.is_empty() || name.len() > 15 || !name.bytes().all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c)) {
                return Err(AppError::new("EINVAL", "Invalid Linux bridge interface name."));
            }
            let helper = path_text(helper)?;
            if helper.contains("--br=") { return Err(AppError::new("EINVAL", "Invalid bridge helper path.")); }
            let helper = if helper.contains([' ', '\t']) { format!("'{}'", helper.replace('\'', "'\\''")) } else { helper.into() };
            format!("bridge,br={name},model=esp32_wifi,helper={}", escape(&helper))
        },
    };
    Ok(Some(vec!["-nic".into(), nic]))
}
