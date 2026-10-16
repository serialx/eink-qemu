//! report.html for a run directory (steps, the failure, the filmstrip of
//! settled screens, heaps, log measurements, the log) or a comparison
//! directory (the comparison table, screenshot pairs with their diffs, and
//! links to both runs' reports). Images are linked by relative path, so the
//! report works opened from disk next to its files.

use serde_json::Value;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

const STYLE: &str = r#"
:root{--bg:#f6f5f1;--fg:#26251f;--muted:#6b685c;--line:#d9d6cc;--card:#fff;--ok:#2f6b3a;--bad:#9a3b2e;--code:#edebe4;color-scheme:light}
@media (prefers-color-scheme:dark){:root{--bg:#1c1c19;--fg:#e8e6dd;--muted:#a5a294;--line:#3a3a32;--card:#24241f;--ok:#7fbb86;--bad:#e08a7a;--code:#2d2d27;color-scheme:dark}}
*{box-sizing:border-box}body{margin:0;padding:24px max(16px,4vw);background:var(--bg);color:var(--fg);font:14px/1.5 system-ui,-apple-system,"Segoe UI",sans-serif}
h1{font-size:22px;margin:0 0 4px}h2{font-size:16px;margin:28px 0 8px}.muted{color:var(--muted)}code,pre{font-family:ui-monospace,Menlo,Consolas,monospace;font-size:12.5px}
code{background:var(--code);padding:1px 4px;border-radius:3px}pre{background:var(--code);padding:12px;border-radius:6px;overflow:auto;max-height:480px}
table{border-collapse:collapse;width:100%;background:var(--card);border:1px solid var(--line)}th,td{text-align:left;padding:6px 8px;border-top:1px solid var(--line);vertical-align:top}
th{background:var(--code);font-weight:600;font-size:12.5px}.wrap{overflow-x:auto}.ok{color:var(--ok);font-weight:600}.bad{color:var(--bad);font-weight:600}
.strip{display:flex;gap:10px;overflow-x:auto;padding-bottom:8px}.strip figure{margin:0;flex:0 0 auto;text-align:center}.strip img{height:220px;border:1px solid var(--line);background:#fff;image-rendering:pixelated}
.strip figcaption{font-size:12px;color:var(--muted)}.fail{border:1px solid var(--bad);border-left:4px solid var(--bad);background:var(--card);padding:10px 14px;border-radius:4px}
.pair{display:flex;gap:12px;flex-wrap:wrap}.pair figure{margin:0}.pair img{height:300px;border:1px solid var(--line);background:#fff}
td img{height:120px;border:1px solid var(--line);background:#fff}
"#;

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn text(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn read_records(dir: &Path) -> Vec<Value> {
    std::fs::read_to_string(dir.join("run.jsonl")).unwrap_or_default().lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
}

/// Write report.html in `dir` (a run or a comparison) and return its path.
pub fn write(dir: &Path) -> Result<PathBuf, String> {
    let html = if dir.join("compare.json").is_file() {
        compare_page(dir)?
    } else if dir.join("run.jsonl").is_file() {
        run_page(dir)?
    } else {
        return Err(format!("{} is neither a run directory (run.jsonl) nor a comparison (compare.json)", dir.display()));
    };
    let path = dir.join("report.html");
    std::fs::write(&path, html).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    Ok(path)
}

fn page(title: &str, body: &str) -> String {
    format!("<!doctype html><html><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>{}</title><style>{STYLE}</style></head><body>{body}</body></html>", esc(title))
}

fn step_detail(r: &Value) -> String {
    let kind = r["step"].as_str().unwrap_or_default();
    match kind {
        "ready" => format!("{} · Home at firmware {} ms · restarts {}", text(&r["how"]), text(&r["home_fw_ms"]), text(&r["restarts"])),
        "wait-log" => format!("<code>{}</code>", esc(&text(&r["line"]))),
        "shot" => format!("<a href=\"{0}\"><img src=\"{0}\" alt=\"{1}\"></a>", esc(&text(&r["file"])), esc(&text(&r["name"]))),
        "heap" => format!(
            "free {} · largest {} · lowest {} · <a href=\"{}\">{}</a>",
            text(&r["free"]), text(&r["largest"]), text(&r["min_free"]), esc(&text(&r["file"])), esc(&text(&r["file"]))
        ),
        "trace-stop" => format!("allocs {} · kept {} bytes in {} blocks · <a href=\"{}\">sites</a>", text(&r["allocs"]), text(&r["kept_bytes"]), text(&r["kept"]), esc(&text(&r["file"]))),
        "wait-screen" => r["frame"].as_str().map(|f| format!("<a href=\"{0}\"><img src=\"{0}\" alt=\"frame\"></a>", esc(f))).unwrap_or_default(),
        "press" | "touch" | "swipe" => String::new(),
        "hmp" => format!("<pre>{}</pre>", esc(&text(&r["output"]))),
        _ => String::new(),
    }
}

fn run_page(dir: &Path) -> Result<String, String> {
    let records = read_records(dir);
    let start = records.iter().find(|r| r["type"] == "start").cloned().unwrap_or(Value::Null);
    let end = records.iter().rev().find(|r| r["type"] == "end").cloned();
    let failed = records.iter().find(|r| r["type"] == "step" && r["ok"] == false).cloned();
    let mut b = String::new();
    let firmware = text(&start["firmware"]);
    let _ = write!(b, "<h1>eink-qemu run</h1><div class=\"muted\"><code>{}</code> on {} · {} timing · {}</div>", esc(&firmware), esc(&text(&start["machine"])), esc(&text(&start["timing"])), esc(&dir.display().to_string()));
    if let Some(error) = records.iter().find(|r| r["type"] == "error") {
        if end.is_none() && failed.is_none() {
            let _ = write!(b, "<p class=\"bad\">Could not start</p><div class=\"fail\">{}</div>", esc(&text(&error["error"])));
            return Ok(page("eink-qemu run (did not start)", &b));
        }
    }
    let verdict = match (&end, &failed) {
        (Some(e), _) if e["ok"] == true => format!("<p class=\"ok\">PASS · {} steps in {} s</p>", text(&e["steps"]), text(&e["duration_s"])),
        (Some(e), _) => format!("<p class=\"bad\">FAIL · {} steps in {} s</p>", text(&e["steps"]), text(&e["duration_s"])),
        (None, Some(_)) => "<p class=\"bad\">FAIL (run still going or interrupted)</p>".into(),
        (None, None) => "<p class=\"muted\">No end record: the run is going, or was interrupted.</p>".into(),
    };
    b += &verdict;
    if let Some(f) = &failed {
        let _ = write!(b, "<div class=\"fail\"><b>Step {} <code>{}</code> failed:</b> {}", text(&f["i"]), esc(&text(&f["text"])), esc(&text(&f["error"])));
        if let Some(lines) = f["last_lines"].as_array() {
            let _ = write!(b, "<pre>{}</pre>", esc(&lines.iter().map(text).collect::<Vec<_>>().join("\n")));
        }
        if dir.join("fail/last.png").is_file() {
            b += "<p>The screen at the failure, the heaps, the last 200 log lines and the registers are in <code>fail/</code>.</p><a href=\"fail/last.png\"><img style=\"height:260px;border:1px solid var(--line);background:#fff\" src=\"fail/last.png\" alt=\"screen at the failure\"></a>";
        }
        b += "</div>";
    }
    b += "<h2>Steps</h2><div class=\"wrap\"><table><tr><th>#</th><th>t (s)</th><th>guest ms</th><th>step</th><th></th><th>detail</th></tr>";
    for r in records.iter().filter(|r| r["type"] == "step") {
        let mark = if r["ok"] == true { "<span class=\"ok\">ok</span>" } else { "<span class=\"bad\">FAIL</span>" };
        let mut detail = step_detail(r);
        if let Some(e) = r["error"].as_str() {
            detail = format!("<span class=\"bad\">{}</span> {detail}", esc(e));
        }
        let iter = r["iter"].as_array().map(|i| format!(" <span class=\"muted\">#{}</span>", i.iter().map(text).collect::<Vec<_>>().join("."))).unwrap_or_default();
        let _ = write!(b, "<tr><td>{}</td><td>{}</td><td>{}</td><td><code>{}</code>{iter}</td><td>{mark}</td><td>{detail}</td></tr>", text(&r["i"]), text(&r["t"]), text(&r["guest_ms"]), esc(&text(&r["text"])));
    }
    b += "</table></div>";
    let frames: Vec<Value> = std::fs::read_to_string(dir.join("frames.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    if !frames.is_empty() {
        b += "<h2>Screens</h2><p class=\"muted\">Every settled screen, by guest time.</p><div class=\"strip\">";
        for f in &frames {
            let label = f["guest_ms"].as_i64().map(|m| format!("{:.2} s", m as f64 / 1000.0)).unwrap_or_else(|| format!("host {} s", text(&f["t"])));
            let _ = write!(b, "<figure><a href=\"{0}\"><img src=\"{0}\" alt=\"frame {1}\" loading=\"lazy\"></a><figcaption>{1} · {label}</figcaption></figure>", esc(&text(&f["file"])), text(&f["index"]));
        }
        b += "</div>";
    }
    let metrics: Value = std::fs::read_to_string(dir.join("metrics.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(Value::Null);
    if metrics["page_renders"].as_u64().unwrap_or(0) > 0 || metrics["mem_samples"].as_u64().unwrap_or(0) > 0 {
        b += "<h2>From the log</h2><table>";
        let row = |b: &mut String, k: &str, v: String| {
            let _ = write!(b, "<tr><th>{k}</th><td>{}</td></tr>", esc(&v));
        };
        row(&mut b, "Page renders", text(&metrics["page_renders"]));
        row(&mut b, "Page drawing (bw_render) median ms", text(&metrics["bw_render_ms"]["median"]));
        row(&mut b, "Total rendering median ms", text(&metrics["total_render_ms"]["median"]));
        row(&mut b, "Cold layout ms", text(&metrics["cold_layout_ms"]));
        row(&mut b, "Lowest Min Free (bytes)", text(&metrics["mem_min_free"]));
        row(&mut b, "Last memory report", text(&metrics["mem_last"]));
        b += "</table>";
    }
    let log = std::fs::read_to_string(dir.join("log.txt")).unwrap_or_default();
    let lines: Vec<&str> = log.lines().collect();
    let shown = &lines[lines.len().saturating_sub(3000)..];
    let _ = write!(
        b,
        "<h2>Log</h2><details><summary>{} lines{} (<a href=\"log.txt\">log.txt</a>)</summary><pre>{}</pre></details>",
        lines.len(),
        if shown.len() < lines.len() { ", the last 3000 shown" } else { "" },
        esc(&shown.join("\n"))
    );
    let _ = write!(b, "<h2>Run</h2><pre>{}</pre>", esc(&serde_json::to_string_pretty(&start).unwrap_or_default()));
    Ok(page(&format!("eink-qemu run {}", Path::new(&firmware).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()), &b))
}

fn compare_page(dir: &Path) -> Result<String, String> {
    let s: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("compare.json")).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    let md = crate::compare::markdown(&s);
    let mut b = String::new();
    let (a, c) = (text(&s["names"][0]), text(&s["names"][1]));
    let _ = write!(b, "<h1>eink-qemu compare: {} vs {}</h1><div class=\"muted\">{}</div>", esc(&a), esc(&c), esc(&dir.display().to_string()));
    // The Markdown tables, as HTML tables.
    let mut in_table = false;
    for line in md.lines() {
        if !line.starts_with('|') {
            if in_table {
                b += "</table></div>";
                in_table = false;
            }
            continue;
        }
        if line.starts_with("|---") {
            continue;
        }
        let cells: Vec<String> = line.trim_matches('|').split(" | ").map(|c| c.trim().to_string()).collect();
        if !in_table {
            b += "<h2></h2><div class=\"wrap\"><table><tr>";
            for cell in &cells {
                let _ = write!(b, "<th>{}</th>", esc(cell));
            }
            b += "</tr>";
            in_table = true;
            continue;
        }
        b += "<tr>";
        for cell in &cells {
            let cell = esc(cell).replace("**", "");
            let _ = write!(b, "<td>{}</td>", cell.replace('`', ""));
        }
        b += "</tr>";
    }
    if in_table {
        b += "</table></div>";
    }
    let shots = s["shots"].as_array().cloned().unwrap_or_default();
    if !shots.is_empty() {
        b += "<h2>Screenshots</h2>";
        for sh in shots {
            let _ = write!(b, "<h3>{} {}</h3><div class=\"pair\">", esc(&text(&sh["name"])), if sh["identical"] == true { "<span class=\"ok\">identical</span>" } else { "<span class=\"bad\">differs</span>" });
            for (label, key) in [(a.as_str(), "a"), (c.as_str(), "b"), ("difference", "diff")] {
                if let Some(path) = sh[key].as_str() {
                    let _ = write!(b, "<figure><a href=\"{0}\"><img src=\"{0}\" alt=\"{1}\"></a><figcaption class=\"muted\">{1}</figcaption></figure>", esc(path), esc(label));
                }
            }
            b += "</div>";
        }
    }
    let _ = write!(b, "<h2>Runs</h2><p><a href=\"{0}/report.html\">{1}</a> · <a href=\"{2}/report.html\">{3}</a></p>", esc(&text(&s["runs"][0])), esc(&a), esc(&text(&s["runs"][1])), esc(&c));
    Ok(page(&format!("eink-qemu compare {a} vs {c}"), &b))
}
