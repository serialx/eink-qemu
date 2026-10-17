//! `eink-qemu compare`: the same steps on two firmwares, at the same time
//! by default (guest-time measurements do not depend on the host's load),
//! then a comparison of what they recorded: readiness, the log's render
//! and memory measurements, heaps, allocation windows, per-step values and
//! screenshots. Writes compare.json and compare.md (a table for a PR) in the
//! comparison directory, beside each run's own directory.

use crate::launch;
use crate::output::Printer;
use crate::run::{collect_steps, execute, usage};
use crate::{RunArgs, EXIT_FAIL, EXIT_INTERRUPTED, EXIT_SETUP, EXIT_USAGE};
use eink_core::script::image;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Time changes below this are noise: under `ms`, or under `pct` percent of
/// the base value.
#[derive(Clone, Copy)]
pub struct Noise {
    pub ms: f64,
    pub pct: f64,
}

impl Default for Noise {
    fn default() -> Self {
        Noise { ms: 10.0, pct: 1.0 }
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn compare(base: PathBuf, candidate: PathBuf, tokens: Vec<String>, args: RunArgs, names: String, require_identical: bool, sequential: bool, noise: Noise) -> u8 {
    let printer = Arc::new(Printer::new(args.quiet, args.progress));
    if args.hold || args.gui {
        return usage(&printer, "compare does not take --hold or --gui; use start/do for one firmware at a time");
    }
    if args.sd_in_place {
        return usage(&printer, "compare does not take --sd-in-place: both runs start from copies of --sd");
    }
    if args.gdb.is_some() && !sequential {
        return usage(&printer, "both runs cannot share one --gdb port: add --sequential to run them one after the other");
    }
    let parts: Vec<&str> = names.split(',').map(str::trim).collect();
    let [a, b] = parts[..] else { return usage(&printer, "--names takes two names: A,B") };
    // Each name is one folder inside the comparison directory.
    let valid = |n: &str| !n.is_empty() && !n.starts_with('.') && n.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c));
    if !valid(a) || !valid(b) || a.eq_ignore_ascii_case(b) {
        return usage(&printer, "--names takes two different names of letters, digits, '-', '_' or '.', not starting with '.'");
    }
    let (all, steps) = match collect_steps(&args, &tokens) {
        Ok(s) => s,
        Err(e) => return usage(&printer, &e),
    };
    if let Err(e) = crate::launch::check_args(&args) {
        return usage(&printer, &e);
    }
    // Both boards must take the steps before either run starts.
    for firmware in [&base, &candidate] {
        if let Ok(flash) = eink_core::flash::make_flash_with(firmware, args.machine.as_deref(), args.partitions.as_deref()) {
            if let Err(e) = eink_core::script::validate(&steps, &flash.machine, args.trace) {
                return usage(&printer, &format!("{}: {e}", firmware.display()));
            }
        }
    }
    // A comparison neither side of which could start may be run again into the same --out.
    if let Some(out) = args.out.as_deref() {
        retry_failed_compare(out, [a, b]);
    }
    let dir = match launch::run_dir(args.out.as_deref(), Path::new("compare"), "") {
        Ok(d) => d,
        Err(e) => return usage(&printer, &e),
    };
    let (dir_a, dir_b) = (dir.join(a), dir.join(b));
    for d in [&dir_a, &dir_b] {
        if let Err(e) = std::fs::create_dir_all(d) {
            return usage(&printer, &format!("cannot create {}: {e}", d.display()));
        }
    }
    // Each side on its own task: one side's blocking work (symbolizing a
    // heap walk's allocation log) never holds up the other's steps.
    let side = |firmware: &Path, dir: &Path, name: &str| {
        let (firmware, dir, name) = (firmware.to_path_buf(), dir.to_path_buf(), name.to_string());
        let (args, printer, steps, all) = (args.clone(), printer.clone(), steps.clone(), all.clone());
        async move { tokio::spawn(async move { execute(&firmware, &args, dir, Some(name), printer, &steps, &all).await }).await.expect("a comparison run panicked") }
    };
    let (out_a, out_b) = if sequential {
        let out_a = side(&base, &dir_a, a).await;
        if out_a.code == EXIT_INTERRUPTED {
            // Interrupted during the first: the second does not start.
            let why = "not run: the comparison was interrupted during the other side";
            let _ = std::fs::write(dir_b.join("error.txt"), why);
            (out_a, crate::run::Outcome { code: EXIT_INTERRUPTED, dir: dir_b.clone(), end: json!({"type": "error", "error": why}) })
        } else {
            (out_a, side(&candidate, &dir_b, b).await)
        }
    } else {
        tokio::join!(side(&base, &dir_a, a), side(&candidate, &dir_b, b))
    };
    let mut summary = summarize(&dir, (a, &base, &dir_a), (b, &candidate, &dir_b));
    summary["noise"] = json!({"ms": noise.ms, "pct": noise.pct});
    let md = markdown(&summary);
    // The comparison is the command's result: losing it fails the command.
    let mut written = true;
    for (name, text) in [("compare.json", serde_json::to_string_pretty(&summary).unwrap_or_default()), ("compare.md", md.clone())] {
        if let Err(e) = std::fs::write(dir.join(name), text) {
            written = false;
            printer.record(&json!({"type": "error", "stage": "compare", "error": format!("cannot write {}: {e}", dir.join(name).display())}));
        }
    }
    let report = crate::report::write(&dir).ok();
    printer.note(&md);
    let differing = summary["shots"].as_array().map(|s| s.iter().filter(|x| x["identical"] == false).count()).unwrap_or(0);
    let ok = out_a.code == 0 && out_b.code == 0 && (!require_identical || differing == 0) && written;
    let codes = [out_a.code, out_b.code];
    let code = if codes.contains(&EXIT_INTERRUPTED) {
        EXIT_INTERRUPTED
    } else if codes.contains(&EXIT_USAGE) {
        EXIT_USAGE
    } else if codes.contains(&EXIT_SETUP) {
        EXIT_SETUP
    } else if ok {
        0
    } else {
        EXIT_FAIL
    };
    printer.record(&json!({
        "type": "compare",
        "ok": ok,
        "dir": dir,
        "names": [a, b],
        "results": [out_a.code == 0, out_b.code == 0],
        "codes": codes,
        "differing_shots": differing,
        "summary": dir.join("compare.json"),
        "markdown": dir.join("compare.md"),
        "report": report,
    }));
    code
}

