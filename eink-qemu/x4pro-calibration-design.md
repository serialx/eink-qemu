# X4 Pro hardware calibration

## Goal and sequence

Run unmodified X4 Pro firmware with behavior and elapsed guest time close to
the connected reader. Use identical firmware and declared workloads on the
device and emulator, preserve raw measurements, and report remaining errors.
This is an effective timing model, not a claim of cycle-accurate silicon.

The first change is support for this reader's UC8279 panel, LUT revision 0x68.
CPU/PSRAM and SDMMC calibration follow as separate changes with their own
measurements. A final reading comparison checks their combined behavior.
Keep existing X3 calibration and the existing X4 Pro UC8179 profile working.

## Hardware baseline, 2026-10-13

Source revision inspected: `04e34edecf`. Local evidence is under
`build/x4pro-calibration.local/`, excluded from git by the existing build rule.

| Item | Evidence |
| --- | --- |
| Device | ESP32-S3 revision 0.2, embedded 8 MiB PSRAM; USB Serial/JTAG |
| Installed firmware | CrossPoint `1.6.5-x4pro` |
| Panel identity | `VER=00 0F 68 01 00`, idle `FLG=13` |
| Firmware panel selection | UC8279, visible 800 x 480 |
| Storage | Firmware reports `SDMMC card mounted` |
| PON firmware wait | 40 ms, one initial sample |
| Fast-refresh firmware waits | 482–484 ms, 13 initial UI samples |
| Full-refresh firmware waits | 1327–1328 ms, two initial samples |
| Grayscale firmware wait | 229 ms, one initial sample |
| Previous-gray settling wait | 559 ms, one initial sample |

The raw `8279x4_DRF` label covers both full and fast refresh. Do not pool
these into one calibration median. These are firmware wait observations,
including scheduling/polling effects, not logic-analyzer BUSY pulse widths.
The initial samples identify useful probes; they are not enough to validate
a general fit over temperature, PLL settings, or different panel revisions.

The initial monitor capture also includes a saved-book render and UI actions
that occurred before this task began. It is discovery evidence, not a
controlled paired reading benchmark. Subsequent resets are separate boots.

The installed release does not answer the newer `INFO` serial-control
handshake. Its screenshot response lacks the metadata required by the running
monitor. A screenshot request made the monitor disconnect and reconnect,
causing a USB reset. Keep this failed attempt in the evidence; use a compatible
capture tool or an instrumented diagnostic build for subsequent measurements.

## Why panel support comes first

`hw/xtensa/esp32s3.c` currently creates `uc8179` for every X4 Pro. That device
returns `00 00 01 FF FF`, decodes six-byte LUT groups, and has a 2 ms PON
default. The connected reader returns a different identity and selects a
different firmware driver, which uploads seven-byte LUT groups. Timing the
existing emulator therefore compares different panel code paths.

Two other gaps require separate treatment:

- X4 Pro uses the DesignWare SDMMC controller. Its current model ignores
  clock settings and completes DMA synchronously. X3 SPI/SSI timing defaults
  cannot calibrate this path.
- Xtensa currently uses ordinary instruction counting at four nanoseconds
  per instruction. X3's measured regional and instruction-class costs are
  specific to the ESP32-C3 and must not be copied to the ESP32-S3.

## Approach

Extend the existing X4 Pro panel model with an explicitly selected UC8279
revision-0x68 profile. Reuse its SPI/GPIO transport, ink simulation, console,
and refresh scheduler. Add the seven-byte LUT decoder and the variant's
register behavior where the evidence requires them.

Expose selection as `-machine x4pro,panel=uc8279-68`; retain `panel=uc8179`
as the default for existing callers. A firmware image identifies the SoC,
not the installed glass, so selecting the panel must be explicit. Launchers
and the browser need a documented way to pass the same profile. No firmware
patch or instruction-PC-specific workaround is part of this design.

Alternatives considered:

- Replacing the default UC8179 would change existing firmware behavior and
  discard the previously measured device profile.
- Reusing the X3 UC8279 device directly would import its 792 x 528 geometry,
  rotation, and X3-specific assumptions. Its decoder is useful reference
  code, but it is not a drop-in X4 Pro panel.
- A separate copy of the entire panel implementation would duplicate the
  ink simulation and transport. A profile within the existing implementation
  keeps those behaviors consistent without a broad display refactor.

## Panel behavior and timing

Return the measured five-byte identity and captured MTP header for the new
profile. Clearly document unavailable MTP waveform contents; do not describe
synthetic data as a complete hardware dump.

Decode seven-byte groups as group repetition, four rail/frame bytes, and
two state repetition counts. Preserve phase order, zero repetition, skipped
groups, VCOM, and differing row lengths. A longest-row BUSY policy remains
an explicit modeling assumption until an unequal-row hardware probe checks it.
Exercise the firmware's 49-byte grayscale and 42-byte settling uploads.

Validate resolution, the visible gate offset, partial-window addressing,
OLD/NEW plane handling, PSR orientation, and post-PON PSR writes against the
actual firmware and screen captures. Do not infer correct pixels from boot
success or matching refresh duration alone.

