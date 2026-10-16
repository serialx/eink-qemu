//! Measurements read from the firmware log. For any ESP-IDF firmware: its
//! own timestamps (`I (1234) tag: text`) and how many errors and warnings
//! it logged. For CrossPoint (eink-qemu/profiles/crosspoint.json), with the
//! device-test skill's definitions: page render and total render times,
//! cold layout preparation, and its periodic memory report; its lines look
//! like `[<ms since boot>] [LEVEL] [ORIGIN] text`.

use crate::profiles::CROSSPOINT;
use regex::Regex;
use serde_json::{json, Map, Value};
use std::sync::LazyLock;

/// ESP-IDF's log prefix: level letter, then ms since boot (or since the RTOS
/// tick started, for early lines).
static IDF_LINE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^([EWIDV]) \((\d+)\) ").unwrap());
static VALUES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b([A-Za-z_][A-Za-z0-9_]*)=(-?\d+)(ms|us|B|KB)?\b").unwrap());

/// ANSI color sequences (ESP-IDF's CONFIG_LOG_COLORS wraps lines in them).
static ANSI: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\x1b\[[0-9;]*m").unwrap());

/// A log line without its color codes.
fn plain(line: &str) -> std::borrow::Cow<'_, str> {
    if line.contains('\x1b') { ANSI.replace_all(line, "") } else { std::borrow::Cow::Borrowed(line) }
}

/// The firmware's own timestamp (ms since its boot), when the line has one
/// in ESP-IDF's format (colored or not) or CrossPoint's.
pub fn firmware_ms(line: &str) -> Option<u64> {
    let line = plain(line);
    if let Some(c) = IDF_LINE.captures(&line) {
        return c[2].parse().ok();
    }
    CROSSPOINT.timestamp.captures(&line).and_then(|c| c[1].parse().ok())
}

/// `name=123ms`-style values in a line, for a wait-log record.
pub fn line_values(line: &str) -> Map<String, Value> {
    let mut values = Map::new();
    for c in VALUES.captures_iter(line) {
        let key = match c.get(3).map(|m| m.as_str()) {
            Some("ms") => format!("{}_ms", &c[1]),
            Some("us") => format!("{}_us", &c[1]),
            Some(unit) => format!("{}_{}", &c[1], unit.to_ascii_lowercase()),
            None => c[1].to_string(),
        };
        if let Ok(v) = c[2].parse::<i64>() {
            values.insert(key, json!(v));
        }
    }
    values
}

fn median(values: &[u64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let mut v = values.to_vec();
    v.sort_unstable();
    let n = v.len();
    Some(if n % 2 == 1 { v[n / 2] as f64 } else { (v[n / 2 - 1] + v[n / 2]) as f64 / 2.0 })
}

/// Summary of a run's log (or a slice of it): ESP-IDF's error and warning
/// counts, and CrossPoint's measurements under "crosspoint" when it logged
/// any.
pub fn summarize<'a>(lines: impl IntoIterator<Item = &'a str>) -> Value {
    let cp = &*CROSSPOINT;
    let (mut bw, mut total, mut tiled) = (Vec::new(), Vec::new(), 0usize);
    let mut cold = Vec::new();
    let mut cold_start: Option<u64> = None;
    let mut mem: Vec<Value> = Vec::new();
    let mut min_free: Option<u64> = None;
    let mut rendered = Vec::new();
    let (mut count, mut errors, mut warnings) = (0usize, 0usize, 0usize);
    for line in lines {
        count += 1;
        let line = &*plain(line);
        if let Some(c) = IDF_LINE.captures(line) {
            match &c[1] {
                "E" => errors += 1,
                "W" => warnings += 1,
                _ => {}
            }
            continue;
        }
        if let Some(c) = cp.page_render.captures(line) {
            let (at, b, t): (u64, u64, u64) = (c[1].parse().unwrap_or(0), c[3].parse().unwrap_or(0), c[4].parse().unwrap_or(0));
            bw.push(b);
            total.push(t);
            if c.get(2).is_some() {
                tiled += 1;
            }
            if let Some(start) = cold_start.take() {
                cold.push(at.saturating_sub(start));
            }
        } else if let Some(c) = cp.cold_start.captures(line) {
            cold_start = c[1].parse().ok();
        } else if let Some(c) = cp.mem.captures(line) {
            let n = |i: usize| c[i].parse::<u64>().unwrap_or(0);
            min_free = Some(min_free.map_or(n(4), |m| m.min(n(4))));
            mem.push(json!({"fw_ms": n(1), "free": n(2), "total": n(3), "min_free": n(4), "max_alloc": n(5)}));
        }
        if let Some(c) = cp.rendered.captures(line) {
            rendered.push(c[2].parse::<u64>().unwrap_or(0));
        }
    }
    let mut summary = json!({"log_lines": count, "log_errors": errors, "log_warnings": warnings});
    if !bw.is_empty() || !mem.is_empty() || !rendered.is_empty() || !cold.is_empty() {
        summary["crosspoint"] = json!({
            "page_renders": bw.len(),
            "page_renders_tiled": tiled,
            "bw_render_ms": {"median": median(&bw), "values": bw},
            "total_render_ms": {"median": median(&total), "values": total},
            "cold_layout_ms": cold,
            "rendered_page_in_ms": {"median": median(&rendered), "values": rendered},
            "mem_samples": mem.len(),
            "mem_last": mem.last(),
            "mem_min_free": min_free,
        });
    }
    summary
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_crosspoint_lines() {
        let lines = [
            "[1000] [DBG] [ERS] Cache not found, building",
            "[1900] [DBG] [ERS] Page render (tiled): prewarm=1ms bw_render=400ms gray=3ms total=1300ms",
            "[3000] [DBG] [ERS] Page render: bw_render=300ms total=900ms",
            "[4000] [INF] [MEM] Free: 70000 bytes, Total: 200000 bytes, Min Free: 40000 bytes, MaxAlloc: 48000 bytes",
        ];
        let s = summarize(lines);
        let c = &s["crosspoint"];
        assert_eq!(c["page_renders"], 2);
        assert_eq!(c["page_renders_tiled"], 1);
        assert_eq!(c["bw_render_ms"]["median"], 350.0);
        assert_eq!(c["cold_layout_ms"], json!([900]));
        assert_eq!(c["mem_min_free"], 40000);
        assert_eq!(firmware_ms(lines[1]), Some(1900));
        let v = line_values(lines[1]);
        assert_eq!(v["bw_render_ms"], 400);
        assert_eq!(v["total_ms"], 1300);
    }

    #[test]
    fn reads_esp_idf_lines_without_crosspoint() {
        let lines = [
            "I (1930) main_task: Calling app_main()",
            "W (2277) boot-preflight: reject physical boot before init: reason=3",
            "E (2300) sdmmc: card init failed",
            "plain text",
        ];
        let s = summarize(lines);
        assert_eq!((s["log_lines"].as_u64(), s["log_errors"].as_u64(), s["log_warnings"].as_u64()), (Some(4), Some(1), Some(1)));
        assert!(s.get("crosspoint").is_none());
        assert_eq!(firmware_ms(lines[0]), Some(1930));
        assert_eq!(firmware_ms(lines[3]), None);
        // CONFIG_LOG_COLORS
        let colored = ["\x1b[0;31mE (123) app: failed\x1b[0m", "\x1b[0;33mW (124) app: warning\x1b[0m"];
        let s = summarize(colored);
        assert_eq!((s["log_errors"].as_u64(), s["log_warnings"].as_u64()), (Some(1), Some(1)));
        assert_eq!(firmware_ms(colored[0]), Some(123));
    }
}