/// Clear `out` when it holds a comparison whose sides both failed to
/// start (the sandbox refused QEMU, say), so the same command runs again.
fn retry_failed_compare(out: &Path, sides: [&str; 2]) {
    let Ok(entries) = std::fs::read_dir(out) else { return };
    let names: Vec<String> = entries.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    let ours = |n: &str| sides.contains(&n) || [launch::CLAIM_FILE, "compare.json", "compare.md", "report.html", "diffs"].contains(&n);
    let sides_failed = sides.iter().all(|side| !out.join(side).exists() || launch::failed_setup(&out.join(side)));
    if !names.is_empty() && names.iter().all(|n| ours(n)) && sides.iter().any(|side| out.join(side).exists()) && sides_failed {
        launch::clear_dir(out);
    }
}

fn records(dir: &Path) -> Vec<Value> {
    std::fs::read_to_string(dir.join("run.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

fn median(v: &mut [f64]) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = v.len();
    Some(if n % 2 == 1 { v[n / 2] } else { (v[n / 2 - 1] + v[n / 2]) / 2.0 })
}

fn steps_of<'a>(records: &'a [Value], kind: &str) -> Vec<&'a Value> {
    records.iter().filter(|r| r["type"] == "step" && r["step"] == kind).collect()
}

/// Join the two runs' records and summaries.
fn summarize(dir: &Path, a: (&str, &Path, &Path), b: (&str, &Path, &Path)) -> Value {
    let (ra, rb) = (records(a.2), records(b.2));
    let read_json = |d: &Path, f: &str| std::fs::read_to_string(d.join(f)).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).unwrap_or(Value::Null);
    let (ma, mb) = (read_json(a.2, "metrics.json"), read_json(b.2, "metrics.json"));
    let outcome = |r: &[Value], d: &Path| {
        let end = r.iter().rev().find(|x| x["type"] == "end").cloned().unwrap_or(Value::Null);
        if end.is_null() {
            // It never started: the reason is in error.txt.
            let error = std::fs::read_to_string(d.join("error.txt")).unwrap_or_else(|_| "the run did not finish".into());
            return json!({"ok": false, "steps": 0, "error": error, "failed_step": null, "started": false});
        }
        json!({"ok": end["ok"], "steps": end["steps"], "error": end["error"], "failed_step": end["failed_step"]})
    };
    let pick_ready = |x: &Value| json!({"how": x["how"], "screen": x["screen"], "home_fw_ms": x["home_fw_ms"], "page_fw_ms": x["page_fw_ms"], "boot_s": x["boot_s"]});
    let ready = |r: &[Value]| steps_of(r, "ready").first().map(|x| pick_ready(x));
    // Every `ready` after the first: later boots (a sleep and wake, a reset).
    let later = |r: &[Value]| steps_of(r, "ready").iter().skip(1).map(|x| pick_ready(x)).collect::<Vec<_>>();

    // Values of wait-log steps (name=123ms in the matched line), by the
    // mark before them and the step text: medians and sums.
    let values = |r: &[Value]| {
        let mut by: BTreeMap<String, BTreeMap<String, Vec<f64>>> = BTreeMap::new();
        let mut window = String::new();
        for x in r.iter().filter(|x| x["type"] == "step") {
            if x["step"] == "mark" {
                window = x["name"].as_str().unwrap_or_default().to_string();
                continue;
            }
            if x["step"] != "wait-log" {
                continue;
            }
            if let Some(v) = x["values"].as_object() {
                let text = x["text"].as_str().unwrap_or_default();
                let key = if window.is_empty() { text.to_string() } else { format!("{window} / {text}") };
                let entry = by.entry(key).or_default();
                for (k, n) in v {
                    if let Some(n) = n.as_f64() {
                        entry.entry(k.clone()).or_default().push(n);
                    }
                }
            }
        }
        by
    };
    let (va, vb) = (values(&ra), values(&rb));
    // Every step and value either side logged: one only the candidate logs
    // (new or renamed) is a row too, empty on the side without it.
    let mut pairs: Vec<(&String, &String)> = va.iter().chain(&vb).flat_map(|(text, keys)| keys.keys().map(move |key| (text, key))).collect();
    pairs.sort();
    pairs.dedup();
    let list = |side: &BTreeMap<String, BTreeMap<String, Vec<f64>>>, text: &str, key: &str| side.get(text).and_then(|k| k.get(key)).cloned().unwrap_or_default();
    let sum = |l: &[f64]| (!l.is_empty()).then(|| l.iter().sum::<f64>());
    let mut value_rows = Vec::new();
    for (text, key) in pairs {
        let (mut la, mut lb) = (list(&va, text, key), list(&vb, text, key));
        let (sa, sb) = (sum(&la), sum(&lb));
        value_rows.push(json!({"step": text, "value": key, "n": [la.len(), lb.len()], "median": [median(&mut la), median(&mut lb)], "sum": [sa, sb]}));
    }

    // Heaps by name.
    let heap_rows: Vec<Value> = steps_of(&ra, "heap")
        .iter()
        .map(|x| {
            let name = x["name"].clone();
            let other = steps_of(&rb, "heap").into_iter().find(|y| y["name"] == name).cloned().unwrap_or(Value::Null);
            let pick = |h: &Value| json!({
                "free": h["free"], "largest": h["largest"], "min_free": h["min_free"], "dram_free": h["dram_free"],
                "dram_largest": h["dram_largest"], "live_sites": h["live_sites"].as_array().map(|s| s.iter().take(10).cloned().collect::<Vec<_>>()),
            });
            json!({"name": name, "a": pick(x), "b": pick(&other)})
        })
        .collect();

    // Allocation windows by name.
    let trace_rows: Vec<Value> = steps_of(&ra, "trace-stop")
        .iter()
        .map(|x| {
            let name = x["name"].clone();
            let other = steps_of(&rb, "trace-stop").into_iter().find(|y| y["name"] == name).cloned().unwrap_or(Value::Null);
            let pick = |t: &Value| {
                let mut w = json!({});
                for k in TRACE_FIELDS {
                    w[k] = t[k].clone();
                }
                w
            };
            json!({"name": name, "a": pick(x), "b": pick(&other)})
        })
        .collect();

    // Shots by name, compared pixel for pixel.
    let diffs = dir.join("diffs");
    let mut shot_rows = Vec::new();
    for x in steps_of(&ra, "shot") {
        let name = x["name"].as_str().unwrap_or_default().to_string();
        let Some(y) = steps_of(&rb, "shot").into_iter().find(|y| y["name"] == name.as_str()) else {
            shot_rows.push(json!({"name": name, "identical": null, "note": format!("only in {}", a.0)}));
            continue;
        };
        let (fa, fb) = (a.2.join(x["file"].as_str().unwrap_or_default()), b.2.join(y["file"].as_str().unwrap_or_default()));
        if x["pixels_sha256"] == y["pixels_sha256"] {
            shot_rows.push(json!({"name": name, "identical": true, "a": rel(dir, &fa), "b": rel(dir, &fb)}));
            continue;
        }
        let _ = std::fs::create_dir_all(&diffs);
        let out = diffs.join(format!("{name}.png"));
        let row = match (image::read_png(&fa), image::read_png(&fb)) {
            (Ok(ia), Ok(ib)) => match image::diff(&ia, &ib, &[], Some(&out)) {
                Ok(d) => json!({"name": name, "identical": d.equal, "differing_pixels": d.differing_pixels, "differing_rows": d.differing_rows, "bbox": d.bbox, "size_matches": d.size_matches, "diff": out.exists().then(|| rel(dir, &out)), "a": rel(dir, &fa), "b": rel(dir, &fb)}),
                Err(e) => json!({"name": name, "identical": false, "note": e}),
            },
            (Err(e), _) | (_, Err(e)) => json!({"name": name, "identical": false, "note": e}),
        };
        shot_rows.push(row);
    }

    let m = |v: &Value, path: &[&str]| path.iter().fold(v.clone(), |acc, k| acc[*k].clone());
    // The emulator's counters for any firmware, then CrossPoint's own measurements.
    let mut metric_rows = vec![
        json!({"metric": "display updates sent, whole run", "a": ma["screen_updates"], "b": mb["screen_updates"]}),
        json!({"metric": "panel refreshes, whole run", "a": ma["panel_refreshes"], "b": mb["panel_refreshes"]}),
        json!({"metric": "panel refresh time, whole run (guest ms)", "a": ma["panel_refresh_ms"], "b": mb["panel_refresh_ms"]}),
        json!({"metric": "error lines in the log (ESP-IDF E, CrossPoint [ERR])", "a": ma["log_errors"], "b": mb["log_errors"], "always": true}),
        json!({"metric": "warning lines in the log (ESP-IDF W)", "a": ma["log_warnings"], "b": mb["log_warnings"]}),
    ];
    let (ca, cb) = (&ma["crosspoint"], &mb["crosspoint"]);
    if ca.is_object() || cb.is_object() {
        metric_rows.extend([
            json!({"metric": "CrossPoint page renders in the whole run", "a": ca["page_renders"], "b": cb["page_renders"]}),
            json!({"metric": "CrossPoint page drawing (bw_render) median ms, whole run", "a": m(ca, &["bw_render_ms", "median"]), "b": m(cb, &["bw_render_ms", "median"])}),
            json!({"metric": "CrossPoint total rendering median ms, whole run", "a": m(ca, &["total_render_ms", "median"]), "b": m(cb, &["total_render_ms", "median"])}),
            json!({"metric": "CrossPoint cold layout ms", "a": ca["cold_layout_ms"], "b": cb["cold_layout_ms"]}),
            json!({"metric": "lowest Min Free since boot (bytes, CrossPoint's [MEM])", "a": ca["mem_min_free"], "b": cb["mem_min_free"]}),
            json!({"metric": "last periodic [MEM] Free (bytes; every 10 s, not at a step)", "a": m(ca, &["mem_last", "free"]), "b": m(cb, &["mem_last", "free"])}),
            json!({"metric": "last periodic [MEM] MaxAlloc (bytes)", "a": m(ca, &["mem_last", "max_alloc"]), "b": m(cb, &["mem_last", "max_alloc"])}),
        ]);
    }

    // Marked windows, in script order: from each `mark` to the next, the
    // last one to the last step, with the guest time the steps took and the
    // emulator's counters, for any firmware.
    let windows = |r: &[Value]| {
        let steps: Vec<&Value> = r.iter().filter(|x| x["type"] == "step").collect();
        let totals = r.iter().rev().find(|x| x["type"] == "end").map(|e| e["metrics"].clone()).unwrap_or(Value::Null);
        let marks: Vec<usize> = steps.iter().enumerate().filter(|(_, x)| x["step"] == "mark").map(|(i, _)| i).collect();
        let mut out: Vec<(String, Value)> = Vec::new();
        for (n, &i) in marks.iter().enumerate() {
            let mark = steps[i];
            let next_mark = marks.get(n + 1).map(|&j| steps[j]);
            let last = steps[steps.len() - 1];
            let diff = |x: Option<f64>, y: Option<f64>| match (x, y) {
                (Some(x), Some(y)) => json!(((y - x) * 10.0).round() / 10.0),
                _ => Value::Null,
            };
            // Counters at the window's end: the next mark's, else the run's totals.
            let end_of = |k: &str| next_mark.map_or_else(|| totals[k].as_f64(), |m| m[k].as_f64());
            let mut w = json!({"to_end": next_mark.is_none(), "guest_ms": diff(mark["guest_ms"].as_f64(), next_mark.unwrap_or(last)["guest_ms"].as_f64())});
            for k in ["screen_updates", "panel_refreshes", "panel_refresh_ms"] {
                w[k] = diff(mark[k].as_f64(), end_of(k));
            }
            out.push((mark["name"].as_str().unwrap_or_default().to_string(), w));
        }
        out
    };
    let (wa, wb) = (windows(&ra), windows(&rb));
    let window_rows: Vec<Value> = wa
        .iter()
        .map(|(name, a)| json!({"name": name, "a": a, "b": wb.iter().find(|(n, _)| n == name).map(|(_, w)| w.clone()).unwrap_or(Value::Null)}))
        .collect();
    let start = |r: &[Value]| r.iter().find(|x| x["type"] == "start").cloned().unwrap_or(Value::Null);
    let (sa, sb) = (start(&ra), start(&rb));
    json!({
        "names": [a.0, b.0],
        "firmware": [a.1, b.1],
        "flash_sha256": [sa["flash"]["sha256"], sb["flash"]["sha256"]],
        "machine": [sa["machine"], sb["machine"]],
        "timing": [sa["timing"], sb["timing"]],
        "pacing": [sa["pacing"], sb["pacing"]],
        "card": [sa["card"]["size_mib"], sb["card"]["size_mib"]],
        "runs": [rel(dir, a.2), rel(dir, b.2)],
        "outcome": [outcome(&ra, a.2), outcome(&rb, b.2)],
        "ready": [ready(&ra), ready(&rb)],
        "later_ready": [later(&ra), later(&rb)],
        "metrics": metric_rows,
        "windows": window_rows,
        "values": value_rows,
        "heaps": heap_rows,
        "traces": trace_rows,
        "shots": shot_rows,
    })
}

