//! Records as JSON lines on stdout, and short progress lines on stderr for
//! people (when stderr is a terminal, or with --progress).

use serde_json::Value;
use std::io::{IsTerminal, Write};
use std::sync::Mutex;

pub struct Printer {
    progress: bool,
    lock: Mutex<()>,
}

impl Printer {
    pub fn new(quiet: bool, progress: bool) -> Printer {
        Printer { progress: !quiet && (progress || std::io::stderr().is_terminal()), lock: Mutex::new(()) }
    }

    pub fn record(&self, record: &Value) {
        let _held = self.lock.lock().unwrap();
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{record}");
        let _ = out.flush();
        if self.progress {
            if let Some(line) = describe(record) {
                let _ = writeln!(std::io::stderr().lock(), "{line}");
            }
        }
    }

    /// A message for people only (stderr), shown unless --quiet.
    pub fn note(&self, text: &str) {
        if self.progress {
            let _held = self.lock.lock().unwrap();
            let _ = writeln!(std::io::stderr().lock(), "{text}");
        }
    }
}

fn s(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// One line about a record.
pub fn describe(r: &Value) -> Option<String> {
    let side = r.get("side").and_then(Value::as_str).map(|s| format!("[{s}] ")).unwrap_or_default();
    match r["type"].as_str()? {
        "start" => Some(format!(
            "{side}eink-qemu: {} on {} ({} timing{}) · {}",
            r["firmware"].as_str().unwrap_or_default(),
            r["machine"].as_str().unwrap_or_default(),
            r["timing"].as_str().unwrap_or_default(),
            if r["elf"].is_null() { ", no ELF" } else { "" },
            r["run_dir"].as_str().unwrap_or_default()
        )),
        "step" => {
            let mark = if r["ok"] == true { "ok  " } else { "FAIL" };
            let detail = match r["step"].as_str().unwrap_or_default() {
                "ready" => format!("{} at {} s", s(&r["how"]), s(&r["boot_s"])),
                "wait-log" => s(&r["line"]),
                "shot" | "heap" | "trace-stop" => {
                    let mut d = s(&r["file"]);
                    if r["step"] == "heap" {
                        d += &format!(" free {} largest {}", s(&r["free"]), s(&r["largest"]));
                    }
                    if r["step"] == "trace-stop" {
                        d += &format!(" allocs {} kept {} B", s(&r["allocs"]), s(&r["kept_bytes"]));
                    }
                    d
                }
                "wait-screen" => s(&r["frame"]),
                "hmp" => s(&r["output"]).lines().next().unwrap_or_default().to_string(),
                _ => String::new(),
            };
            let mut line = format!("{side}{mark} {:>7.2}s  {}  {}", r["t"].as_f64().unwrap_or(0.0), s(&r["text"]), detail);
            if let Some(e) = r["error"].as_str() {
                line += &format!("\n       {e}");
                if let Some(lines) = r["last_lines"].as_array() {
                    for l in lines {
                        line += &format!("\n       | {}", s(l));
                    }
                }
            }
            Some(line.trim_end().to_string())
        }
        "end" => Some(format!(
            "{side}{} {} steps in {:.1} s · {}",
            if r["ok"] == true { "PASS" } else { "FAIL" },
            s(&r["steps"]),
            r["duration_s"].as_f64().unwrap_or(0.0),
            r["report"].as_str().or(r["run_dir"].as_str()).unwrap_or_default()
        )),
        "error" => Some(format!("{side}error: {}", s(&r["error"]))),
        "compare" => None,
        _ => None,
    }
}
