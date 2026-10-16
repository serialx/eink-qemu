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

pub async fn compare(base: PathBuf, candidate: PathBuf, tokens: Vec<String>, args: RunArgs, names: String, require_identical: bool, sequential: bool) -> u8 {
    let printer = Arc::new(Printer::new(args.quiet, args.progress));
    if args.hold || args.gui {
        return usage(&printer, "compare does not take --hold or --gui; use start/do for one firmware at a time");
    }
    if args.sd_in_place {
        return usage(&printer, "compare does not take --sd-in-place: both runs start from copies of --sd");
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
    let run_a = execute(&base, &args, dir_a.clone(), Some(a.to_string()), printer.clone(), &steps, &all);
    let run_b = execute(&candidate, &args, dir_b.clone(), Some(b.to_string()), printer.clone(), &steps, &all);
    let (out_a, out_b) = if sequential {
        let out_a = run_a.await;
        (out_a, run_b.await)
    } else {
        tokio::join!(run_a, run_b)
    };
    let summary = summarize(&dir, (a, &base, &dir_a), (b, &candidate, &dir_b));
    let _ = std::fs::write(dir.join("compare.json"), serde_json::to_string_pretty(&summary).unwrap_or_default());
    let md = markdown(&summary);
    let _ = std::fs::write(dir.join("compare.md"), &md);
    let report = crate::report::write(&dir).ok();
    printer.note(&md);
    let differing = summary["shots"].as_array().map(|s| s.iter().filter(|x| x["identical"] == false).count()).unwrap_or(0);
    let ok = out_a.code == 0 && out_b.code == 0 && (!require_identical || differing == 0);
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
    let ready = |r: &[Value]| steps_of(r, "ready").first().map(|x| json!({"how": x["how"], "home_fw_ms": x["home_fw_ms"], "boot_s": x["boot_s"]}));

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
    let mut value_rows = Vec::new();
    for (text, keys) in &va {
        for (key, list) in keys {
            let mut la = list.clone();
            let mut lb = vb.get(text).and_then(|k| k.get(key)).cloned().unwrap_or_default();
            let (sa, sb): (f64, f64) = (la.iter().sum(), lb.iter().sum());
            value_rows.push(json!({"step": text, "value": key, "n": [la.len(), lb.len()], "median": [median(&mut la), median(&mut lb)], "sum": [sa, sb]}));
        }
    }

    // Heaps by name.
    let heap_rows: Vec<Value> = steps_of(&ra, "heap")
        .iter()
        .map(|x| {
            let name = x["name"].clone();
            let other = steps_of(&rb, "heap").into_iter().find(|y| y["name"] == name).cloned().unwrap_or(Value::Null);
            let pick = |h: &Value| json!({
                "free": h["free"], "largest": h["largest"], "min_free": h["min_free"], "dram_free": h["dram_free"],
                "dram_largest": h["dram_largest"], "live_sites": h["live_sites"].as_array().map(|s| s.iter().take(5).cloned().collect::<Vec<_>>()),
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
            let pick = |t: &Value| json!({"allocs": t["allocs"], "alloc_bytes": t["alloc_bytes"], "kept": t["kept"], "kept_bytes": t["kept_bytes"]});
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
        json!({"metric": "ESP-IDF error lines in the log", "a": ma["log_errors"], "b": mb["log_errors"]}),
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

    // Marked windows: from each `mark` to the next (or the end), the guest
    // time the steps took and the emulator's counters, for any firmware.
    let windows = |r: &[Value]| {
        let mut out: BTreeMap<String, Value> = BTreeMap::new();
        let end = r.iter().rev().find(|x| x["type"] == "end");
        let marks: Vec<&Value> = r.iter().filter(|x| x["type"] == "step" && x["step"] == "mark").collect();
        let steps: Vec<&Value> = r.iter().filter(|x| x["type"] == "step").collect();
        let last = steps.last().copied();
        for (i, mark) in marks.iter().enumerate() {
            let next = marks.get(i + 1).copied().or(last);
            let Some(next) = next else { continue };
            let diff = |k: &str| match (mark[k].as_f64(), next[k].as_f64()) {
                (Some(x), Some(y)) if !std::ptr::eq(*mark, next) => json!(((y - x) * 10.0).round() / 10.0),
                _ => Value::Null,
            };
            let mut w = json!({
                "guest_ms": diff("guest_ms"),
                "screen_updates": diff("screen_updates"),
                "panel_refreshes": diff("panel_refreshes"),
                "panel_refresh_ms": diff("panel_refresh_ms"),
            });
            // The last window runs to the last step; its counters come from the end of the run.
            if marks.get(i + 1).is_none() && next["screen_updates"].is_null() {
                let totals = end.map(|e| &e["metrics"]);
                if let Some(t) = totals {
                    for k in ["screen_updates", "panel_refreshes", "panel_refresh_ms"] {
                        if let (Some(x), Some(y)) = (mark[k].as_f64(), t[k].as_f64()) {
                            w[k] = json!(((y - x) * 10.0).round() / 10.0);
                        }
                    }
                }
            }
            out.insert(mark["name"].as_str().unwrap_or_default().to_string(), w);
        }
        out
    };
    let (wa, wb) = (windows(&ra), windows(&rb));
    let window_rows: Vec<Value> = wa
        .iter()
        .map(|(name, a)| json!({"name": name, "a": a, "b": wb.get(name).cloned().unwrap_or(Value::Null)}))
        .collect();
    let start = |r: &[Value]| r.iter().find(|x| x["type"] == "start").cloned().unwrap_or(Value::Null);
    let (sa, sb) = (start(&ra), start(&rb));
    json!({
        "names": [a.0, b.0],
        "firmware": [a.1, b.1],
        "flash_sha256": [sa["flash"]["sha256"], sb["flash"]["sha256"]],
        "machine": [sa["machine"], sb["machine"]],
        "timing": [sa["timing"], sb["timing"]],
        "card": [sa["card"]["size_mib"], sb["card"]["size_mib"]],
        "runs": [rel(dir, a.2), rel(dir, b.2)],
        "outcome": [outcome(&ra, a.2), outcome(&rb, b.2)],
        "ready": [ready(&ra), ready(&rb)],
        "metrics": metric_rows,
        "windows": window_rows,
        "values": value_rows,
        "heaps": heap_rows,
        "traces": trace_rows,
        "shots": shot_rows,
    })
}

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

fn delta(a: &Value, b: &Value) -> String {
    match (a.as_f64(), b.as_f64()) {
        (Some(x), Some(y)) => {
            let d = y - x;
            let pct = if x != 0.0 { format!(" ({:+.1}%)", d / x * 100.0) } else { String::new() };
            if d.fract() == 0.0 { format!("{:+}{pct}", d as i64) } else { format!("{d:+.1}{pct}") }
        }
        _ => String::new(),
    }
}

/// The comparison as Markdown tables.
pub fn markdown(s: &Value) -> String {
    let (a, b) = (cell(&s["names"][0]), cell(&s["names"][1]));
    let mut out = String::new();
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
    out += &format!("| Result | {} | {} | |\n", result(&s["outcome"][0]), result(&s["outcome"][1]));
    let (ha, hb) = (&s["ready"][0]["home_fw_ms"], &s["ready"][1]["home_fw_ms"]);
    if !ha.is_null() || !hb.is_null() {
        out += &format!("| CrossPoint Home drawn (firmware ms) | {} | {} | {} |\n", cell(ha), cell(hb), delta(ha, hb));
    }
    let (ba, bb) = (&s["ready"][0]["boot_s"], &s["ready"][1]["boot_s"]);
    if !ba.is_null() || !bb.is_null() {
        out += &format!("| Boot to ready (s) | {} | {} | {} |\n", cell(ba), cell(bb), delta(ba, bb));
    }
    for w in s["windows"].as_array().into_iter().flatten() {
        let name = cell(&w["name"]);
        for (key, label) in [("guest_ms", "guest ms"), ("panel_refreshes", "panel refreshes"), ("screen_updates", "display updates")] {
            let (x, y) = (&w["a"][key], &w["b"][key]);
            if !x.is_null() || !y.is_null() {
                out += &format!("| `mark:{name}` window: {label} | {} | {} | {} |\n", cell(x), cell(y), delta(x, y));
            }
        }
    }
    // Marked windows (`mark:NAME` before the steps) first: they are what a
    // script chose to measure; values outside any mark come after them.
    let mut rows: Vec<Value> = s["values"].as_array().cloned().unwrap_or_default();
    rows.sort_by_key(|r| !r["step"].as_str().unwrap_or_default().contains(" / "));
    let mut value_lines = String::new();
    for row in &rows {
        let (x, y) = (&row["median"][0], &row["median"][1]);
        value_lines += &format!("| `{}` {} median (n={}/{}) | {} | {} | {} |\n", cell(&row["step"]), cell(&row["value"]), cell(&row["n"][0]), cell(&row["n"][1]), cell(x), cell(y), delta(x, y));
        let n = row["n"][0].as_u64().unwrap_or(0).max(row["n"][1].as_u64().unwrap_or(0));
        if n > 1 && row["value"].as_str().is_some_and(|v| v.ends_with("_ms")) {
            let (x, y) = (&row["sum"][0], &row["sum"][1]);
            value_lines += &format!("| `{}` {} sum | {} | {} | {} |\n", cell(&row["step"]), cell(&row["value"]), cell(x), cell(y), delta(x, y));
        }
    }
    out += &value_lines;
    for row in s["metrics"].as_array().into_iter().flatten() {
        if row["a"].is_null() && row["b"].is_null() || row["a"] == json!([]) && row["b"] == json!([]) {
            continue;
        }
        out += &format!("| {} | {} | {} | {} |\n", cell(&row["metric"]), cell(&row["a"]), cell(&row["b"]), delta(&row["a"], &row["b"]));
    }
    if s["timing"].as_array().is_some_and(|t| t.iter().any(|v| v == "turbo")) {
        out += "\n**Turbo timing: the time rows above are not valid for comparison; run without --turbo.**\n";
    }
    let heaps = s["heaps"].as_array().cloned().unwrap_or_default();
    if !heaps.is_empty() {
        out += &format!("\n| Heap snapshot | {a} free / largest / lowest | {b} free / largest / lowest | change in free |\n|---|---|---|---|\n");
        for h in &heaps {
            let f = |x: &Value| format!("{} / {} / {}", cell(&x["free"]), cell(&x["largest"]), cell(&x["min_free"]));
            out += &format!("| {} | {} | {} | {} |\n", cell(&h["name"]), f(&h["a"]), f(&h["b"]), delta(&h["a"]["free"], &h["b"]["free"]));
        }
        for h in &heaps {
            let (la, lb) = (h["a"]["live_sites"].as_array(), h["b"]["live_sites"].as_array());
            if la.is_none() && lb.is_none() {
                continue;
            }
            out += &format!("\n| Live memory at `{}` | {a} bytes (blocks) | {b} bytes (blocks) |\n|---|---|---|\n", cell(&h["name"]));
            let mut sites: Vec<String> = Vec::new();
            for list in [la, lb].into_iter().flatten() {
                for site in list {
                    let name = cell(&site["fn"]);
                    if !sites.contains(&name) {
                        sites.push(name);
                    }
                }
            }
            for name in sites {
                let find = |list: Option<&Vec<Value>>| list.and_then(|l| l.iter().find(|s| cell(&s["fn"]) == name)).map(|s| format!("{} ({})", cell(&s["bytes"]), cell(&s["blocks"]))).unwrap_or_else(|| "-".into());
                out += &format!("| `{}` | {} | {} |\n", name.replace('|', "\\|"), find(la), find(lb));
            }
        }
    }
    let traces = s["traces"].as_array().cloned().unwrap_or_default();
    if !traces.is_empty() {
        out += &format!("\n| Allocation window | {a} allocs / kept bytes | {b} allocs / kept bytes | change in kept |\n|---|---|---|---|\n");
        for t in traces {
            let f = |x: &Value| format!("{} / {}", cell(&x["allocs"]), cell(&x["kept_bytes"]));
            out += &format!("| {} | {} | {} | {} |\n", cell(&t["name"]), f(&t["a"]), f(&t["b"]), delta(&t["a"]["kept_bytes"], &t["b"]["kept_bytes"]));
        }
    }
    let shots = s["shots"].as_array().cloned().unwrap_or_default();
    if !shots.is_empty() {
        out += "\n| Screenshot | identical | differing pixels (rows) | diff |\n|---|---|---|---|\n";
        for sh in shots {
            let same = match sh["identical"].as_bool() { Some(true) => "yes", Some(false) => "**no**", None => "-" };
            let px = if sh["identical"] == false && sh["differing_pixels"].is_number() { format!("{} ({})", cell(&sh["differing_pixels"]), cell(&sh["differing_rows"])) } else { String::new() };
            out += &format!("| {} | {same} | {px} | {} |\n", cell(&sh["name"]), sh["diff"].as_str().map(|d| format!("`{d}`")).unwrap_or_default());
        }
    }
    out
}