/// What a trace-stop record counts about its window, all compared.
const TRACE_FIELDS: [&str; 7] = ["allocs", "alloc_bytes", "freed", "kept", "kept_bytes", "older_freed", "older_freed_bytes"];

fn rel(base: &Path, path: &Path) -> String {
    path.strip_prefix(base).map(|p| p.display().to_string()).unwrap_or_else(|_| path.display().to_string())
}

fn cell(v: &Value) -> String {
    match v {
        Value::Null => "-".into(),
        Value::Number(n) => {
            let f = n.as_f64().unwrap_or(0.0);
            if f.fract() == 0.0 { format!("{}", f as i64) } else { format!("{f:.1}") }
        }
        Value::String(s) => s.clone(),
        Value::Array(a) => a.iter().map(cell).collect::<Vec<_>>().join(", "),
        other => other.to_string(),
    }
}

/// What a row measures, for its change: a time in ms changes by less than
/// the threshold when under its ms or its percentage of the base (a host
/// time, which depends on the host's load, ten times the ms and five times
/// the percentage; a sum of N times, N times the ms); a count or size
/// changes at any difference.
#[derive(Clone, Copy, PartialEq)]
enum Measure {
    Time,
    HostTime,
    Sum(u64),
    Count,
}

/// A row's change and whether it is one: present on one side only, a
/// count that differs, or a time beyond the noise floor (a time within it
/// is shown without a percentage, marked as noise).
fn change(a: &Value, b: &Value, measure: Measure, noise: Noise) -> (String, bool) {
    match (a, b) {
        // Lists of the same length (CrossPoint's cold layouts): element by element.
        (Value::Array(xs), Value::Array(ys)) if xs.len() == ys.len() && xs.iter().chain(ys).all(Value::is_number) => {
            let changes: Vec<(String, bool)> = xs.iter().zip(ys).map(|(x, y)| change(x, y, measure, noise)).collect();
            let changed = changes.iter().any(|c| c.1);
            if !changed {
                let moved = changes.iter().any(|c| !c.0.is_empty());
                return (if moved { "(below threshold)".into() } else { String::new() }, false);
            }
            (changes.iter().map(|c| if c.0.is_empty() { "0".to_string() } else { c.0.clone() }).collect::<Vec<_>>().join(", "), true)
        }
        _ => match (a.as_f64(), b.as_f64()) {
            (Some(x), Some(y)) => {
                let d = y - x;
                let number = if d.fract() == 0.0 { format!("{:+}", d as i64) } else { format!("{d:+.1}") };
                let floor = match measure {
                    Measure::Time => Some(noise.ms.max(noise.pct / 100.0 * x.abs())),
                    Measure::HostTime => Some((noise.ms * 10.0).max(noise.pct * 5.0 / 100.0 * x.abs())),
                    Measure::Sum(n) => Some((noise.ms * n as f64).max(noise.pct / 100.0 * x.abs())),
                    Measure::Count => None,
                };
                if floor.is_some_and(|floor| d.abs() < floor) {
                    return (if d == 0.0 { String::new() } else { format!("{number} (below threshold)") }, false);
                }
                let pct = if x != 0.0 { format!(" ({:+.1}%)", d / x * 100.0) } else { String::new() };
                (if d == 0.0 { String::new() } else { format!("{number}{pct}") }, d != 0.0)
            }
            _ => (String::new(), a != b),
        },
    }
}

