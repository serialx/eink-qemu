//! What eink-qemu knows about particular firmware, from the shared files in
//! eink-qemu/profiles (the web front end reads the same ones). Everything
//! else in the core assumes only ESP-IDF: ROM and bootloader messages,
//! crash reports, `I (1234) tag:` log lines and the heap allocator.

use regex::Regex;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::sync::LazyLock;

const CROSSPOINT_JSON: &str = include_str!("../../../profiles/crosspoint.json");
const XTEINK_STOCK_JSON: &str = include_str!("../../../profiles/xteink-stock.json");

/// CrossPoint Reader and its forks.
pub struct CrossPoint {
    /// Strings (NUL-terminated in the image) that mark a CrossPoint app.
    pub image_strings: Vec<String>,
    /// A completed panel refresh, as its display driver logs it.
    pub refresh_done: Regex,
    /// Refreshes of a boot to Home.
    pub milestones: u32,
    /// Its periodic memory report, which follows a finished boot.
    pub report: Regex,
    /// Seconds after the last refresh that make a report "ready" without Home.
    pub quiet_after_draw_s: f64,
    /// Its message for a card it cannot mount.
    pub sd_failed: Regex,
    /// Its log prefix, `[<ms since boot>] `.
    pub timestamp: Regex,
    pub page_render: Regex,
    pub cold_start: Regex,
    pub mem: Regex,
    pub rendered: Regex,
    pub nvs_seed: NvsSeed,
}

/// An NVS entry the firmware expects (u8 values in one namespace).
pub struct NvsSeed {
    pub machines: Vec<String>,
    pub namespace: String,
    pub values: Vec<(String, u8)>,
}

/// Xteink's stock firmware.
pub struct XteinkStock {
    /// Its boot gate turning the device off: Power was not held long enough.
    pub power_wait: Regex,
}

#[derive(Deserialize)]
struct CrossPointFile {
    detect: Detect,
    ready: CrossPointReady,
    metrics: BTreeMap<String, String>,
    nvs_seed: SeedFile,
}

#[derive(Deserialize)]
struct Detect {
    image_strings: Vec<String>,
}

#[derive(Deserialize)]
struct CrossPointReady {
    refresh_done: String,
    milestones: u32,
    report: String,
    quiet_after_draw_s: f64,
    sd_failed: String,
}

#[derive(Deserialize)]
struct SeedFile {
    machines: Vec<String>,
    namespace: String,
    u8: BTreeMap<String, u8>,
}

#[derive(Deserialize)]
struct XteinkStockFile {
    ready: StockReady,
}

#[derive(Deserialize)]
struct StockReady {
    power_wait: String,
}

fn regex(pattern: &str) -> Regex {
    Regex::new(pattern).unwrap_or_else(|e| panic!("profile pattern {pattern}: {e}"))
}

pub static CROSSPOINT: LazyLock<CrossPoint> = LazyLock::new(|| {
    let file: CrossPointFile = serde_json::from_str(CROSSPOINT_JSON).expect("profiles/crosspoint.json");
    let metric = |name: &str| regex(file.metrics.get(name).unwrap_or_else(|| panic!("profiles/crosspoint.json: no metrics.{name}")));
    CrossPoint {
        image_strings: file.detect.image_strings.clone(),
        refresh_done: regex(&file.ready.refresh_done),
        milestones: file.ready.milestones,
        report: regex(&file.ready.report),
        quiet_after_draw_s: file.ready.quiet_after_draw_s,
        sd_failed: regex(&file.ready.sd_failed),
        timestamp: metric("timestamp"),
        page_render: metric("page_render"),
        cold_start: metric("cold_start"),
        mem: metric("mem"),
        rendered: metric("rendered"),
        nvs_seed: NvsSeed {
            machines: file.nvs_seed.machines.clone(),
            namespace: file.nvs_seed.namespace.clone(),
            values: file.nvs_seed.u8.iter().map(|(k, &v)| (k.clone(), v)).collect(),
        },
    }
});

pub static XTEINK_STOCK: LazyLock<XteinkStock> = LazyLock::new(|| {
    let file: XteinkStockFile = serde_json::from_str(XTEINK_STOCK_JSON).expect("profiles/xteink-stock.json");
    XteinkStock { power_wait: regex(&file.ready.power_wait) }
});

impl CrossPoint {
    /// Whether an app image is CrossPoint's: it holds every marker string,
    /// NUL-terminated.
    pub fn is_image(&self, app: &[u8]) -> bool {
        self.image_strings.iter().all(|s| {
            let mut needle = s.as_bytes().to_vec();
            needle.push(0);
            app.windows(needle.len()).any(|w| w == needle.as_slice())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles_load() {
        assert!(CROSSPOINT.refresh_done.is_match("[4015]   Wait complete:  8279x4_DRF (484 ms)"));
        assert!(CROSSPOINT.report.is_match("[10010] [INF] [MEM] Free: 1 bytes"));
        assert_eq!(CROSSPOINT.nvs_seed.values, [("dev_det".to_string(), 2)]);
        assert!(XTEINK_STOCK.power_wait.is_match("W (2277) boot-preflight: reject physical boot before init: reason=3"));
    }

    #[test]
    fn image_markers_need_their_terminator() {
        assert!(CROSSPOINT.is_image(b"..cphw\0..dev_det\0.."));
        assert!(!CROSSPOINT.is_image(b"..cphwx\0..dev_det\0.."));
        assert!(!CROSSPOINT.is_image(b"..cphw\0.."));
    }
}
