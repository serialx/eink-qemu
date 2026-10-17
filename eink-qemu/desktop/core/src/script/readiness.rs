//! When a boot is finished. Any firmware is ready once its screen has been
//! quiet for a while (5 s by default), as long as the ROM is not rebooting
//! it in a loop; ROM messages about an image it cannot boot fail the wait
//! at once. Firmware with a profile (eink-qemu/profiles) is read more
//! closely, with the same rules as the desktop page's readiness.ts:
//! CrossPoint is ready after its boot refreshes (Home drawn), or when it
//! boots into a book after the reader's first page, and then its memory
//! report (a reboot starts the count again, a card it cannot mount fails),
//! and the stock Xteink firmware's boot gate explains a wait for Power.

use crate::profiles::{CROSSPOINT, XTEINK_STOCK};
use regex::Regex;
use std::sync::LazyLock;

/// The ESP ROM or second-stage bootloader refusing the image.
static BOOT_FAILURE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"No bootable app partitions|invalid segment length|Image hash failed|Failed to verify app image|invalid magic byte|Checksum failure|ota data partition invalid and no factory").unwrap()
});

#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// Not yet.
    Waiting,
    /// Ready; how it was decided ("home": Home drawn and reported,
    /// "reader": a book's first page drawn and reported, "quiet": reported
    /// after the screen went quiet, "screen-quiet": no CrossPoint log, the
    /// screen quiet).
    Ready(&'static str),
    /// The boot cannot finish as it is; why.
    Failed(String),
}

#[derive(Default)]
pub struct Readiness {
    milestones: u32,
    /// Host seconds of the last completed refresh, and of the Home screen.
    last_draw: Option<f64>,
    pub home_at: Option<f64>,
    /// The firmware's own timestamp (ms since its boot) of that refresh.
    pub home_fw_ms: Option<u64>,
    /// The screen (activity) CrossPoint entered last in this boot.
    pub screen: Option<String>,
    /// This boot went into a book's reader: its first page, not Home's
    /// refreshes, finishes the boot.
    in_reader: bool,
    /// Host seconds and firmware ms of the reader's first page.
    pub page_at: Option<f64>,
    pub page_fw_ms: Option<u64>,
    /// A memory report came in this boot.
    reported: bool,
    sd_failed: bool,
    /// Seen any of CrossPoint's refresh lines (kept across resets).
    pub crosspoint: bool,
    pub restarts: u32,
    /// Host seconds of the last ROM banner (a boot starting).
    last_rom: Option<f64>,
    pub power_wait: bool,
    boot_failure: Option<String>,
    verdict: Option<Verdict>,
    /// After a reset: ignore lines until the ROM banner of the new boot.
    await_rom: bool,
}

impl Readiness {
    pub fn new() -> Self {
        Self::default()
    }

    /// A fresh count after a reset that keeps what was learned about the
    /// firmware (whether it is CrossPoint).
    pub fn after_reset(previous: &Readiness) -> Self {
        Readiness { crosspoint: previous.crosspoint, await_rom: true, ..Self::default() }
    }

    /// Host seconds of this boot's ROM banner, when the console showed one.
    pub fn rom_banner_at(&self) -> Option<f64> {
        self.last_rom
    }

    /// For a console without the ROM's banner: a boot after a reset counts
    /// from the reset, and shows itself by drawing.
    pub fn skip_rom_wait(&mut self) {
        self.await_rom = false;
    }

