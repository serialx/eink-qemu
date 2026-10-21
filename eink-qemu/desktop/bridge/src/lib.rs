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
#[cfg(windows)]
#[path = "platform/windows/security.rs"]
pub mod windows_security;
#[cfg(windows)]
#[path = "platform/windows/device.rs"]
pub mod windows_device;
#[cfg(windows)]
#[path = "platform/windows/command.rs"]
pub mod windows_command;
#[cfg(windows)]
#[path = "platform/windows/topology.rs"]
pub mod windows_topology;
#[cfg(windows)]
#[path = "platform/windows/profile.rs"]
pub mod windows_profile;
#[cfg(windows)]
#[path = "platform/windows/options.rs"]
pub mod windows_options;
#[cfg(windows)]
#[path = "platform/windows/static_config.rs"]
pub mod windows_static;
#[cfg(windows)]
#[path = "platform/windows/network_bridge.rs"]
pub mod windows_network_bridge;
#[cfg(windows)]
#[path = "platform/windows/boot.rs"]
pub mod windows_boot;
#[cfg(windows)]
#[path = "platform/windows/dispatch.rs"]
pub mod windows_dispatch;
#[cfg(windows)]
#[path = "platform/windows/server.rs"]
pub mod windows_server;
#[cfg(windows)]
#[path = "platform/windows/scm.rs"]
pub mod windows_scm;
#[cfg(windows)]
#[path = "platform/windows/membership.rs"]
pub mod windows_membership;
#[cfg(windows)]
#[path = "platform/windows/elevation.rs"]
pub mod windows_elevation;
#[cfg(windows)]
#[path = "platform/windows/driver.rs"]
pub mod windows_driver;
#[cfg(windows)]
#[path = "platform/windows/verify.rs"]
pub mod windows_verify;
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
#[cfg(target_os = "linux")]
#[path = "platform/linux/verify.rs"]
pub mod linux_verify;
#[cfg(target_os = "linux")]
#[path = "platform/linux/transaction.rs"]
pub mod linux_transaction;
#[cfg(target_os = "linux")]
#[path = "platform/linux/network.rs"]
pub mod linux_network;
#[cfg(target_os = "linux")]
#[path = "platform/linux/coordinator.rs"]
pub mod linux_platform;
#[cfg(target_os = "linux")]
#[path = "platform/linux/dispatch.rs"]
pub mod linux_dispatch;
#[cfg(target_os = "linux")]
#[path = "platform/linux/server.rs"]
pub mod linux_server;
