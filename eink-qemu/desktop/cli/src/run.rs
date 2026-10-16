//! `eink-qemu run`: one firmware, one session, the steps, the records.

use crate::launch::{self, Prepared};
use crate::output::Printer;
use crate::{RunArgs, EXIT_FAIL, EXIT_INTERRUPTED, EXIT_SETUP, EXIT_USAGE};
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

pub async fn run(firmware: PathBuf, tokens: Vec<String>, args: RunArgs) -> u8 {
    if args.gui {
        return crate::gui::run(firmware, tokens, args).await;
    }
    let printer = Arc::new(Printer::new(args.quiet, args.progress));
    let (_, steps) = match collect_steps(&args, &tokens) {
        Ok(collected) => collected,
        Err(e) => return usage(&printer, &e),
    };
    if let Err(e) = launch::check_args(&args) {
        return usage(&printer, &e);
    }
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

/// The steps of a run, as written and parsed: --steps-file alone when
/// given, else --script's lines, then the command line's.
pub fn collect_steps(args: &RunArgs, tokens: &[String]) -> Result<(Vec<String>, Vec<Step>), String> {
    let mut all = Vec::new();
    if let Some(file) = &args.steps_file {
        let text = std::fs::read_to_string(file).map_err(|e| format!("{}: {e}", file.display()))?;
        all.extend(text.lines().filter(|l| !l.is_empty()).map(String::from));
    } else {
        if let Some(script) = &args.script {
            let text = if script.as_os_str() == "-" {
                std::io::read_to_string(std::io::stdin()).map_err(|e| format!("cannot read steps from stdin: {e}"))?
            } else {
                std::fs::read_to_string(script).map_err(|e| format!("--script {}: {e}", script.display()))?
            };
            all.extend(steps::script_lines(&text));
        }
        all.extend(tokens.iter().cloned());
    }
    for token in &all {
        if token.starts_with("--") {
            return Err(format!("`{token}` is not a step; it looks like an option, but no such option exists here (`eink-qemu run --help`)"));
        }
    }
    let parsed = steps::parse(&all)?;
    Ok((all, parsed))
}

pub struct Outcome {
    pub code: u8,
    #[allow(dead_code)]
    pub dir: PathBuf,
    #[allow(dead_code)]
    pub end: Value,
}

fn options(args: &RunArgs, p: &Prepared) -> Result<RunOptions, String> {
    let mut opts = RunOptions::new(p.dir.clone());
    opts.timeout = args.timeout;
    opts.ready_timeout = args.ready_timeout;
    opts.settle = Duration::from_millis(args.settle.max(20));
    opts.panel_idle = Duration::from_millis(args.panel_idle);
    opts.input_quiet = Duration::from_millis(args.input_quiet);
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

/// Resolves on SIGINT or SIGTERM.
pub async fn interrupted() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        if let (Ok(mut int), Ok(mut term)) = (signal(SignalKind::interrupt()), signal(SignalKind::terminate())) {
            tokio::select! {
                _ = int.recv() => {}
                _ = term.recv() => {}
            }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
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
    let setup_error = |printer: &Printer, dir: &Path, stage: &str, error: String, code: u8| {
        let _ = std::fs::write(dir.join("error.txt"), &error);
        let record = side_field(json!({"type": "error", "stage": stage, "error": error, "run_dir": dir, "code": code}));
        printer.record(&record);
        if let Some(path) = &args.ready_file {
            let _ = std::fs::write(path, json!({"ok": false, "error": record["error"], "code": code}).to_string());
        }
        Outcome { code, dir: dir.to_path_buf(), end: record }
    };
    let mut prepared = match launch::prepare(args, firmware, dir.clone()) {
        Ok(p) => p,
        Err(e) => return setup_error(&printer, &dir, "setup", e, EXIT_SETUP),
    };
    if let Err(e) = eink_core::script::validate(steps, &prepared.flash.machine, prepared.trace.is_ok()) {
        return setup_error(&printer, &dir, "usage", e, EXIT_USAGE);
    }
    let opts = match options(args, &prepared) {
        Ok(o) => o,
        Err(e) => return setup_error(&printer, &dir, "usage", e, EXIT_USAGE),
    };
    let events = match RunEvents::new(&dir, args.echo_log) {
        Ok(e) => Arc::new(e),
        Err(e) => return setup_error(&printer, &dir, "setup", e, EXIT_SETUP),
    };
    let session = match launch::launch(&mut prepared, events.clone()).await {
        Ok(s) => s,
        Err(e) => return setup_error(&printer, &dir, "qemu", e, EXIT_SETUP),
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
            return setup_error(&printer, &dir, "setup", e, EXIT_SETUP);
        }
    };
    let start = launch::start_record(&prepared, &session, None);
    let _ = std::fs::write(dir.join("run.json"), serde_json::to_string_pretty(&start).unwrap_or_default());
    runner.emit(&start);
    crate::control::remember_trace_starts(&mut runner.trace_names, steps);
    let mut was_interrupted = false;
    let ran = tokio::select! {
        ok = runner.run(steps) => Some(ok),
        _ = interrupted() => None,
    };
    let mut emulator_exited = false;
    match ran {
        None => {
            was_interrupted = true;
            runner.emit(&json!({"type": "interrupted", "steps": runner.index}));
        }
        Some(_) if args.hold => {
            let ended = crate::control::serve(&mut runner, args, tap.clone(), &printer).await;
            was_interrupted = ended == "interrupted";
            emulator_exited = ended == "qemu-exited";
        }
        Some(_) => {}
    }
    // Every step counts, the initial ones and any a held session ran.
    let ok = runner.failed.is_none() && !was_interrupted && !emulator_exited;
    let end = finish(&mut runner, &session, ok, started, was_interrupted).await;
    let code = if was_interrupted { EXIT_INTERRUPTED } else if ok { 0 } else { EXIT_FAIL };
    Outcome { code, dir, end }
}

/// Stop the session and write the run's summary files, its end record and
/// then its report (which shows the end record).
pub async fn finish(runner: &mut Runner, session: &Arc<eink_core::session::Session>, ok: bool, started: Instant, interrupted: bool) -> Value {
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
    let failed = runner.failed.clone();
    let end = json!({
        "type": "end",
        "ok": ok,
        "interrupted": interrupted,
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
        "report": dir.join("report.html"),
    });
    runner.emit(&end);
    let _ = crate::report::write(&dir);
    end
}

/// The setup-failure code a background run reported, for `start`.
pub fn child_code(ready: &Value) -> u8 {
    ready["code"].as_u64().map(|c| c as u8).unwrap_or(EXIT_SETUP)
}
