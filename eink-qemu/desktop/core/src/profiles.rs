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
    /// Seconds after Home that make a fork which never reports ready.
    pub report_wait_s: f64,
    /// A panel wait and its guest ms (capture 1); one this long ran out:
    /// the display never answered, so nothing was drawn.
    pub panel_wait: Regex,
    pub wait_ran_out_ms: u64,
    /// Its message for a card it cannot mount.
    pub sd_failed: Regex,
    /// The screen it enters (capture 1), and the ones that are a book's reader:
    /// a boot into a reader is drawn when its first page is (`page_drawn`),
    /// not after Home's refreshes.
    pub activity: Regex,
    pub reader_activity: Regex,
    /// A line entering a book's reader at a level release builds keep (they
    /// drop the activity and page lines).
    pub reader_entered: Regex,
    pub page_drawn: Regex,
    /// Its log prefix, `[<ms since boot>] `.
    pub timestamp: Regex,
    /// Its own error and warning lines, counted with ESP-IDF's.
    pub error: Regex,
    pub warning: Regex,
    pub page_render: Regex,
    pub cold_start: Regex,
    pub mem: Regex,
    pub rendered: Regex,
    pub nvs_seed: NvsSeed,
    /// Functions that only wrap an allocation (its own make_unique): the
    /// allocation site is their caller.
    pub allocation_wrappers: Vec<Regex>,
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
    trace: TraceFile,
}

#[derive(Deserialize)]
struct TraceFile {
    allocation_wrappers: Vec<String>,
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
    report_wait_s: f64,
    sd_failed: String,
    panel_wait: String,
    wait_ran_out_ms: u64,
    activity: String,
    reader_activity: String,
    reader_entered: String,
    page_drawn: String,
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
        report_wait_s: file.ready.report_wait_s,
        panel_wait: regex(&file.ready.panel_wait),
        wait_ran_out_ms: file.ready.wait_ran_out_ms,
        sd_failed: regex(&file.ready.sd_failed),
        activity: regex(&file.ready.activity),
        reader_activity: regex(&file.ready.reader_activity),
        reader_entered: regex(&file.ready.reader_entered),
        page_drawn: regex(&file.ready.page_drawn),
        timestamp: metric("timestamp"),
        error: metric("error"),
        warning: metric("warning"),
        page_render: metric("page_render"),
        cold_start: metric("cold_start"),
        mem: metric("mem"),
        rendered: metric("rendered"),
        nvs_seed: NvsSeed {
            machines: file.nvs_seed.machines.clone(),
            namespace: file.nvs_seed.namespace.clone(),
            values: file.nvs_seed.u8.iter().map(|(k, &v)| (k.clone(), v)).collect(),
        },
        allocation_wrappers: file.trace.allocation_wrappers.iter().map(|p| regex(p)).collect(),
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
        assert_eq!(&CROSSPOINT.activity.captures("[615] [DBG] [ACT] Entering activity: EpubReader").unwrap()[1], "EpubReader");
        assert!(CROSSPOINT.reader_activity.is_match("XtcReader") && !CROSSPOINT.reader_activity.is_match("EpubReaderMenu"));
        assert!(CROSSPOINT.page_drawn.is_match("[3980] [DBG] [ERS] Page render (tiled): prewarm=19ms total=3337ms"));
        assert!(CROSSPOINT.error.is_match("[12559] [ERR] [DEC] No decoder found for image: EPUB/images/cover.svg"));
        assert!(!CROSSPOINT.error.is_match("[12559] [INF] [DEC] [ERR] quoted"));
        assert!(CROSSPOINT.allocation_wrappers[0].is_match("std::unique_ptr<T> makeUniqueNoThrow<ParsedText>(bool&)"));
        assert!(XTEINK_STOCK.power_wait.is_match("W (2277) boot-preflight: reject physical boot before init: reason=3"));
    }

    #[test]
    fn image_markers_need_their_terminator() {
        assert!(CROSSPOINT.is_image(b"..cphw\0..dev_det\0.."));
        assert!(!CROSSPOINT.is_image(b"..cphwx\0..dev_det\0.."));
        assert!(!CROSSPOINT.is_image(b"..cphw\0.."));
    }
}
