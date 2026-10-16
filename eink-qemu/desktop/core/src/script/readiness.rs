//! When a boot is finished. Any firmware is ready once its screen has been
//! quiet for a while (5 s by default), as long as the ROM is not rebooting
//! it in a loop; ROM messages about an image it cannot boot fail the wait
//! at once. Firmware with a profile (eink-qemu/profiles) is read more
//! closely, with the same rules as the desktop page's readiness.ts:
//! CrossPoint is ready after its boot refreshes and then its memory report
//! (a reboot starts the count again, a card it cannot mount fails), and the
//! stock Xteink firmware's boot gate explains a wait for Power.

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
    /// "quiet": reported after the screen went quiet, "screen-quiet": no
    /// CrossPoint log, the screen quiet).
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
        if crosspoint.refresh_done.is_match(line) {
            self.crosspoint = true;
            self.last_draw = Some(now);
            self.milestones += 1;
            if self.milestones == crosspoint.milestones {
                self.home_at = Some(now);
                self.home_fw_ms = super::metrics::firmware_ms(line.trim_start());
            }
        }
        if crosspoint.report.is_match(line) && self.verdict.is_none() {
            if self.sd_failed {
                self.verdict = Some(Verdict::Failed(
                    "the firmware could not mount the SD card (\"SD card initialization failed\"); check the card image".into(),
                ));
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
    pub fn verdict(&self, screen_quiet: Option<f64>, quiet_needed: f64, now: f64) -> Verdict {
        if let Some(failure) = &self.boot_failure {
            return Verdict::Failed(failure.clone());
        }
        if let Some(verdict) = &self.verdict {
            return verdict.clone();
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
        if self.crosspoint {
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
        r.feed("[MEM] Free", 3.0);
        assert!(matches!(r.verdict(None, 5.0, 3.0), Verdict::Failed(_)));
    }

    #[test]
    fn a_reboot_after_ready_waits_again() {
        let mut r = Readiness::new();
        for line in ["Wait complete: X3_DRF", "Wait complete: X3_DRF", "Wait complete: X3_DRF", "[MEM] Free"] {
            r.feed(line, 1.0);
        }
        assert_eq!(r.verdict(None, 5.0, 1.0), Verdict::Ready("home"));
        r.feed("ESP-ROM:esp32c3-api1-20210207", 2.0);
        assert_eq!(r.verdict(Some(10.0), 5.0, 2.5), Verdict::Waiting);
    }

    #[test]
    fn other_firmware_is_ready_when_quiet_but_not_in_a_boot_loop() {
        let r = Readiness::new();
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
        let after = Readiness::after_reset(&Readiness::new());
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
    fn crosspoint_is_remembered_across_a_reset() {
        let mut r = Readiness::new();
        r.feed("Wait complete: X3_DRF", 0.0);
        let mut after = Readiness::after_reset(&r);
        assert!(after.crosspoint);
        assert_eq!(after.verdict(Some(30.0), 5.0, 30.0), Verdict::Waiting);
        // Lines from before the new boot's ROM banner do not count.
        for line in ["Wait complete: X3_DRF", "Wait complete: X3_DRF", "Wait complete: X3_DRF", "[MEM] Free"] {
            after.feed(line, 31.0);
        }
        assert_eq!(after.verdict(None, 5.0, 31.0), Verdict::Waiting);
        after.feed("ESP-ROM:esp32c3-api1-20210207", 32.0);
        assert_eq!(after.restarts, 0);
    }
}
