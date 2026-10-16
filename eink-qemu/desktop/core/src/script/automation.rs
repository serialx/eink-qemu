//! A scripted run inside the desktop app (`eink-qemu run --gui`): the
//! command-line tool prepares everything (flash, card, ELF, QEMU arguments)
//! and writes this spec; the app starts the session from it, shows it in its
//! window, and runs the steps with the same runner, printing records on its
//! stdout for the tool to relay.

use super::events::RunEvents;
use super::{steps, Recorder, RunOptions, Runner};
use crate::session::{Session, StartOptions, TracePlan};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

#[derive(Serialize, Deserialize, Clone)]
pub struct Automation {
    pub dir: PathBuf,
    pub firmware: PathBuf,
    pub machine: String,
    /// The composed 16 MiB flash image.
    pub flash: PathBuf,
    pub card: PathBuf,
    pub elf: Option<PathBuf>,
    pub options: StartOptions,
    /// The heaptrace plugin and the ELF it reads, when tracing.
    pub trace: Option<(PathBuf, PathBuf)>,
    pub guest_clock: bool,
    pub timeout: f64,
    pub ready_timeout: f64,
    pub settle_ms: u64,
    pub panel_idle_ms: u64,
    #[serde(default = "default_input_quiet")]
    pub input_quiet_ms: u64,
    pub crash_patterns: Vec<String>,
    pub frames: bool,
    pub steps: Vec<String>,
    /// The start record, without the QEMU command line.
    pub start: Value,
}

fn default_input_quiet() -> u64 {
    200
}

/// What the app does when its window closes after a scripted run: the
/// final frame index and the allocation log go into the run directory
/// before the session (and its temporary directory) ends.
pub struct Finalizer {
    dir: PathBuf,
    frames: Arc<Recorder>,
    session: Arc<Session>,
}

impl Finalizer {
    pub fn finalize(&self) {
        self.frames.stop();
        let frames = self.frames.frames();
        let _ = std::fs::write(self.dir.join("frames.json"), serde_json::to_string_pretty(&frames).unwrap_or_default());
        if let Some(Ok(log)) = self.session.trace_log() {
            let _ = std::fs::copy(log, self.dir.join("trace.bin"));
        }
    }
}

impl Automation {
    pub fn load(path: &Path) -> Result<Automation, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn trace_plan(&self) -> TracePlan {
        self.trace.clone().ok_or_else(|| "run with --trace to record allocations".to_string())
    }

    fn options(&self) -> Result<RunOptions, String> {
        let mut opts = RunOptions::new(self.dir.clone());
        opts.timeout = self.timeout;
        opts.ready_timeout = self.ready_timeout;
        opts.settle = Duration::from_millis(self.settle_ms);
        opts.panel_idle = Duration::from_millis(self.panel_idle_ms);
        opts.input_quiet = Duration::from_millis(self.input_quiet_ms);
        opts.elf = self.elf.clone();
        opts.frames = self.frames;
        opts.crash_patterns = self.crash_patterns.iter().map(|p| Regex::new(p).map_err(|e| e.to_string())).collect::<Result<_, _>>()?;
        Ok(opts)
    }
}

fn print(record: &Value) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{record}");
    let _ = out.flush();
}

/// Start the session and run the steps; records go to run.jsonl and stdout.
/// Ends with {"type":"steps-done","ok":…}; the session keeps running for the
/// window until the app quits, when `finalizer` (set once the run started)
/// keeps the frame index and allocation log.
pub async fn run(session: Arc<Session>, events: Arc<RunEvents>, spec: Automation, finalizer: Arc<std::sync::Mutex<Option<Finalizer>>>) -> bool {
    let fail = |error: String| {
        print(&json!({"type": "error", "stage": "qemu", "error": error}));
        print(&json!({"type": "steps-done", "ok": false, "setup_failed": true}));
        false
    };
    let parsed = match steps::parse(&spec.steps) {
        Ok(p) => p,
        Err(e) => return fail(e),
    };
    let flash = match std::fs::read(&spec.flash) {
        Ok(f) => f,
        Err(e) => return fail(format!("{}: {e}", spec.flash.display())),
    };
    if let Err(e) = session.start(spec.options.clone(), flash, spec.trace_plan()).await {
        return fail(format!("QEMU did not start: {}", e.message));
    }
    let opts = match spec.options() {
        Ok(o) => o,
        Err(e) => return fail(e),
    };
    let mut runner = match Runner::new(session.clone(), events, spec.guest_clock, opts, Box::new(|r: &Value| print(r))) {
        Ok(r) => r,
        Err(e) => return fail(e),
    };
    *finalizer.lock().unwrap() = Some(Finalizer { dir: spec.dir.clone(), frames: runner.frames.clone(), session: session.clone() });
    let mut start = spec.start.clone();
    start["qemu"] = json!(session.command());
    start["gui"] = json!(true);
    let _ = std::fs::write(spec.dir.join("run.json"), serde_json::to_string_pretty(&start).unwrap_or_default());
    runner.emit(&start);
    let ok = runner.run(&parsed).await;
    // Let the last screen settle into frames/.
    tokio::time::sleep(Duration::from_millis(600)).await;
    let frames = runner.frames.frames();
    let _ = std::fs::write(spec.dir.join("frames.json"), serde_json::to_string_pretty(&frames).unwrap_or_default());
    runner.events.flush();
    print(&json!({"type": "steps-done", "ok": ok, "steps": runner.index, "failed": runner.failed, "screen": runner.latest_frame().map(|f| spec.dir.join(f.file))}));
    // Dropping the runner leaves its frame recorder running while the window stays open.
    ok
}
