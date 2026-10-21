//! Native bridge setup, separate from the desktop UI and emulator process.
pub mod model;
pub mod protocol;
mod platform;
pub use platform::inspect;
mod journal;
pub mod coordinator;
pub mod recovery;
pub mod transport;