/// The comparison's rows in Markdown tables. `compact` keeps the result,
/// readiness and each marked window's guest time, and otherwise only rows
/// that changed (beyond the threshold for times).
fn tables(s: &Value, compact: bool) -> String {
    let (a, b) = (cell(&s["names"][0]), cell(&s["names"][1]));
    let noise = s["noise"].as_object().map_or_else(Noise::default, |n| Noise {
        ms: n.get("ms").and_then(Value::as_f64).unwrap_or(10.0),
        pct: n.get("pct").and_then(Value::as_f64).unwrap_or(1.0),
    });
    let mut out = String::new();
    let row = |out: &mut String, label: &str, x: &Value, y: &Value, measure: Measure, always: bool| {
        let (delta, changed) = change(x, y, measure, noise);
        if compact && !always && !changed {
            return;
        }
        *out += &format!("| {label} | {} | {} | {delta} |\n", cell(x), cell(y));
    };
    let result = |o: &Value| {
        if o["ok"] == true {
            format!("PASS ({} steps)", cell(&o["steps"]))
        } else {
            format!("FAIL at step {}: {}", cell(&o["failed_step"]), cell(&o["error"]))
        }
    };
    out += &format!("| | {a} | {b} | change |\n|---|---|---|---|\n");
    out += &format!("| Firmware | `{}` | `{}` | |\n", cell(&s["firmware"][0]), cell(&s["firmware"][1]));
    out += &format!("| Board, timing, card MiB | {} {} {} | {} {} {} | |\n", cell(&s["machine"][0]), cell(&s["timing"][0]), cell(&s["card"][0]), cell(&s["machine"][1]), cell(&s["timing"][1]), cell(&s["card"][1]));
    let pacing = |p: &Value| if p.is_object() { format!("{} / {} ms", cell(&p["input_quiet_ms"]), cell(&p["key_hold_ms"])) } else { "-".into() };
    if s["pacing"].as_array().is_some_and(|p| p.iter().any(Value::is_object)) {
        out += &format!("| Input pacing: quiet needed before each input / default key hold (guest ms) | {} | {} | |\n", pacing(&s["pacing"][0]), pacing(&s["pacing"][1]));
    }
    out += &format!("| Result | {} | {} | |\n", result(&s["outcome"][0]), result(&s["outcome"][1]));
    let (ra, rb) = (&s["ready"][0], &s["ready"][1]);
    for (key, label) in [("home_fw_ms", "First `ready`: CrossPoint Home drawn (firmware ms)"), ("page_fw_ms", "First `ready`: CrossPoint booted into a book, first page drawn (firmware ms)")] {
        if !ra[key].is_null() || !rb[key].is_null() {
            row(&mut out, label, &ra[key], &rb[key], Measure::Time, true);
        }
    }
    let ms = |v: &Value| v.as_f64().map_or(Value::Null, |s| json!((s * 1000.0).round()));
    if !ra["boot_s"].is_null() || !rb["boot_s"].is_null() {
        let label = "First `ready`: ROM banner to Home, a book's first page, or the last screen update (host ms)";
        row(&mut out, label, &ms(&ra["boot_s"]), &ms(&rb["boot_s"]), Measure::HostTime, true);
    }
    // Later boots (sleep and wake, reset), by how they finished: medians.
    let later = |side: usize, how: &str, key: &str| -> Vec<f64> {
        s["later_ready"][side].as_array().into_iter().flatten().filter(|r| r["how"] == how).filter_map(|r| r[key].as_f64()).collect()
    };
    // Firmware times where the boot has them; host times for every kind
    // (a quiet screen has only those).
    for (how, key, what) in [
        ("reader", Some("page_fw_ms"), "booted into a book, first page drawn"),
        ("home", Some("home_fw_ms"), "CrossPoint Home drawn"),
        ("quiet", None, "CrossPoint ready once its screen went quiet"),
        ("screen-quiet", None, "ready on a quiet screen"),
    ] {
        let (mut ha, mut hb) = (later(0, how, "boot_s"), later(1, how, "boot_s"));
        if ha.is_empty() && hb.is_empty() {
            continue;
        }
        let n = format!("n={}/{}", ha.len(), hb.len());
        if let Some(key) = key {
            let (mut xa, mut xb) = (later(0, how, key), later(1, how, key));
            row(&mut out, &format!("Later `ready` passes ({n}): {what}, median (firmware ms)"), &json!(median(&mut xa)), &json!(median(&mut xb)), Measure::Time, true);
        }
        let (ha, hb) = (median(&mut ha).map(|v| json!(v)).unwrap_or(Value::Null), median(&mut hb).map(|v| json!(v)).unwrap_or(Value::Null));
        row(&mut out, &format!("Later `ready` passes ({n}): {what}, median from the ROM banner (host ms)"), &ms(&ha), &ms(&hb), Measure::HostTime, key.is_none());
    }
    for w in s["windows"].as_array().into_iter().flatten() {
        let name = cell(&w["name"]);
        let name = if w["a"]["to_end"] == true { format!("{name}` (to the end)") } else { format!("{name}`") };
        for (key, label, measure, always) in [
            ("guest_ms", "guest ms", Measure::Time, true),
            ("panel_refreshes", "panel refreshes", Measure::Count, false),
            ("screen_updates", "display updates", Measure::Count, false),
        ] {
            let (x, y) = (&w["a"][key], &w["b"][key]);
            if !x.is_null() || !y.is_null() {
                row(&mut out, &format!("`mark:{name} window: {label}"), x, y, measure, always);
            }
        }
    }
    // Marked windows (`mark:NAME` before the steps) first: they are what a
    // script chose to measure; values outside any mark come after them.
    let mut rows: Vec<Value> = s["values"].as_array().cloned().unwrap_or_default();
    rows.sort_by_key(|r| !r["step"].as_str().unwrap_or_default().contains(" / "));
    for r in &rows {
        let time = r["value"].as_str().is_some_and(|v| v.ends_with("_ms"));
        let measure = if time { Measure::Time } else { Measure::Count };
        let label = format!("`{}` {} median (n={}/{})", cell(&r["step"]), cell(&r["value"]), cell(&r["n"][0]), cell(&r["n"][1]));
        // Medians of different numbers of samples always show.
        row(&mut out, &label, &r["median"][0], &r["median"][1], measure, r["n"][0] != r["n"][1]);
        let n = r["n"][0].as_u64().unwrap_or(0).max(r["n"][1].as_u64().unwrap_or(0));
        if n > 1 && time {
            row(&mut out, &format!("`{}` {} sum", cell(&r["step"]), cell(&r["value"])), &r["sum"][0], &r["sum"][1], Measure::Sum(n), false);
        }
    }
    for m in s["metrics"].as_array().into_iter().flatten() {
        if m["a"].is_null() && m["b"].is_null() || m["a"] == json!([]) && m["b"] == json!([]) {
            continue;
        }
        let label = cell(&m["metric"]);
        let measure = if label.contains("ms") { Measure::Time } else { Measure::Count };
        row(&mut out, &label, &m["a"], &m["b"], measure, m["always"] == true);
    }
    if s["timing"].as_array().is_some_and(|t| t.iter().any(|v| v == "turbo")) {
        out += "\n**Turbo timing: the time rows above are not valid for comparison; run without --turbo.**\n";
    }
    let heaps = s["heaps"].as_array().cloned().unwrap_or_default();
    let same_heap = |h: &Value| ["free", "largest", "min_free"].iter().all(|k| h["a"][*k] == h["b"][*k]);
    let shown: Vec<&Value> = heaps.iter().filter(|h| !compact || !same_heap(h)).collect();
    if !shown.is_empty() {
        out += &format!("\n| Heap snapshot | {a} free / largest / lowest | {b} free / largest / lowest | change in free |\n|---|---|---|---|\n");
        for h in &shown {
            let f = |x: &Value| format!("{} / {} / {}", cell(&x["free"]), cell(&x["largest"]), cell(&x["min_free"]));
            out += &format!("| {} | {} | {} | {} |\n", cell(&h["name"]), f(&h["a"]), f(&h["b"]), change(&h["a"]["free"], &h["b"]["free"], Measure::Count, noise).0);
        }
    }
    // Live sites: each side's five largest, looked up in both sides' ten
    // largest (a site ranked sixth on one side still has its bytes); the
    // compact table keeps the sites that differ.
    let mut unranked = false;
    for h in &heaps {
        let (la, lb) = (h["a"]["live_sites"].as_array(), h["b"]["live_sites"].as_array());
        if la.is_none() && lb.is_none() {
            continue;
        }
        let mut sites: Vec<String> = Vec::new();
        for list in [la, lb].into_iter().flatten() {
            for site in list.iter().take(5) {
                let name = cell(&site["fn"]);
                if !sites.contains(&name) {
                    sites.push(name);
                }
            }
        }
        let find = |list: Option<&Vec<Value>>, name: &str| list.and_then(|l| l.iter().find(|s| cell(&s["fn"]) == name)).map(|s| format!("{} ({})", cell(&s["bytes"]), cell(&s["blocks"])));
        let lines: Vec<String> = sites
            .iter()
            .filter_map(|name| {
                let (x, y) = (find(la, name), find(lb, name));
                if compact && x == y {
                    return None;
                }
                unranked |= x.is_none() || y.is_none();
                Some(format!("| `{}` | {} | {} |\n", name.replace('|', "\\|"), x.unwrap_or_else(|| "-".into()), y.unwrap_or_else(|| "-".into())))
            })
            .collect();
        if !lines.is_empty() {
            out += &format!("\n| Live memory at `{}` | {a} bytes (blocks) | {b} bytes (blocks) |\n|---|---|---|\n{}", cell(&h["name"]), lines.concat());
        }
    }
    if unranked {
        out += "\n`-`: not among that run's ten largest live allocation sites.\n";
    }
    let traces = s["traces"].as_array().cloned().unwrap_or_default();
    let same_trace = |t: &Value| TRACE_FIELDS.iter().all(|k| t["a"][*k] == t["b"][*k]);
    let shown: Vec<&Value> = traces.iter().filter(|t| !compact || !same_trace(t)).collect();
    if !shown.is_empty() {
        out += &format!("\n| Allocation window | {a} | {b} | change in kept bytes |\n|---|---|---|---|\n");
        for t in &shown {
            let f = |x: &Value| {
                let older = if x["older_freed"].as_u64().unwrap_or(0) > 0 { format!("; freed {} older ({} B)", cell(&x["older_freed"]), cell(&x["older_freed_bytes"])) } else { String::new() };
                format!("{} allocs, {} B; kept {} ({} B), freed {}{older}", cell(&x["allocs"]), cell(&x["alloc_bytes"]), cell(&x["kept"]), cell(&x["kept_bytes"]), cell(&x["freed"]))
            };
            out += &format!("| {} | {} | {} | {} |\n", cell(&t["name"]), f(&t["a"]), f(&t["b"]), change(&t["a"]["kept_bytes"], &t["b"]["kept_bytes"], Measure::Count, noise).0);
        }
    }

    let shots = s["shots"].as_array().cloned().unwrap_or_default();
    let shown: Vec<&Value> = shots.iter().filter(|sh| !compact || sh["identical"] != true).collect();
    if !shown.is_empty() {
        out += "\n| Screenshot | identical | differing pixels (rows) | diff |\n|---|---|---|---|\n";
        for sh in &shown {
            let same = match sh["identical"].as_bool() { Some(true) => "yes", Some(false) => "**no**", None => "-" };
            let px = if sh["identical"] == false && sh["differing_pixels"].is_number() { format!("{} ({})", cell(&sh["differing_pixels"]), cell(&sh["differing_rows"])) } else { String::new() };
            out += &format!("| {} | {same} | {px} | {} |\n", cell(&sh["name"]), sh["diff"].as_str().map(|d| format!("`{d}`")).unwrap_or_default());
        }
    }
    if compact && !shots.is_empty() && shown.len() < shots.len() {
        let same = shots.len() - shown.len();
        out += &format!("\n{same} of {} screenshots identical.\n", shots.len());
    }
    out
}

