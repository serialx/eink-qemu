//! eink-qemu's emulator core, shared by the desktop app (src-tauri) and the
//! command-line tool (cli): QEMU sessions and their QMP monitor, the eink
//! display stream, heap snapshots and allocation traces, SD card images,
//! flash images, and scripted runs.

pub mod cards;
pub mod display;
pub mod firmware;
pub mod flash;
pub mod heap;
pub mod heapview;
pub mod host;
pub mod qmp;
pub mod script;
pub mod sd;
pub mod session;
pub mod trace;