    /// Feed one line, logged `now` host seconds after the run started.
    pub fn feed(&mut self, line: &str, now: f64) {
        if self.await_rom {
            if !line.starts_with("ESP-ROM:") {
                return;
            }
            self.await_rom = false;
        }
        if line.starts_with("ESP-ROM:") {
            if self.milestones > 0 || self.last_rom.is_some() {
                // The guest rebooted (a crash, a reset, a firmware restart): count again.
                self.restarts += 1;
            }
            self.milestones = 0;
            self.last_draw = None;
            self.home_at = None;
            self.home_fw_ms = None;
            self.screen = None;
            self.in_reader = false;
            self.page_at = None;
            self.page_fw_ms = None;
            self.reported = false;
            self.sd_failed = false;
            self.power_wait = false;
            self.verdict = None;
            self.last_rom = Some(now);
        }
        if let Some(m) = BOOT_FAILURE.find(line) {
            self.boot_failure.get_or_insert_with(|| format!("the ROM cannot boot the image: {} ({})", m.as_str(), line.trim()));
        }
        let crosspoint = &*CROSSPOINT;
        if crosspoint.sd_failed.is_match(line) {
            self.sd_failed = true;
        }
        if XTEINK_STOCK.power_wait.is_match(line) {
            self.power_wait = true;
        }
        let ran_out = crosspoint.panel_wait.captures(line).and_then(|c| c[1].parse::<u64>().ok()).filter(|&ms| ms >= crosspoint.wait_ran_out_ms);
        if let Some(ms) = ran_out {
            // Not a refresh: the display never answered, as when firmware for
            // one board drives another's panel.
            self.boot_failure.get_or_insert_with(|| {
                format!("the display did not answer the firmware (a {} s panel wait ran out): is this firmware for this board (--machine, --panel)?", ms / 1000)
            });
        } else if crosspoint.refresh_done.is_match(line) {
            self.crosspoint = true;
            self.last_draw = Some(now);
            self.milestones += 1;
            // In a reader these refreshes draw its first page, not Home.
            if self.milestones == crosspoint.milestones && !self.in_reader {
                self.home_at = Some(now);
                self.home_fw_ms = super::metrics::firmware_ms(line.trim_start());
            }
        }
        if self.verdict.is_none() {
            if let Some(c) = crosspoint.activity.captures(line) {
                self.crosspoint = true;
                self.in_reader |= crosspoint.reader_activity.is_match(&c[1]);
                self.screen = Some(c[1].to_string());
            }
            if crosspoint.reader_entered.is_match(line) {
                self.crosspoint = true;
                self.in_reader = true;
            }
            if crosspoint.page_drawn.is_match(line) && self.page_at.is_none() {
                // A page drawn while booting: it booted into a book (also
                // without the activity line, in builds that do not log it).
                self.crosspoint = true;
                self.in_reader = true;
                self.page_at = Some(now);
                self.page_fw_ms = super::metrics::firmware_ms(line.trim_start());
            }
        }
        // A card that fails after the boot was ready (Home, a book or quiet)
        // fails it at the next report: a later `ready` says so.
        if crosspoint.report.is_match(line) && self.sd_failed && matches!(self.verdict, Some(Verdict::Ready(_))) {
            self.verdict = None;
        }
        if crosspoint.report.is_match(line) && self.verdict.is_none() {
            self.reported = true;
            if self.sd_failed {
                self.verdict = Some(Verdict::Failed(
                    "the firmware could not mount the SD card (\"SD card initialization failed\"); check the card image".into(),
                ));
            } else if self.in_reader {
                // Home's refresh count means nothing in a reader: its first
                // page does (decided in `verdict`, which also takes a page
                // drawn after this report).
            } else if self.home_at.is_some() {
                self.verdict = Some(Verdict::Ready("home"));
            } else if self.last_draw.is_some_and(|t| now - t > crosspoint.quiet_after_draw_s) {
                // Firmware that resumes a book or an app draws fewer full
                // refreshes than a boot to Home; quiet and reporting is ready.
                self.verdict = Some(Verdict::Ready("quiet"));
            }
        }
    }

    /// The verdict so far. `screen_quiet` is how long the screen has been
    /// unchanged since an update that belongs to this boot (None before
    /// one); `now` is host seconds since the run started.
    /// A CrossPoint verdict, once reached, stays: later lines (another
    /// screen) do not change the boot that finished.
    pub fn verdict(&mut self, screen_quiet: Option<f64>, quiet_needed: f64, now: f64) -> Verdict {
        if let Some(failure) = &self.boot_failure {
            return Verdict::Failed(failure.clone());
        }
        if let Some(verdict) = &self.verdict {
            return verdict.clone();
        }
        if self.in_reader && !self.sd_failed {
            // The reader's first page, and a memory report before or after
            // it; a fork that does not report is ready this long after it.
            // A reader that logs no page line is ready once long quiet.
            let page = self.page_at.is_some_and(|t| self.reported || now - t >= CROSSPOINT.report_wait_s);
            let quiet = self.page_at.is_none() && self.last_draw.is_some_and(|t| now - t >= CROSSPOINT.report_wait_s);
            let verdict = match (page, quiet) {
                (true, _) => Verdict::Ready("reader"),
                (_, true) => Verdict::Ready("quiet"),
                _ => return Verdict::Waiting,
            };
            self.verdict = Some(verdict.clone());
            return verdict;
        }
        // CrossPoint's Home drawn and no memory report since: a fork that
        // does not report. It is ready this long after Home.
        if self.home_at.is_some_and(|t| now - t >= CROSSPOINT.report_wait_s) && !self.sd_failed {
            self.verdict = Some(Verdict::Ready("home"));
            return Verdict::Ready("home");
        }
        let rom_quiet = self.last_rom.is_none_or(|t| now - t >= quiet_needed);
        // After a reset, nothing counts until the new boot's ROM banner.
        // The stock firmware's power gate turned the device off: its quiet
        // screen is not a finished boot.
        if !self.crosspoint && !self.await_rom && !self.power_wait && rom_quiet && screen_quiet.is_some_and(|q| q >= quiet_needed) {
            return Verdict::Ready("screen-quiet");
        }
        Verdict::Waiting
    }

