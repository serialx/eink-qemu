//! `eink-qemu compare`: the same steps on two firmwares, at the same time
//! by default (guest-time measurements do not depend on the host's load),
//! then a comparison of what they recorded: readiness, the log's render
//! and memory measurements, heaps, allocation windows, per-step values and
//! screenshots. Writes compare.json and compare.md (a table for a PR) in the
//! comparison directory, beside each run's own directory.

use crate::launch;
use crate::output::Printer;
use crate::run::{collect_steps, execute, usage};
use crate::{RunArgs, EXIT_FAIL, EXIT_SETUP};
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
    let valid = |n: &str| !n.is_empty() && n.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c));
    if !valid(a) || !valid(b) || a == b {
        return usage(&printer, "--names takes two different names of letters, digits, '-', '_' or '.'");
    }
    let steps = match collect_steps(&args, &tokens) {
        Ok(s) => s,
        Err(e) => return usage(&printer, &e),
    };
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
    let run_a = execute(&base, &args, dir_a.clone(), Some(a.to_string()), printer.clone(), &steps);
    let run_b = execute(&candidate, &args, dir_b.clone(), Some(b.to_string()), printer.clone(), &steps);
    let (out_a, out_b) = if sequential {
        let out_a = run_a.await;
        (out_a, run_b.await)
    } else {
        tokio::join!(run_a, run_b)
    };
    if out_a.code == EXIT_SETUP || out_b.code == EXIT_SETUP {
        printer.record(&json!({"type": "compare", "ok": false, "error": "a run could not start; see its error record", "dir": dir}));
        return EXIT_SETUP;
    }
    let summary = summarize(&dir, (a, &base, &dir_a), (b, &candidate, &dir_b));
    let _ = std::fs::write(dir.join("compare.json"), serde_json::to_string_pretty(&summary).unwrap_or_default());
    let md = markdown(&summary);
    let _ = std::fs::write(dir.join("compare.md"), &md);
    let report = crate::report::write(&dir).ok();
    printer.note(&md);
    let differing = summary["shots"].as_array().map(|s| s.iter().filter(|x| x["identical"] == false).count()).unwrap_or(0);
    let ok = out_a.code == 0 && out_b.code == 0 && (!require_identical || differing == 0);
    printer.record(&json!({
        "type": "compare",
        "ok": ok,
        "dir": dir,
        "names": [a, b],
        "results": [out_a.code == 0, out_b.code == 0],
        "differing_shots": differing,
        "summary": dir.join("compare.json"),
        "markdown": dir.join("compare.md"),
        "report": report,
    }));
    if ok { 0 } else { EXIT_FAIL }
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
    let outcome = |r: &[Value]| {
        let end = r.iter().rev().find(|x| x["type"] == "end").cloned().unwrap_or(Value::Null);
        json!({"ok": end["ok"], "steps": end["steps"], "error": end["error"], "failed_step": end["failed_step"]})
    };
    let ready = |r: &[Value]| steps_of(r, "ready").first().map(|x| json!({"how": x["how"], "home_fw_ms": x["home_fw_ms"], "boot_s": x["boot_s"]}));

    // Values of wait-log steps (name=123ms in the matched line), by step text: medians.
    let values = |r: &[Value]| {
        let mut by: BTreeMap<String, BTreeMap<String, Vec<f64>>> = BTreeMap::new();
        for x in steps_of(r, "wait-log") {
            if let Some(v) = x["values"].as_object() {
                let entry = by.entry(x["text"].as_str().unwrap_or_default().to_string()).or_default();
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
            value_rows.push(json!({"step": text, "value": key, "n": [la.len(), lb.len()], "median": [median(&mut la), median(&mut lb)]}));
        }
    }

    // Heaps by name.
    let heap_rows: Vec<Value> = steps_of(&ra, "heap")
        .iter()
        .map(|x| {
            let name = x["name"].clone();
            let other = steps_of(&rb, "heap").into_iter().find(|y| y["name"] == name).cloned().unwrap_or(Value::Null);
            let pick = |h: &Value| json!({"free": h["free"], "largest": h["largest"], "min_free": h["min_free"], "internal_free": h["internal_free"], "internal_largest": h["internal_largest"]});
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
        if x["sha256"] == y["sha256"] {
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
    let metric_rows = vec![
        json!({"metric": "page renders", "a": ma["page_renders"], "b": mb["page_renders"]}),
        json!({"metric": "page drawing (bw_render) median ms", "a": m(&ma, &["bw_render_ms", "median"]), "b": m(&mb, &["bw_render_ms", "median"])}),
        json!({"metric": "total rendering median ms", "a": m(&ma, &["total_render_ms", "median"]), "b": m(&mb, &["total_render_ms", "median"])}),
        json!({"metric": "cold layout ms", "a": ma["cold_layout_ms"], "b": mb["cold_layout_ms"]}),
        json!({"metric": "lowest Min Free (bytes)", "a": ma["mem_min_free"], "b": mb["mem_min_free"]}),
        json!({"metric": "last [MEM] Free (bytes)", "a": m(&ma, &["mem_last", "free"]), "b": m(&mb, &["mem_last", "free"])}),
        json!({"metric": "last [MEM] MaxAlloc (bytes)", "a": m(&ma, &["mem_last", "max_alloc"]), "b": m(&mb, &["mem_last", "max_alloc"])}),
    ];
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
        "outcome": [outcome(&ra), outcome(&rb)],
        "ready": [ready(&ra), ready(&rb)],
        "metrics": metric_rows,
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
        out += &format!("| Home drawn (firmware ms) | {} | {} | {} |\n", cell(ha), cell(hb), delta(ha, hb));
    }
    for row in s["metrics"].as_array().into_iter().flatten() {
        if row["a"].is_null() && row["b"].is_null() || row["a"] == json!([]) && row["b"] == json!([]) {
            continue;
        }
        out += &format!("| {} | {} | {} | {} |\n", cell(&row["metric"]), cell(&row["a"]), cell(&row["b"]), delta(&row["a"], &row["b"]));
    }
    for row in s["values"].as_array().into_iter().flatten() {
        let (x, y) = (&row["median"][0], &row["median"][1]);
        out += &format!("| `{}` {} median (n={}/{}) | {} | {} | {} |\n", cell(&row["step"]), cell(&row["value"]), cell(&row["n"][0]), cell(&row["n"][1]), cell(x), cell(y), delta(x, y));
    }
    let heaps = s["heaps"].as_array().cloned().unwrap_or_default();
    if !heaps.is_empty() {
        out += &format!("\n| Heap snapshot | {a} free / largest / lowest | {b} free / largest / lowest | change in free |\n|---|---|---|---|\n");
        for h in heaps {
            let f = |x: &Value| format!("{} / {} / {}", cell(&x["free"]), cell(&x["largest"]), cell(&x["min_free"]));
            out += &format!("| {} | {} | {} | {} |\n", cell(&h["name"]), f(&h["a"]), f(&h["b"]), delta(&h["a"]["free"], &h["b"]["free"]));
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