Use QEMU virtual-clock deadlines for PON and refresh completion. Fit register
LUT duration to measured frame totals and a fixed overhead only after
repeated probes support that relationship. Measure OTP fast/full durations
separately. Their synthetic animation frames must span the measured duration;
they are not evidence of the undisclosed OTP waveform's actual frame count.

Keep optical parameters distinct from timing parameters. Existing UC8179
photo fits may serve as initial approximations, but they do not establish
UC8279-0x68 ghosting accuracy. Firmware screenshots verify raster content;
photographs or video are needed to assess physical ghosting and flashing.

Log panel profile, relevant register settings, decoded frame counts, virtual
start/end times, and selected BUSY duration. Invalid profile values must fail
at startup. Reset must cancel pending timers and restore profile defaults.
Property overrides must remain explicit and reproducible.

## Device state and probe protocol

Preserve the installed flash before any firmware upload, hash the backup,
and decode the current partition table. It has NVS at 0x9000, OTA data at
0xe000, app0 at 0x10000, app1 at 0x650000, SPIFFS at 0xc90000, and coredump
at 0xff0000. Any diagnostic upload must use verified application ranges;
do not upload a padded factory image over persistent partitions.

Record settings, reading position, firmware/probe hashes, panel configuration,
SD card identity and geometry, and fixtures before controlled runs. Preserve
the user's unrelated card contents. Diagnostic SD writes must target a
dedicated allocated test file, never arbitrary raw card sectors.

For panel measurements, collect repeated PON/full/fast/grayscale/settling
operations over three boots, with register settings and completion markers.
Keep logging outside timed transfers. Use a reversible diagnostic build only
where release telemetry cannot provide the required observations, and restore
the original firmware afterward. Record host-tool versions separately.

First boot the exact backed-up image on the unchanged emulator to record
the wrong panel selection. A private empty SD image is adequate for that
identity check, but cannot supply a reading-performance comparison. Then
replay the same firmware and controlled panel workload with the new profile.

## Acceptance for the panel change

- Unmodified firmware detects `VER=00 0F 68 01 00` and uses its UC8279 driver.
- Native and browser runs boot, draw, and accept their existing inputs using
  the explicitly selected profile.
- Tests cover both LUT formats, partial addressing and orientation, PON,
  OLD/NEW handling, BUSY edges, reset during refresh, and property validation.
- Repeated hardware/emulator wait medians agree within the larger of 2 ms
  or 2% at the measured configuration. Report range and sample count, not
  just the median. Deviations remain failures or documented limitations.
- Compare matching screen checkpoints for boot, menu navigation, text,
  grayscale, and a page turn. Separate raster checks from optical assessment.
- Existing UC8179, X3 panel/CPU timing, and X4 Pro peripheral regressions pass.
- Retain commands, hashes, raw logs, failed attempts, analysis, and restoration
  evidence. No calibration claim is made for an unmeasured revision or mode.

## Following calibration changes

1. Adapt the existing CPU probe for ESP32-S3: IRAM/flash code, SRAM/PSRAM
   data, integer and floating-point operations, and real rendering paths.
   Record both core configuration and CPU clocks. Fit costs to checksummed
   workloads, validate on held-out rendering, and document cache-miss limits.
2. Probe SDMMC reads and dedicated-file writes across operation sizes and
   access patterns. Match card identity, bus width, bus clock, and FAT geometry.
   Model command, transfer, and card-busy phases with virtual deadlines, then
   validate IRQ/DMA ordering and throughput. Keep generic defaults compatible.
3. Run paired cold/warm reading workloads with identical firmware, books,
   fonts, settings, and cache conditions. Report per-stage and total rendering
   error. Check touch, buttons, low-power transitions, and reset behavior.
4. Investigate deep-sleep wake as a separate functional gap; completing panel
   or timing calibration does not establish that wake is implemented.

## Current verification

This section records the pre-implementation baseline. Completed implementation
checks and remaining acceptance limits are in the
[calibration report](x4pro-calibration-report.md).

A fresh native build of both targets succeeds from the inspected revision.
All 13 existing `eink-qemu.tests.test_x4pro_boot` tests pass with that Xtensa
binary. This verifies the starting baseline only; no model code has changed.

The complete 16 MiB flash backup succeeded through the ESP32-S3 ROM. Its
SHA-256 is `c6ae669eca525b4c8888da5d2ae6812bd4616b458654ba15b59c178965d5d1d0`.
Bootloader, partition table, and NVS match the earlier independent 64 KiB
read. The monitor was reconnected and the device booted its original firmware.
No persistent firmware writes were performed.

A disposable copy of that exact flash reaches Home on the fresh emulator
with a private empty SD image. It selects UC8179 (`VER=00 00 01 FF FF`),
reports a 1 ms PON wait, a 1500 ms full-refresh wait, and 250–251 ms fast
refreshes. These compare with the hardware observations above, but are not
a same-state reading benchmark: the emulator card has no device books,
fonts, or settings. The missing-IMU error is present in both hardware and
emulator logs and predates this work.

Evidence files: `flash-read-rom.log`, `partitions.json`,
`device-baseline.events.jsonl`, `device-baseline.log`,
`emulator-baseline-command.json`, `emulator-baseline.log`,
`build-fresh.log`, and `baseline-qtests.log`, all under the local evidence
directory. The failed RAM-stub read and incompatible screenshot attempt are
also retained. Full flash contents and device logs stay local.