/// Table rows in Markdown (not headers or separators).
fn data_rows(md: &str) -> usize {
    let lines = md.lines().filter(|l| l.starts_with('|'));
    let (rows, separators) = lines.fold((0, 0), |(r, s), l| if l.starts_with("|---") { (r, s + 1) } else { (r + 1, s) });
    rows - separators
}

/// The compact rows for a pull request (the result, readiness, marked
/// windows, and what changed beyond the threshold) and a line on what
/// they leave out.
pub fn compact_tables(s: &Value) -> String {
    let mut out = tables(s, true);
    let shots = s["shots"].as_array().map_or(0, |sh| sh.iter().filter(|x| x["identical"] == true).count());
    let hidden = data_rows(&tables(s, false)).saturating_sub(data_rows(&out) + shots);
    if hidden > 0 {
        let noise = s["noise"].as_object().map_or_else(Noise::default, |n| Noise {
            ms: n.get("ms").and_then(Value::as_f64).unwrap_or(10.0),
            pct: n.get("pct").and_then(Value::as_f64).unwrap_or(1.0),
        });
        let fmt = |v: f64| if v.fract() == 0.0 { format!("{v:.0}") } else { format!("{v}") };
        out += &format!(
            "\n{hidden} more rows are unchanged, or changed by less than the threshold (times: {} ms or {}% of the base; host times: {} ms or {}%; a sum of N times: N × {} ms or {}%): see all rows below.\n",
            fmt(noise.ms), fmt(noise.pct), fmt(noise.ms * 10.0), fmt(noise.pct * 5.0), fmt(noise.ms), fmt(noise.pct)
        );
    }
    out
}

