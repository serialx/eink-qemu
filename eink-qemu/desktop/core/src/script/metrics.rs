//! Measurements read from the firmware log, with the device-test skill's
//! definitions: CrossPoint's page render and total render times, cold
//! layout preparation, and its periodic memory report. Lines look like
//! `[<ms since boot>] [LEVEL] [ORIGIN] text`.

use regex::Regex;
use serde_json::{json, Map, Value};
use std::sync::LazyLock;

static PAGE_RENDER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\[(\d+)\] \[DBG\] \[ERS\] Page render( \(tiled\))?: .*?bw_render=(\d+)ms .*?total=(\d+)ms").unwrap()
});
static COLD_START: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[(\d+)\] \[DBG\] \[ERS\] Cache not found, building").unwrap());
static MEM: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\[(\d+)\] \[INF\] \[MEM\] Free: (\d+) bytes, Total: (\d+) bytes, Min Free: (\d+) bytes, MaxAlloc: (\d+) bytes").unwrap()
});
static RENDERED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[(\d+)\] \[\w+\] \[ERS\] Rendered page in (\d+)\s*ms").unwrap());
static STAMP: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\[(\d+)\] ").unwrap());
static VALUES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b([A-Za-z_][A-Za-z0-9_]*)=(-?\d+)(ms|us|B|KB)?\b").unwrap());

/// The firmware's own timestamp (ms since its boot), when the line has one.
pub fn firmware_ms(line: &str) -> Option<u64> {
    STAMP.captures(line).and_then(|c| c[1].parse().ok())
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

/// Summary of a run's log (or a slice of it).
pub fn summarize<'a>(lines: impl IntoIterator<Item = &'a str>) -> Value {
    let (mut bw, mut total, mut tiled) = (Vec::new(), Vec::new(), 0usize);
    let mut cold = Vec::new();
    let mut cold_start: Option<u64> = None;
    let mut mem: Vec<Value> = Vec::new();
    let mut min_free: Option<u64> = None;
    let mut rendered = Vec::new();
    for line in lines {
        if let Some(c) = PAGE_RENDER.captures(line) {
            let (at, b, t): (u64, u64, u64) = (c[1].parse().unwrap_or(0), c[3].parse().unwrap_or(0), c[4].parse().unwrap_or(0));
            bw.push(b);
            total.push(t);
            if c.get(2).is_some() {
                tiled += 1;
            }
            if let Some(start) = cold_start.take() {
                cold.push(at.saturating_sub(start));
            }
        } else if let Some(c) = COLD_START.captures(line) {
            cold_start = c[1].parse().ok();
        } else if let Some(c) = MEM.captures(line) {
            let n = |i: usize| c[i].parse::<u64>().unwrap_or(0);
            min_free = Some(min_free.map_or(n(4), |m| m.min(n(4))));
            mem.push(json!({"fw_ms": n(1), "free": n(2), "total": n(3), "min_free": n(4), "max_alloc": n(5)}));
        }
        if let Some(c) = RENDERED.captures(line) {
            rendered.push(c[2].parse::<u64>().unwrap_or(0));
        }
    }
    json!({
        "page_renders": bw.len(),
        "page_renders_tiled": tiled,
        "bw_render_ms": {"median": median(&bw), "values": bw},
        "total_render_ms": {"median": median(&total), "values": total},
        "cold_layout_ms": cold,
        "rendered_page_in_ms": {"median": median(&rendered), "values": rendered},
        "mem_samples": mem.len(),
        "mem_last": mem.last(),
        "mem_min_free": min_free,
    })
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
        assert_eq!(s["page_renders"], 2);
        assert_eq!(s["page_renders_tiled"], 1);
        assert_eq!(s["bw_render_ms"]["median"], 350.0);
        assert_eq!(s["cold_layout_ms"], json!([900]));
        assert_eq!(s["mem_min_free"], 40000);
        assert_eq!(firmware_ms(lines[1]), Some(1900));
        let v = line_values(lines[1]);
        assert_eq!(v["bw_render_ms"], 400);
        assert_eq!(v["total_ms"], 1300);
    }
}
