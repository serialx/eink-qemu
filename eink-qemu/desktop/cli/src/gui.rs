//! `eink-qemu run --gui`: the run in the desktop app's window. The run is
//! prepared here as for a headless run; the app starts it from a spec in
//! the run directory (EINK_QEMU_AUTOMATION), shows it, runs the steps with
//! the same runner and prints their records, which are relayed here. The
//! window stays open after the steps; closing it ends the run.

use crate::launch;
use crate::output::Printer;
use crate::run::{collect_steps, usage};
use crate::{RunArgs, EXIT_FAIL, EXIT_SETUP};
use eink_core::script::automation::Automation;
use eink_core::script::{metrics, CRASH_PATTERNS};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Instant;
use tokio::io::{AsyncBufReadExt, BufReader};

/// The desktop app: EINK_QEMU_DESKTOP, a build beside this executable
/// (`pnpm tauri build`), or the installed app.
fn find_desktop() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("EINK_QEMU_DESKTOP") {
        return Ok(PathBuf::from(path));
    }
    let name = if cfg!(windows) { "eink-qemu-desktop.exe" } else { "eink-qemu-desktop" };
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join(name));
            // A debug CLI beside a release app build in the same workspace.
            candidates.push(dir.join("..").join("release").join(name));
        }
    }
    if cfg!(target_os = "macos") {
        candidates.push(PathBuf::from("/Applications/eink-qemu.app/Contents/MacOS/eink-qemu-desktop"));
    }
    candidates.into_iter().find(|p| p.is_file()).ok_or_else(|| {
        "the desktop app was not found: build it with `pnpm install && pnpm tauri build --no-bundle` in eink-qemu/desktop, \
         or set EINK_QEMU_DESKTOP to its executable. (A plain `cargo build` of the app expects the Vite dev server.)"
            .to_string()
    })
}

pub async fn run(firmware: PathBuf, tokens: Vec<String>, args: RunArgs) -> u8 {
    let started = Instant::now();
    let printer = Arc::new(Printer::new(args.quiet, args.progress));
    if args.hold {
        return usage(&printer, "--gui keeps the window open after the steps already; drop --hold");
    }
    let (all_steps, steps) = match collect_steps(&args, &tokens) {
        Ok(s) => s,
        Err(e) => return usage(&printer, &e),
    };
    if let Err(e) = launch::check_args(&args) {
        return usage(&printer, &e);
    }
    let desktop = match find_desktop() {
        Ok(d) => d,
        Err(e) => return usage(&printer, &e),
    };
    let dir = match launch::run_dir(args.out.as_deref(), &firmware, "") {
        Ok(d) => d,
        Err(e) => return usage(&printer, &e),
    };
    let setup_error = |error: String| {
        printer.record(&json!({"type": "error", "stage": "setup", "error": error, "run_dir": dir}));
        EXIT_SETUP
    };
    let prepared = match launch::prepare(&args, &firmware, dir.clone()) {
        Ok(p) => p,
        Err(e) => return setup_error(e),
    };
    if let Err(e) = eink_core::script::validate(&steps, &prepared.flash.machine, prepared.trace.is_ok()) {
        return usage(&printer, &e);
    }
    let flash = dir.join("flash.bin");
    if let Err(e) = std::fs::write(&flash, &prepared.flash.image) {
        return setup_error(format!("cannot write {}: {e}", flash.display()));
    }
    let mut start = json!({
        "type": "start",
        "run_dir": dir,
        "firmware": prepared.firmware,
        "machine": prepared.flash.machine,
        "flash": {"kind": prepared.flash.kind, "sha256": eink_core::flash::sha256_hex(&prepared.flash.image), "app": prepared.flash.app},
        "elf": prepared.elf,
        "elf_note": prepared.elf_note,
        "card": prepared.card_info,
        "timing": if prepared.options.turbo { "turbo" } else { "accurate" },
        "ghosting": prepared.options.ghosting,
        "animate": prepared.options.animate,
        "guest_clock": prepared.guest_clock,
    });
    start["trace"] = json!(prepared.trace.as_ref().ok().map(|(p, _)| p.clone()));
    let mut crash_patterns: Vec<String> = if args.no_crash_check { Vec::new() } else { CRASH_PATTERNS.iter().map(|s| s.to_string()).collect() };
    crash_patterns.extend(args.crash_pattern.iter().cloned());
    let spec = Automation {
        dir: dir.clone(),
        firmware: prepared.firmware.clone(),
        machine: prepared.flash.machine.clone(),
        flash,
        card: prepared.card.clone(),
        elf: prepared.elf.clone(),
        options: prepared.options.clone(),
        trace: prepared.trace.clone().ok(),
        guest_clock: prepared.guest_clock,
        timeout: args.timeout,
        ready_timeout: args.ready_timeout,
        settle_ms: args.settle.max(20),
        panel_idle_ms: args.panel_idle,
        input_quiet_ms: args.input_quiet,
        crash_patterns,
        frames: !args.no_frames,
        steps: all_steps,
        start,
    };
    let spec_path = dir.join("automation.json");
    if let Err(e) = std::fs::write(&spec_path, serde_json::to_string_pretty(&spec).unwrap_or_default()) {
        return setup_error(format!("cannot write {}: {e}", spec_path.display()));
    }
    drop(prepared);
    let log = match std::fs::File::create(dir.join("desktop.log")) {
        Ok(f) => f,
        Err(e) => return setup_error(e.to_string()),
    };
    let mut child = match tokio::process::Command::new(&desktop)
        .env("EINK_QEMU_AUTOMATION", &spec_path)
        .env("EINK_QEMU_DATA_DIR", dir.join("desktop"))
        .env("EINK_QEMU_BIN", spec_qemu_bin(&args, &spec))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::from(log))
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return setup_error(format!("cannot start {}: {e}", desktop.display())),
    };
    printer.note(&format!("eink-qemu: running in the desktop window ({}); close it to finish", desktop.display()));
    let mut ok = false;
    let mut steps_done = false;
    let mut setup_failed = false;
    if let Some(stdout) = child.stdout.take() {
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let Ok(record) = serde_json::from_str::<Value>(&line) else { continue };
            if record["type"] == "steps-done" {
                ok = record["ok"] == true;
                steps_done = true;
                setup_failed = record["setup_failed"] == true;
                printer.note(&format!("eink-qemu: steps {}; the window stays open, close it to finish", if ok { "passed" } else { "failed" }));
            }
            // steps-done goes out too: a script can close the window on it.
            printer.record(&record);
        }
    }
    // The app crashing after the steps (while the window stayed open) fails the run too.
    let exit = child.wait().await.ok();
    let app_failed = exit.as_ref().is_some_and(|s| !s.success());
    if app_failed && steps_done {
        printer.record(&json!({"type": "error", "stage": "gui", "error": format!("the desktop app exited with {:?} after the steps", exit.and_then(|s| s.code()))}));
        ok = false;
    }
    if !steps_done {
        let log = std::fs::read_to_string(dir.join("desktop.log")).unwrap_or_default();
        let tail: Vec<&str> = log.lines().rev().take(15).collect::<Vec<_>>().into_iter().rev().collect();
        printer.record(&json!({"type": "error", "stage": "gui", "error": "the desktop app exited before the steps finished", "log_tail": tail, "run_dir": dir}));
    }
    finish(&dir, ok && steps_done, started, &printer);
    if ok && steps_done { 0 } else if steps_done && !setup_failed { EXIT_FAIL } else { EXIT_SETUP }
}

