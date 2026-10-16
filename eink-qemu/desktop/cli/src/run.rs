//! `eink-qemu run`: one firmware, one session, the steps, the records.

use crate::launch::{self, Prepared};
use crate::output::Printer;
use crate::{RunArgs, EXIT_FAIL, EXIT_SETUP, EXIT_USAGE};
use eink_core::script::events::RunEvents;
use eink_core::script::steps::{self, Step};
use eink_core::script::{metrics, RunOptions, Runner};
use regex::Regex;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc::UnboundedSender;

/// Extra receivers of records (a `do` client while it waits).
pub type Tap = Arc<Mutex<Option<UnboundedSender<Value>>>>;

pub async fn run(firmware: PathBuf, steps: Vec<String>, args: RunArgs) -> u8 {
    if args.gui {
        return crate::gui::run(firmware, steps, args).await;
    }
    let printer = Arc::new(Printer::new(args.quiet, args.progress));
    let steps = match collect_steps(&args, &steps) {
        Ok(steps) => steps,
        Err(e) => return usage(&printer, &e),
    };
    let dir = match launch::run_dir_in(args.out.as_deref(), &firmware, "", args.ready_file.is_some()) {
        Ok(dir) => dir,
        Err(e) => return usage(&printer, &e),
    };
    execute(&firmware, &args, dir, None, printer, &steps).await.code
}

pub fn usage(printer: &Printer, error: &str) -> u8 {
    printer.record(&json!({"type": "error", "stage": "usage", "error": error}));
    EXIT_USAGE
}

/// The steps of a run: --script's lines, then the command line's.
pub fn collect_steps(args: &RunArgs, tokens: &[String]) -> Result<Vec<Step>, String> {
    let mut all = Vec::new();
    if let Some(script) = &args.script {
        let text = if script.as_os_str() == "-" {
            std::io::read_to_string(std::io::stdin()).map_err(|e| format!("cannot read steps from stdin: {e}"))?
        } else {
            std::fs::read_to_string(script).map_err(|e| format!("--script {}: {e}", script.display()))?
        };
        all.extend(steps::script_lines(&text));
    }
    all.extend(tokens.iter().cloned());
    for token in &all {
        if token.starts_with("--") {
            return Err(format!("`{token}` is not a step; it looks like an option, but no such option exists here (`eink-qemu run --help`)"));
        }
    }
    steps::parse(&all)
}

pub struct Outcome {
    pub code: u8,
    #[allow(dead_code)]
    pub dir: PathBuf,
    #[allow(dead_code)]
    pub end: Value,
}

impl Outcome {
    fn with_code(mut self, code: u8) -> Self {
        self.code = code;
        self
    }
}

fn options(args: &RunArgs, p: &Prepared) -> Result<RunOptions, String> {
    let mut opts = RunOptions::new(p.dir.clone());
    opts.timeout = args.timeout;
    opts.ready_timeout = args.ready_timeout;
    opts.settle = Duration::from_millis(args.settle.max(20));
    opts.panel_idle = Duration::from_millis(args.panel_idle);
    opts.elf = p.elf.clone();
    opts.frames = !args.no_frames;
    if args.no_crash_check {
        opts.crash_patterns.clear();
    }
    for pattern in &args.crash_pattern {
        opts.crash_patterns.push(Regex::new(pattern).map_err(|e| format!("--crash-pattern {pattern}: {e}"))?);
    }
    Ok(opts)
}

