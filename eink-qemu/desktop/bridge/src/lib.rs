//! Native bridge setup, separate from the desktop UI and emulator process.
pub mod model;
pub mod protocol;
mod platform;
pub use platform::inspect;
mod journal;
pub mod coordinator;
pub mod recovery;
pub mod transport;
pub mod authorization;
#[cfg(target_os = "macos")]
#[path = "platform/macos/service.rs"]
pub mod macos_service;
#[cfg(target_os = "macos")]
#[path = "platform/macos/coordinator.rs"]
pub mod macos_platform;
#[cfg(target_os = "macos")]
#[path = "platform/macos/dispatch.rs"]
pub mod macos_dispatch;
#[cfg(target_os = "macos")]
#[path = "platform/macos/server.rs"]
pub mod macos_server;
#[cfg(target_os = "linux")]
#[path = "platform/linux/profiles.rs"]
pub mod linux_profiles;
#[cfg(target_os = "linux")]
#[path = "platform/linux/network_manager.rs"]
pub mod linux_network_manager;
#[cfg(target_os = "linux")]
#[path = "platform/linux/authorization.rs"]
pub mod linux_authorization;
#[cfg(target_os = "linux")]
#[path = "platform/linux/helper.rs"]
pub mod linux_helper;
