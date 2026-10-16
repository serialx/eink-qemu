//! When a boot is finished, from the firmware log: the rules of the desktop
//! page's readiness.ts (CrossPoint's completed refreshes, then its memory
//! report; a reboot starts the count again; a card it cannot mount; the
//! stock X4 Pro firmware's wait for Power). Firmware that logs none of
//! CrossPoint's refresh lines is ready once its screen has been quiet, as
//! long as the ROM is not rebooting it in a loop. ROM messages about an
//! image it cannot boot fail the wait at once.

use regex::Regex;
use std::sync::LazyLock;

/// Panel refresh completions: UC8179/UC8279 boards log their DRF command,
/// the X4 (SSD1677) a plain refresh.
static REFRESH_DONE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"Wait complete:\s+(?:(?:8179|8279|8279x4|X3)_DRF|refresh)\b").unwrap());
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
    /// Seen any CrossPoint-style refresh line (kept across resets).
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
    /// firmware (whether it logs CrossPoint's refresh lines).
    pub fn after_reset(previous: &Readiness) -> Self {
        Readiness { crosspoint: previous.crosspoint, await_rom: true, ..Self::default() }
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
            self.verdict = None;
            self.last_rom = Some(now);
        }
        if let Some(m) = BOOT_FAILURE.find(line) {
            self.boot_failure.get_or_insert_with(|| format!("the ROM cannot boot the image: {} ({})", m.as_str(), line.trim()));
        }
        if line.contains("SD card initialization failed") {
            self.sd_failed = true;
        }
        if line.contains("reject physical boot before init") || line.contains("POWERON without key") {
            self.power_wait = true;
        }
        if REFRESH_DONE.is_match(line) {
            self.crosspoint = true;
            self.last_draw = Some(now);
            self.milestones += 1;
            if self.milestones == 3 {
                self.home_at = Some(now);
                self.home_fw_ms = super::metrics::firmware_ms(line.trim_start());
            }
        }
        if line.contains("[MEM]") && self.verdict.is_none() {
            if self.sd_failed {
                self.verdict = Some(Verdict::Failed(
                    "the firmware could not mount the SD card (\"SD card initialization failed\"); check the card image".into(),
                ));
            } else if self.home_at.is_some() {
                self.verdict = Some(Verdict::Ready("home"));
            } else if self.last_draw.is_some_and(|t| now - t > 3.0) {
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
        if !self.crosspoint && !self.await_rom && rom_quiet && screen_quiet.is_some_and(|q| q >= quiet_needed) {
            return Verdict::Ready("screen-quiet");
        }
        Verdict::Waiting
    }

    /// What it is waiting for, for a timeout message.
    pub fn waiting_for(&self) -> String {
        if self.power_wait {
            return "the firmware is waiting for Power at boot (stock X4 Pro firmware): put press:power:2000 before ready".into();
        }
        if self.restarts > 2 {
            return format!("the guest rebooted {} times (a boot loop?); see log.txt", self.restarts);
        }
        if self.crosspoint {
            format!("{} of 3 boot refreshes seen, then a [MEM] report; restarts: {}", self.milestones.min(3), self.restarts)
        } else {
            "no CrossPoint refresh lines, and the screen did not stay quiet for 5 s".into()
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