/// The QEMU directory the CLI resolved, for the app.
fn spec_qemu_bin(args: &RunArgs, spec: &Automation) -> PathBuf {
    launch::find_qemu(args.qemu.as_deref(), &spec.machine).map(|q| q.bin).unwrap_or_default()
}

/// The summary files and end record, from what the app wrote.
fn finish(dir: &Path, ok: bool, started: Instant, printer: &Printer) {
    let log = std::fs::read_to_string(dir.join("log.txt")).unwrap_or_default();
    let summary = metrics::summarize(log.lines());
    let _ = std::fs::write(dir.join("metrics.json"), serde_json::to_string_pretty(&summary).unwrap_or_default());
    let records: Vec<Value> = std::fs::read_to_string(dir.join("run.jsonl")).unwrap_or_default().lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
    let steps = records.iter().filter(|r| r["type"] == "step").count();
    let failed = records.iter().find(|r| r["type"] == "step" && r["ok"] == false);
    let frames = std::fs::read_to_string(dir.join("frames.json")).ok().and_then(|t| serde_json::from_str::<Vec<Value>>(&t).ok()).map(|f| f.len()).unwrap_or(0);
    let end = json!({
        "type": "end",
        "ok": ok,
        "steps": steps,
        "failed_step": failed.map(|f| f["i"].clone()),
        "error": failed.map(|f| f["error"].clone()),
        "duration_s": (started.elapsed().as_secs_f64() * 10.0).round() / 10.0,
        "frames": frames,
        "metrics": {
            "page_renders": summary["page_renders"],
            "bw_render_ms_median": summary["bw_render_ms"]["median"],
            "total_render_ms_median": summary["total_render_ms"]["median"],
            "mem_min_free": summary["mem_min_free"],
        },
        "run_dir": dir,
        "gui": true,
    });
    let mut end = end;
    end["report"] = json!(dir.join("report.html"));
    if let Ok(mut file) = std::fs::OpenOptions::new().append(true).create(true).open(dir.join("run.jsonl")) {
        use std::io::Write;
        let _ = writeln!(file, "{end}");
    }
    let _ = crate::report::write(dir);
    printer.record(&end);
}