/// Prepare, start, run the steps (then serve `do` with --hold), stop.
pub async fn execute(firmware: &Path, args: &RunArgs, dir: PathBuf, side: Option<String>, printer: Arc<Printer>, steps: &[Step]) -> Outcome {
    let started = Instant::now();
    let side_field = |mut v: Value| {
        if let Some(side) = &side {
            v["side"] = json!(side);
        }
        v
    };
    let setup_error = |printer: &Printer, dir: &Path, stage: &str, error: String| {
        let _ = std::fs::write(dir.join("error.txt"), &error);
        let record = side_field(json!({"type": "error", "stage": stage, "error": error, "run_dir": dir}));
        printer.record(&record);
        let ready = args.ready_file.clone();
        if let Some(path) = ready {
            let _ = std::fs::write(path, json!({"ok": false, "error": record["error"]}).to_string());
        }
        Outcome { code: EXIT_SETUP, dir: dir.to_path_buf(), end: record }
    };
    let mut prepared = match launch::prepare(args, firmware, dir.clone()) {
        Ok(p) => p,
        Err(e) => return setup_error(&printer, &dir, "setup", e),
    };
    if let Err(e) = eink_core::script::validate(steps, &prepared.flash.machine, prepared.trace.is_ok()) {
        return setup_error(&printer, &dir, "usage", e).with_code(EXIT_USAGE);
    }
    let events = match RunEvents::new(&dir, args.echo_log) {
        Ok(e) => Arc::new(e),
        Err(e) => return setup_error(&printer, &dir, "setup", e),
    };
    let session = match launch::launch(&mut prepared, events.clone()).await {
        Ok(s) => s,
        Err(e) => return setup_error(&printer, &dir, "qemu", e),
    };
    let opts = match options(args, &prepared) {
        Ok(o) => o,
        Err(e) => {
            let _ = session.stop().await;
            return setup_error(&printer, &dir, "usage", e);
        }
    };
    let tap: Tap = Arc::default();
    let sink = {
        let printer = printer.clone();
        let side = side.clone();
        let tap = tap.clone();
        Box::new(move |record: &Value| {
            let mut record = record.clone();
            if let Some(side) = &side {
                record["side"] = json!(side);
            }
            printer.record(&record);
            if let Some(tx) = tap.lock().unwrap().as_ref() {
                let _ = tx.send(record);
            }
        })
    };
    let mut runner = match Runner::new(session.clone(), events.clone(), prepared.guest_clock, opts, sink) {
        Ok(r) => r,
        Err(e) => {
            let _ = session.stop().await;
            return setup_error(&printer, &dir, "setup", e);
        }
    };
    let start = launch::start_record(&prepared, &session, None);
    let _ = std::fs::write(dir.join("run.json"), serde_json::to_string_pretty(&start).unwrap_or_default());
    runner.emit(&start);
    crate::control::remember_trace_starts(&mut runner.trace_names, steps);
    let mut ok = runner.run(steps).await;
    if args.hold {
        ok = crate::control::serve(&mut runner, args, tap.clone(), ok, &printer).await && ok;
    }
    let end = finish(&mut runner, &session, ok, started).await;
    let code = if ok { 0 } else { EXIT_FAIL };
    Outcome { code, dir, end }
}

/// Stop the session and write the run's summary files and end record.
pub async fn finish(runner: &mut Runner, session: &Arc<eink_core::session::Session>, ok: bool, started: Instant) -> Value {
    // Let the last frames settle into frames/ before stopping.
    tokio::time::sleep(Duration::from_millis(50)).await;
    runner.finish();
    let dir = runner.dir().to_path_buf();
    // The allocation log lives in the session's temporary directory: keep it.
    if let Some(Ok(log)) = session.trace_log() {
        let _ = std::fs::copy(&log, dir.join("trace.bin"));
    }
    let _ = session.stop().await;
    let summary = runner.events.with_lines(0, |lines| metrics::summarize(lines.iter().map(|l| l.text.as_str())));
    let _ = std::fs::write(dir.join("metrics.json"), serde_json::to_string_pretty(&summary).unwrap_or_default());
    let frames = runner.frames.frames();
    let _ = std::fs::write(dir.join("frames.json"), serde_json::to_string_pretty(&frames).unwrap_or_default());
    let report = crate::report::write(&dir).ok();
    let failed = runner.failed.clone();
    let end = json!({
        "type": "end",
        "ok": ok,
        "steps": runner.index,
        "failed_step": failed.as_ref().map(|f| f["i"].clone()),
        "error": failed.as_ref().map(|f| f["error"].clone()),
        "duration_s": (started.elapsed().as_secs_f64() * 10.0).round() / 10.0,
        "frames": frames.len(),
        "metrics": {
            "page_renders": summary["page_renders"],
            "bw_render_ms_median": summary["bw_render_ms"]["median"],
            "total_render_ms_median": summary["total_render_ms"]["median"],
            "mem_min_free": summary["mem_min_free"],
        },
        "run_dir": dir,
        "report": report,
    });
    runner.emit(&end);
    end
}