/// The comparison as Markdown for a pull request: the compact rows, then
/// every row in a collapsed table.
pub fn markdown(s: &Value) -> String {
    format!("{}\n<details><summary>All rows</summary>\n\n{}\n</details>\n", compact_tables(s), tables(s, false))
}

/// Every row, for the HTML report.
pub fn full_tables(s: &Value) -> String {
    tables(s, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_dir(name: &str, records: &[Value]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("eink-qemu-compare-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("run.jsonl"), records.iter().map(|r| format!("{r}\n")).collect::<String>()).unwrap();
        dir
    }

    #[test]
    fn values_only_the_candidate_logs_are_compared_too() {
        let step = |values: Value| json!({"type": "step", "step": "wait-log", "text": "wait-log:Page render", "ok": true, "values": values});
        let a = run_dir("a", &[step(json!({"render_ms": 10}))]);
        let b = run_dir("b", &[step(json!({"render_ms": 12, "total_ms": 20}))]);
        let summary = summarize(&std::env::temp_dir(), ("a", Path::new("a.bin"), &a), ("b", Path::new("b.bin"), &b));
        let _ = (std::fs::remove_dir_all(&a), std::fs::remove_dir_all(&b));
        let rows = summary["values"].as_array().unwrap();
        let row = |key: &str| rows.iter().find(|r| r["value"] == key).cloned().unwrap_or_default();
        assert_eq!(row("render_ms")["median"], json!([10.0, 12.0]));
        assert_eq!(row("total_ms")["n"], json!([0, 1]));
        assert_eq!(row("total_ms")["median"], json!([null, 20.0]));
        assert_eq!(row("total_ms")["sum"], json!([null, 20.0]));
        assert!(markdown(&summary).contains("`wait-log:Page render` total_ms median (n=0/1) | - | 20 |"));
    }

    #[test]
    fn changes_below_the_threshold_stay_out_of_the_compact_table() {
        let value = |v: &str, a: f64, b: f64| json!({"step": "turns / wait-log:Page render", "value": v, "n": [12, 12], "median": [a, b], "sum": [a * 12.0, b * 12.0]});
        let shot = |name: &str, identical: bool| json!({"name": name, "identical": identical, "differing_pixels": 3, "differing_rows": 1});
        let summary = json!({
            "names": ["base", "pr"], "noise": {"ms": 10.0, "pct": 1.0},
            "outcome": [{"ok": true, "steps": 30}, {"ok": true, "steps": 30}],
            "ready": [{"how": "reader", "page_fw_ms": 6512, "boot_s": 7.1}, {"how": "reader", "page_fw_ms": 6512, "boot_s": 7.2}],
            "windows": [{"name": "turns", "a": {"guest_ms": 17916.0, "panel_refreshes": 36, "screen_updates": 48},
                         "b": {"guest_ms": 17920.0, "panel_refreshes": 36, "screen_updates": 48}}],
            "values": [value("gray_display_ms", 149.0, 143.0), value("total_ms", 1448.0, 1493.0), value("prewarm_ms", 18.0, 18.0)],
            "metrics": [{"metric": "error lines in the log (ESP-IDF E, CrossPoint [ERR])", "a": 43, "b": 43, "always": true},
                        {"metric": "panel refreshes, whole run", "a": 80, "b": 80}],
            "heaps": [{"name": "opened", "a": {"free": 1, "largest": 2, "min_free": 3}, "b": {"free": 1, "largest": 2, "min_free": 3}}],
            "shots": [shot("page1", true), shot("page2", true), shot("page3", false)],
        });
        let md = markdown(&summary);
        let (compact, full) = md.split_once("<details>").unwrap();
        // Shown: readiness, the window's guest time (+4 ms is below the threshold), changes beyond it, the error count.
        assert!(compact.contains("first page drawn (firmware ms) | 6512 | 6512 |  |"), "{compact}");
        assert!(compact.contains("window: guest ms | 17916 | 17920 | +4 (below threshold) |"), "{compact}");
        assert!(compact.contains("total_ms median (n=12/12) | 1448 | 1493 | +45 (+3.1%) |"), "{compact}");
        assert!(compact.contains("total_ms sum | 17376 | 17916 | +540 (+3.1%) |"), "{compact}");
        assert!(compact.contains("CrossPoint [ERR]) | 43 | 43 |"), "{compact}");
        // Host time: 100 ms of a 7 s boot is within its wider threshold.
        assert!(compact.contains("last screen update (host ms) | 7100 | 7200 | +100 (below threshold) |"), "{compact}");
        // Left out: changes below the threshold (twelve 6 ms differences are not a 72 ms one), equal values, heaps and shots.
        for gone in ["gray_display_ms", "prewarm_ms", "panel refreshes", "Heap snapshot", "| page1 |"] {
            assert!(!compact.contains(gone), "{gone} in {compact}");
        }
        assert!(compact.contains("| page3 | **no** | 3 (1) |"), "{compact}");
        assert!(compact.contains("2 of 3 screenshots identical."), "{compact}");
        assert!(compact.contains("8 more rows are unchanged, or changed by less than the threshold (times: 10 ms or 1% of the base; host times: 100 ms or 5%; a sum of N times: N × 10 ms or 1%)"), "{compact}");
        // The full table has every row, changes below the threshold marked without a percentage.
        assert!(full.contains("gray_display_ms median (n=12/12) | 149 | 143 | -6 (below threshold) |"), "{full}");
        assert!(full.contains("gray_display_ms sum | 1788 | 1716 | -72 (below threshold) |"), "{full}");
        assert!(full.contains("| opened | 1 / 2 / 3 | 1 / 2 / 3 |  |"), "{full}");
        assert_eq!(data_rows(full), 20);
    }

    #[test]
    fn sample_counts_live_sites_and_later_boots() {
        let site = |f: &str, bytes: u64| json!({"fn": f, "bytes": bytes, "blocks": 1});
        // Six sites; the fifth and sixth swap places between the runs.
        let sites = |fifth: &str, sixth: &str| json!([site("A", 900), site("B", 800), site("C", 700), site("D", 600), site(fifth, 4096), site(sixth, 4096)]);
        let ready = |how: &str, ms: u64| json!({"how": how, "page_fw_ms": ms, "boot_s": 5.0});
        let summary = json!({
            "names": ["base", "pr"],
            "outcome": [{"ok": true, "steps": 9}, {"ok": true, "steps": 9}],
            "ready": [{"how": "home", "home_fw_ms": 3421, "boot_s": 3.9}, {"how": "home", "home_fw_ms": 3421, "boot_s": 3.9}],
            "later_ready": [[ready("reader", 4007), ready("reader", 4008), ready("reader", 4005)], [ready("reader", 3979), ready("reader", 3985)]],
            "values": [{"step": "wait-log:Page render", "value": "total_ms", "n": [12, 2], "median": [100.0, 100.0], "sum": [1200.0, 200.0]}],
            "heaps": [{"name": "after", "a": {"free": 5, "largest": 1, "min_free": 1, "live_sites": sites("CssParser", "XML_GetBuffer")},
                       "b": {"free": 5, "largest": 1, "min_free": 1, "live_sites": sites("XML_GetBuffer", "CssParser")}},
                      {"name": "changed", "a": {"free": 5, "largest": 1, "min_free": 1, "live_sites": [site("A", 900), site("B", 800)]},
                       "b": {"free": 5, "largest": 1, "min_free": 1, "live_sites": [site("A", 900), site("B", 1800)]}}],
        });
        let md = markdown(&summary);
        let (compact, full) = md.split_once("<details>").unwrap();
        // Medians of 12 and of 2 samples are not the same measurement, equal or not.
        assert!(compact.contains("total_ms median (n=12/2) | 100 | 100 |  |"), "{compact}");
        // Later boots: medians of each side's own.
        assert!(compact.contains("Later `ready` passes (n=3/2): booted into a book, first page drawn, median (firmware ms) | 4007 | 3982 | -25 (below threshold) |"), "{compact}");
        // A site ranked fifth on one side and sixth on the other has its bytes on both.
        assert!(full.contains("| `CssParser` | 4096 (1) | 4096 (1) |"), "{full}");
        assert!(full.contains("| `XML_GetBuffer` | 4096 (1) | 4096 (1) |"), "{full}");
        assert!(!full.contains("not among"), "{full}");
        // The compact table keeps the sites that changed, also under equal heap totals.
        assert!(compact.contains("| Live memory at `changed` |") && compact.contains("| `B` | 800 (1) | 1800 (1) |"), "{compact}");
        assert!(!compact.contains("Live memory at `after`"), "{compact}");
    }

    #[test]
    fn allocation_window_counts_and_later_quiet_boots_show() {
        let window = |kept: u64, freed: u64, older: u64| json!({"allocs": 3, "alloc_bytes": 100, "kept": kept, "kept_bytes": 50, "freed": freed, "older_freed": older, "older_freed_bytes": older * 4096});
        let quiet = |boot_s: f64| json!({"how": "quiet", "boot_s": boot_s});
        let summary = json!({
            "names": ["base", "pr"],
            "outcome": [{"ok": true, "steps": 9}, {"ok": true, "steps": 9}],
            "later_ready": [[quiet(3.0)], [quiet(8.0)]],
            "traces": [{"name": "counts", "a": window(1, 2, 0), "b": window(2, 1, 0)}, {"name": "older", "a": window(1, 2, 0), "b": window(1, 2, 1)}],
        });
        let md = markdown(&summary);
        let (compact, _) = md.split_once("<details>").unwrap();
        assert!(compact.contains("| counts | 3 allocs, 100 B; kept 1 (50 B), freed 2 | 3 allocs, 100 B; kept 2 (50 B), freed 1 |"), "{compact}");
        assert!(compact.contains("| older | 3 allocs, 100 B; kept 1 (50 B), freed 2 | 3 allocs, 100 B; kept 1 (50 B), freed 2; freed 1 older (4096 B) |"), "{compact}");
        assert!(compact.contains("Later `ready` passes (n=1/1): CrossPoint ready once its screen went quiet, median from the ROM banner (host ms) | 3000 | 8000 |"), "{compact}");
    }

    #[test]
    fn a_comparison_that_never_started_can_run_again() {
        let dir = std::env::temp_dir().join(format!("eink-qemu-compare-retry-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for side in ["base", "pr"] {
            std::fs::create_dir_all(dir.join(side)).unwrap();
            std::fs::write(dir.join(side).join("error.txt"), "QEMU did not start").unwrap();
            std::fs::write(dir.join(side).join("run.jsonl"), "{\"type\":\"error\"}\n").unwrap();
        }
        std::fs::write(dir.join("compare.md"), "|").unwrap();
        // The claim every run directory gets, the comparison's own included.
        std::fs::write(dir.join(launch::CLAIM_FILE), "token").unwrap();
        retry_failed_compare(&dir, ["base", "pr"]);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        // One side that ran: the directory stays as it is.
        std::fs::create_dir_all(dir.join("base")).unwrap();
        std::fs::write(dir.join("base").join("run.jsonl"), "{\"type\":\"start\"}\n").unwrap();
        std::fs::write(dir.join("base").join("error.txt"), "").unwrap();
        retry_failed_compare(&dir, ["base", "pr"]);
        assert!(dir.join("base").join("run.jsonl").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn lists_change_element_by_element() {
        let noise = Noise::default();
        assert_eq!(change(&json!([3380, 1209]), &json!([3379, 1209]), Measure::Time, noise), ("(below threshold)".into(), false));
        assert_eq!(change(&json!([3380, 1209]), &json!([3380, 1309]), Measure::Time, noise), ("0, +100 (+8.3%)".into(), true));
        assert!(change(&json!([3380]), &json!([3380, 1]), Measure::Time, noise).1);
        assert_eq!(change(&json!(5), &json!(5), Measure::Count, noise), (String::new(), false));
    }
}
