//! eink-qemu's emulator core, shared by the desktop app (src-tauri) and the
//! command-line tool (cli): QEMU sessions and their QMP monitor, the eink
//! display stream, heap snapshots and allocation traces, SD card images,
//! flash images, scripted runs, and what is known about particular firmware
//! (profiles).

pub mod cards;
pub mod display;
pub mod firmware;
pub mod flash;
pub mod heap;
pub mod heapview;
pub mod host;
pub mod profiles;
pub mod qmp;
pub mod script;
pub mod sd;
pub mod session;
pub mod trace;