    /// What it is waiting for, for a timeout message.
    pub fn waiting_for(&self) -> String {
        if self.power_wait {
            return "the firmware turned itself off because Power was not held long enough at boot (stock Xteink firmware): \
                    run with --power-hold 3000, or put press:power:4000 before ready"
                .into();
        }
        if self.restarts > 2 {
            return format!("the guest rebooted {} times (a boot loop?); see log.txt", self.restarts);
        }
        if self.crosspoint && self.in_reader {
            let screen = self.screen.as_deref().unwrap_or("a reader");
            let report = if self.reported { "" } else { ", then a [MEM] report" };
            format!("CrossPoint booted into {screen}: waiting for its first page (a Page render line){report}; restarts: {}", self.restarts)
        } else if self.crosspoint {
            let needed = CROSSPOINT.milestones;
            format!("CrossPoint: {} of {needed} boot refreshes seen, then a [MEM] report; restarts: {}", self.milestones.min(needed), self.restarts)
        } else {
            "the screen never showed a drawn (not blank) image that stayed unchanged long enough (see --ready-quiet)".into()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_then_report() {
        let mut r = Readiness::new();
        for (i, line) in ["[10] [DBG] [EPD] Wait complete: X3_DRF", "Wait complete: X3_DRF", "Wait complete: X3_DRF"].iter().enumerate() {
            r.feed(line, i as f64);
        }
        assert_eq!(r.verdict(None, 5.0, 3.0), Verdict::Waiting);
        r.feed("[11000] [INF] [MEM] Free: 1 bytes", 3.0);
        assert_eq!(r.verdict(None, 5.0, 3.0), Verdict::Ready("home"));
    }

    #[test]
    fn reboot_counts_again_and_sd_failure_fails() {
        let mut r = Readiness::new();
        r.feed("Wait complete: X3_DRF", 0.0);
        r.feed("ESP-ROM:esp32c3-api1-20210207", 1.0);
        assert_eq!(r.restarts, 1);
        r.feed("SD card initialization failed", 2.0);
        r.feed("[MEM] Free: 1 bytes", 3.0);
        assert!(matches!(r.verdict(None, 5.0, 3.0), Verdict::Failed(_)));
    }

    #[test]
    fn a_reboot_after_ready_waits_again() {
        let mut r = Readiness::new();
        for line in ["Wait complete: X3_DRF", "Wait complete: X3_DRF", "Wait complete: X3_DRF", "[MEM] Free: 1 bytes"] {
            r.feed(line, 1.0);
        }
        assert_eq!(r.verdict(None, 5.0, 1.0), Verdict::Ready("home"));
        r.feed("ESP-ROM:esp32c3-api1-20210207", 2.0);
        assert_eq!(r.verdict(Some(10.0), 5.0, 2.5), Verdict::Waiting);
    }

    #[test]
    fn other_firmware_is_ready_when_quiet_but_not_in_a_boot_loop() {
        let mut r = Readiness::new();
        assert_eq!(r.verdict(Some(4.0), 5.0, 10.0), Verdict::Waiting);
        assert_eq!(r.verdict(Some(5.5), 5.0, 10.0), Verdict::Ready("screen-quiet"));
        let mut looping = Readiness::new();
        looping.feed("ESP-ROM:esp32c3-api1-20210207", 8.0);
        looping.feed("invalid segment length 0xffffffff", 8.1);
        assert!(matches!(looping.verdict(Some(9.0), 5.0, 10.0), Verdict::Failed(_)));
        let mut rebooting = Readiness::new();
        rebooting.feed("ESP-ROM:esp32c3-api1-20210207", 8.0);
        assert_eq!(rebooting.verdict(Some(9.0), 5.0, 10.0), Verdict::Waiting);
    }

    #[test]
    fn other_firmware_after_a_reset_waits_for_the_rom_banner() {
        let mut after = Readiness::after_reset(&Readiness::new());
        assert_eq!(after.verdict(Some(6.0), 5.0, 100.0), Verdict::Waiting);
    }

    #[test]
    fn without_the_rom_banner_a_reset_boot_counts_from_the_reset() {
        let mut after = Readiness::after_reset(&Readiness::new());
        after.skip_rom_wait();
        assert_eq!(after.verdict(Some(6.0), 5.0, 100.0), Verdict::Ready("screen-quiet"));
        assert_eq!(after.verdict(None, 5.0, 100.0), Verdict::Waiting);
    }

    #[test]
    fn stock_xteink_power_gate_explains_the_wait() {
        let mut r = Readiness::new();
        r.feed("ESP-ROM:esp32s3-20210327", 0.0);
        r.feed("I (2277) boot-preflight: source=1 validation=2 gate=0 key=1 charge=1 configured=1000ms effective=600ms hold=336ms decision=2 reason=3 auth=0", 1.0);
        r.feed("W (2277) boot-preflight: reject physical boot before init: reason=3", 1.0);
        assert!(!r.crosspoint);
        assert!(r.waiting_for().contains("--power-hold 3000"), "{}", r.waiting_for());
        // The device is off: its quiet screen is not ready, until a new boot.
        assert_eq!(r.verdict(Some(30.0), 5.0, 30.0), Verdict::Waiting);
        r.feed("ESP-ROM:esp32s3-20210327", 31.0);
        assert_eq!(r.verdict(Some(6.0), 5.0, 37.0), Verdict::Ready("screen-quiet"));
    }

    #[test]
    fn esp_idf_firmware_is_ready_on_a_quiet_screen() {
        let mut r = Readiness::new();
        r.feed("ESP-ROM:esp32s3-20210327", 0.0);
        r.feed("I (1930) main_task: Calling app_main()", 2.0);
        assert_eq!(r.verdict(Some(4.0), 5.0, 6.0), Verdict::Waiting);
        assert_eq!(r.verdict(Some(5.0), 5.0, 7.0), Verdict::Ready("screen-quiet"));
    }

    #[test]
    fn a_fork_without_a_report_is_ready_a_while_after_home() {
        let mut r = Readiness::new();
        for line in ["Wait complete: refresh (1 ms)"; 3] {
            r.feed(line, 3.0);
        }
        assert_eq!(r.verdict(None, 5.0, 10.0), Verdict::Waiting);
        assert_eq!(r.verdict(None, 5.0, 15.0), Verdict::Ready("home"));
    }

    #[test]
    fn a_panel_wait_that_ran_out_is_no_refresh_and_fails() {
        let mut r = Readiness::new();
        r.feed("[30301]   Wait complete:  CMD_SOFT_RESET (30001 ms)", 1.0);
        for _ in 0..3 {
            r.feed("[120317]   Wait complete: refresh (29999 ms)", 2.0);
        }
        assert!(r.home_at.is_none());
        let Verdict::Failed(why) = r.verdict(None, 5.0, 3.0) else { panic!() };
        assert!(why.contains("30 s panel wait ran out"), "{why}");
    }

    #[test]
    fn crosspoint_is_remembered_across_a_reset() {
        let mut r = Readiness::new();
        r.feed("Wait complete: X3_DRF", 0.0);
        let mut after = Readiness::after_reset(&r);
        assert!(after.crosspoint);
        assert_eq!(after.verdict(Some(30.0), 5.0, 30.0), Verdict::Waiting);
        // Lines from before the new boot's ROM banner do not count.
        for line in ["Wait complete: X3_DRF", "Wait complete: X3_DRF", "Wait complete: X3_DRF", "[MEM] Free: 1 bytes"] {
            after.feed(line, 31.0);
        }
        assert_eq!(after.verdict(None, 5.0, 31.0), Verdict::Waiting);
        after.feed("ESP-ROM:esp32c3-api1-20210207", 32.0);
        assert_eq!(after.restarts, 0);
    }

    /// CrossPoint X3 waking into the book it slept in (eink-qemu 0.6.0's
    /// emulated X3): its third refresh is in the middle of the first page.
    const WAKE_INTO_BOOK: &[&str] = &[
        "ESP-ROM:esp32c3-api1-20210207",
        "[615] [DBG] [ACT] Entering activity: EpubReader",
        "[615] [INF] [MEM] reader enter: free=152848 max_block=114676",
        "[890]   Wait complete:  X3_PON (128 ms)",
        "[1825]   Wait complete:  X3_DRF (935 ms)",
        "[2532]   Wait complete:  X3_DRF (459 ms)",
        "[3011]   Wait complete:  X3_DRF (383 ms)",
        "[3543]   Wait complete:  X3_DRF (485 ms)",
        "[3885]   Wait complete:  X3_DRF (228 ms)",
        "[3980] [DBG] [ERS] Page render (tiled): prewarm=19ms bw_render=4ms display=2878ms total=3337ms",
        "[3980] [DBG] [ERS] Rendered page in 3337ms",
    ];

    #[test]
    fn a_boot_into_a_book_is_ready_at_its_first_page_not_at_home() {
        let mut r = Readiness::new();
        for (i, line) in WAKE_INTO_BOOK.iter().enumerate() {
            r.feed(line, i as f64);
            // Neither the reader's own [MEM] line nor Home's three refreshes finish it.
            assert_eq!(r.verdict(None, 5.0, i as f64), Verdict::Waiting, "after {line}");
        }
        assert!(r.home_at.is_none());
        assert_eq!((r.page_fw_ms, r.screen.as_deref()), (Some(3980), Some("EpubReader")));
        assert!(r.waiting_for().contains("first page"), "{}", r.waiting_for());
        r.feed("[10028] [INF] [MEM] Free: 148380 bytes, Total: 265020 bytes, Min Free: 136532 bytes, MaxAlloc: 114676 bytes", 20.0);
        assert_eq!(r.verdict(None, 5.0, 20.0), Verdict::Ready("reader"));
        assert_eq!(r.page_at, Some(9.0));
    }

    #[test]
    fn a_page_after_the_report_is_ready_at_once() {
        let mut r = Readiness::new();
        for (i, line) in WAKE_INTO_BOOK[..8].iter().enumerate() {
            r.feed(line, i as f64);
        }
        // A slow first page: the periodic report comes before it.
        r.feed("[10028] [INF] [MEM] Free: 1 bytes, Total: 2 bytes, Min Free: 1 bytes, MaxAlloc: 1 bytes", 10.0);
        assert_eq!(r.verdict(None, 5.0, 10.0), Verdict::Waiting);
        r.feed("[12000] [DBG] [ERS] Page render: prewarm=1ms total=900ms", 12.0);
        assert_eq!(r.verdict(None, 5.0, 12.0), Verdict::Ready("reader"));
    }

    #[test]
    fn without_activity_lines_a_page_still_means_a_reader() {
        let mut r = Readiness::new();
        // A build that logs neither its activity nor the reader's memory line.
        for (i, line) in WAKE_INTO_BOOK.iter().filter(|l| !l.contains("[ACT]") && !l.contains("reader enter")).enumerate() {
            r.feed(line, i as f64);
        }
        // Home's milestone was reached before the page: the page decides.
        assert!(r.home_at.is_some());
        r.feed("[10028] [INF] [MEM] Free: 1 bytes, Total: 2 bytes, Min Free: 1 bytes, MaxAlloc: 1 bytes", 20.0);
        assert_eq!(r.verdict(None, 5.0, 20.0), Verdict::Ready("reader"));
        assert_eq!(r.screen, None);
    }

    #[test]
    fn a_reader_verdict_stays_when_the_screen_changes_later() {
        let mut r = Readiness::new();
        for (i, line) in WAKE_INTO_BOOK.iter().enumerate() {
            r.feed(line, i as f64);
        }
        r.feed("[10028] [INF] [MEM] Free: 1 bytes, Total: 2 bytes, Min Free: 1 bytes, MaxAlloc: 1 bytes", 20.0);
        assert_eq!(r.verdict(None, 5.0, 20.0), Verdict::Ready("reader"));
        r.feed("[30000] [DBG] [ACT] Entering activity: Home", 30.0);
        assert_eq!(r.verdict(None, 5.0, 30.0), Verdict::Ready("reader"));
        assert_eq!(r.screen.as_deref(), Some("EpubReader"));
    }

    #[test]
    fn a_card_failure_after_ready_fails_at_the_next_report() {
        let mut r = Readiness::new();
        for (i, line) in WAKE_INTO_BOOK.iter().enumerate() {
            r.feed(line, i as f64);
        }
        r.feed("[10028] [INF] [MEM] Free: 1 bytes, Total: 2 bytes, Min Free: 1 bytes, MaxAlloc: 1 bytes", 20.0);
        assert_eq!(r.verdict(None, 5.0, 20.0), Verdict::Ready("reader"));
        r.feed("SD card initialization failed", 25.0);
        assert_eq!(r.verdict(None, 5.0, 25.0), Verdict::Ready("reader"));
        r.feed("[20028] [INF] [MEM] Free: 1 bytes, Total: 2 bytes, Min Free: 1 bytes, MaxAlloc: 1 bytes", 30.0);
        assert!(matches!(r.verdict(None, 5.0, 30.0), Verdict::Failed(_)));
        // The same after a boot to Home.
        let mut home = Readiness::new();
        for line in ["Wait complete: X3_DRF", "Wait complete: X3_DRF", "Wait complete: X3_DRF", "[MEM] Free: 1 bytes"] {
            home.feed(line, 1.0);
        }
        assert_eq!(home.verdict(None, 5.0, 1.0), Verdict::Ready("home"));
        home.feed("SD card initialization failed", 2.0);
        home.feed("[MEM] Free: 1 bytes", 3.0);
        assert!(matches!(home.verdict(None, 5.0, 3.0), Verdict::Failed(_)));
    }

    #[test]
    fn a_release_build_enters_the_reader_by_its_memory_line() {
        // LOG_LEVEL=1 keeps INF lines only: no activity or page lines.
        let mut r = Readiness::new();
        r.feed("[615] [INF] [MEM] reader enter: free=152848 max_block=114676", 0.0);
        for _ in 0..3 {
            r.feed("[3011]   Wait complete:  X3_DRF (383 ms)", 3.0);
        }
        assert!(r.home_at.is_none());
        r.feed("[10028] [INF] [MEM] Free: 1 bytes, Total: 2 bytes, Min Free: 1 bytes, MaxAlloc: 1 bytes", 10.0);
        assert_eq!(r.verdict(None, 5.0, 10.0), Verdict::Waiting);
        assert_eq!(r.verdict(None, 5.0, 15.0), Verdict::Ready("quiet"));
    }

    #[test]
    fn a_reader_without_page_lines_or_reports_is_ready_once_long_quiet() {
        let mut r = Readiness::new();
        r.feed("[600] [DBG] [ACT] Entering activity: TxtReader", 0.0);
        for _ in 0..3 {
            r.feed("Wait complete: X3_DRF (1 ms)", 1.0);
        }
        assert_eq!(r.verdict(None, 5.0, 12.0), Verdict::Waiting);
        assert_eq!(r.verdict(None, 5.0, 13.0), Verdict::Ready("quiet"));
    }

    #[test]
    fn a_reader_without_page_lines_is_ready_once_long_quiet() {
        let mut r = Readiness::new();
        r.feed("[600] [DBG] [ACT] Entering activity: TxtReader", 0.0);
        for _ in 0..3 {
            r.feed("Wait complete: X3_DRF (1 ms)", 1.0);
        }
        r.feed("[10000] [INF] [MEM] Free: 1 bytes, Total: 2 bytes, Min Free: 1 bytes, MaxAlloc: 1 bytes", 10.0);
        assert_eq!(r.verdict(None, 5.0, 10.0), Verdict::Waiting);
        assert_eq!(r.verdict(None, 5.0, 13.0), Verdict::Ready("quiet"));
    }

    #[test]
    fn a_boot_to_home_is_unchanged_by_activity_lines() {
        let mut r = Readiness::new();
        for line in ["[1120] [DBG] [ACT] Entering activity: Boot", "[2276]   Wait complete:  X3_DRF (935 ms)", "[2951]   Wait complete:  X3_DRF (383 ms)",
            "[3002] [DBG] [ACT] Entering activity: Home", "[3441]   Wait complete:  X3_DRF (382 ms)"] {
            r.feed(line, 1.0);
        }
        r.feed("[10053] [INF] [MEM] Free: 154556 bytes, Total: 265020 bytes, Min Free: 153812 bytes, MaxAlloc: 114676 bytes", 2.0);
        assert_eq!(r.verdict(None, 5.0, 2.0), Verdict::Ready("home"));
        assert_eq!((r.home_fw_ms, r.screen.as_deref()), (Some(3441), Some("Home")));
        // Opening a book after the boot changes nothing about it.
        r.feed("[11654] [DBG] [ACT] Entering activity: EpubReader", 3.0);
        assert_eq!(r.verdict(None, 5.0, 3.0), Verdict::Ready("home"));
    }
}
