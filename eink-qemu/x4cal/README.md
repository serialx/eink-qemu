# X4 Pro fidelity calibration

The X4 Pro defaults now include measured Xtensa execution costs, CPU clock
scaling, SDMMC command/data/card delays, SPI setup time, and a stateful UC8279
revision `0x68` timing profile. RTC light sleep and deep-sleep restart are also
implemented. These measurements characterize one reader and its installed card;
they do not establish cycle-accurate cache, radio, or optical behavior.

Select the connected reader's panel explicitly:

```sh
X4PANEL=uc8279-68 eink-qemu/run.sh flash.bin sd.img
X4PANEL=uc8279-68 eink-qemu/wasm/run-node.sh flash.bin sd.img
```

The browser uses `?device=x4pro&panel=uc8279-68&turbo=0`. Direct QEMU requires
`-machine x4pro,panel=uc8279-68 -icount shift=0,sleep=off`. The CPU costs are
nanoseconds at 240 MHz; using the old `shift=2` multiplies them by four.
Native, Node, browser and smoke-test launchers now agree on shift zero.
The generic S3, Picco, X3 and legacy UC8179 profile retain their own defaults.
Turbo explicitly disables SDMMC and SPI delays and shortens panel waits.

## Results

Measured October 13, 2026 on an ESP32-S3 rev 0.2, 8 MiB PSRAM, 16 MiB flash,
UC8279 rev `0x68`, and ASTC card with 512-byte sectors, 8 KiB clusters and a
one-bit SDMMC bus. Errors below compare medians of guest elapsed time. They are
not host benchmark results or guarantees for other cards.

| Workload | Hardware | Emulator | Difference |
| --- | ---: | ---: | ---: |
| Held-out rendering loop, SRAM | 149,238 µs | 150,859 µs | +1.09% |
| Held-out rendering loop, cached PSRAM | 149,454 µs | 150,859 µs | +0.94% |
| Cached CPU/memory workloads, including 40/80/160/240 MHz | — | — | −8.64% to +6.86% |
| SD reads/writes, 1–32 sectors, 10/20/40 MHz | — | — | −5.64% to +4.98% |
| SPI, 60,000 bytes, chunks 1–1024, 2.5/5/10 MHz | — | — | −1.13% to +2.15% |
| Panel BUSY, measured PLL/LUT/OTP/power cases | — | — | −0.11% to +0.064% |
| Light sleep, requested 100 ms | 91,649 µs | 91,856 µs | +0.23% |
| Deep sleep, request through next application setup | 266,197 µs | 634,637 µs | +138% |
| Sequential PSRAM, 512 KiB working set | 240,183 µs | 91,366 µs | −62.0% |
| Random PSRAM, 512 KiB working set | 245,114 µs | 19,248 µs | −92.1% |

CPU validation includes three physical boots, five repetitions per case, and
identical frozen v2 firmware on hardware and QEMU. Every compared CPU checksum
matches. The rendering workload and PSRAM rows were excluded from the CPU fit.
The rendering loop is a synthetic packed-pixel workload, not a complete EPUB
page turn. SD validation includes **6,480 verified physical transfers** and
2,160 matching emulator transfers. Keeping repeated/sequential/random patterns
separate gives −5.64% to +5.06% median errors. Individual writes can be much
slower than the median; the model does not reproduce flash garbage collection.

The checked-in CSVs retain individual samples, checksums and error counts.
They exclude device identifiers, allocation addresses and user content.
[manifest.json](evidence/manifest.json) records image/source hashes, the pinned
toolchain, card geometry, sleep records and verified restoration. Recalculate:

```sh
python3 eink-qemu/x4cal/analyze.py --fit-cpu > residuals.json
```

The script checks checksums, transfer errors and paired coverage before reporting
residuals. `--fit-cpu` reproduces the nonnegative least-squares coordinate fit
and integer refinement from the first hardware run and one-nanosecond emulator
sensitivity sweeps. Later v2 hardware repetitions validate the fitted defaults.
The final v4 source extends the panel/SPI/sleep commands; it cannot recreate the
older v2 binary byte for byte. Both frozen images' hashes are recorded. Build
and run the current source on both targets for a new paired experiment.

## Implemented models

**CPU.** Base execution cost is 4 ns in ROM, SRAM and flash. Extra costs are
load 1, store 0, multiply 5, divide 47, conditional branch 6, call/return 12,
FPU 2, normalized FMA 16 ns. These are empirical instruction-class costs, not
pipeline latencies. ROM's unnormalized `maddn.s` uses the ordinary FPU class.
Costs scale with the actual S3 clock registers. Clock changes preserve CCOUNT,
reschedule compares, and invalidate translated blocks. MMIO replay refunds and
maximum block-cost bounds are tested. The secondary core remains gated until
firmware releases it.

**SDMMC.** Command and data phases use virtual deadlines derived from source
clock, dividers, bus width, response length, data bytes and CRC framing. Data
buffers, descriptor ownership and completion IRQs change at completion, not
command submission. The measured card adds 315 µs read latency plus 13 µs per
additional block, capped after three increments; writes add 900 µs plus 16 µs
per block beyond eight. Qtests exercise pending reset, DMA reset, descriptor
ownership, clock gating, bus width, overlap, stop/abort during data or DMA
ownership waits, and recovery after a sticky response error. Mid-transfer clock
disable/re-enable restarts the current phase; it does not preserve fractional
progress. Card-wear and latency-tail distributions are
not modeled.

**SPI.** The existing S3 GPSPI wire-time model adds 650 ns plus two programmed
SPI-clock periods per transfer. These are effective residual costs measured
with this driver, not a claim about a specific unimplemented CS register.
The fit uses chunks 1–64; chunks 100 and 1024 check batching. Generic GPSPI
properties remain zero. The three clock rates and PIO/GDMA completion ordering
are covered by tests.

**Panel.** A seven-byte LUT group uses a group repeat, four phase bytes and two
pair repeats. BUSY follows the longest of all five LUT rows. Repeated hardware
probes covered zero-duration groups, separate phases, group/pair repetition,
unequal rows, neutral and driven gray sequences, and 1–63-frame sweeps.

| Clock state | Frame duration | Refresh overhead |
| --- | ---: | ---: |
| Explicit PLL `0x0d` | 13,378 µs | 84,348 µs |
| Explicit PLL `0x0e` | 12,506 µs | 82,510 µs |
| Explicit PLL `0x0f` | 11,632 µs | 80,690 µs |
| After OTP refresh, no PLL rewrite | 18,903 µs | 95,870 µs |
| Same post-OTP state, measured 480-row partial window | 18,903 µs | 84,585 µs |

OTP full/fast BUSY is 1,328/483 ms. PON is 39,398 µs. POF is
53,625/50,120/46,624 µs at PLL `0x0d/0x0e/0x0f`. A PLL write clears the
post-OTP clock state; reset does too. Other PLL values fall back to `0x0e`;
other partial-window sizes use full-window overhead. POF after an OTP refresh
without a PLL rewrite has not been separately calibrated.

For `uc8279-68`, zero `frame-us` and `refresh-overhead-us` select measured
automatic timing. Set a nonzero value in either property to select literal
`frames * frame-us + refresh-overhead-us` instead. Zero `pon-ms`/`pof-ms`
select automatic power timing; nonzero values override it. `busy-ms` overrides
refresh timing, including OTP, and the existing turbo arguments still work.
The legacy UC8179 interpretation of these properties is unchanged.

**Sleep/reset.** RTC counting now agrees with TIMG calibration, including after
clock-mux changes. RTC continues during sleep while SYSTIMER is suspended;
firmware advances its system time once on wake. The old fixed 5 ms early-wake
adjustment is removed. Calibration multiplication is explicitly 64-bit for
WebAssembly. Timer wake distinguishes deep sleep, records reset cause 5 and
retains RTC memory. CPU gate reset and cancellation of a pending halt allow
both cores to restart. The measured deep-wake result includes the bootloader
and application setup; the remaining boot-latency error is reported above.

## Reproducing the diagnostic

The probe performs no benchmark automatically on boot. Its commands are `info`,
`cpu`, `sd`, `panel`, `matrix`, `vendor`, `spi`, `light`, and `deep`.

```sh
pio run -d eink-qemu/x4cal/probe -e x4pro
python3 eink-qemu/mkflash.py build/x4pro-calibration.local/probe-build/x4pro /tmp/x4cal-flash.bin
python3 eink-qemu/mksd.py /tmp/x4cal-sd.img 514 --cluster-size 8192
python3 eink-qemu/x4cal/run_probe.py \
  --qemu build/qemu-system-xtensa --flash /tmp/x4cal-flash.bin \
  --sd /tmp/x4cal-sd.img --log /tmp/x4cal.log --timeout 240 \
  cpu sd panel matrix vendor spi light deep
```

Use the freshly built emulator path on your machine. The runner never flashes
hardware. It sends benchmark commands once, preserves coalesced serial lines,
and rejects errors, mismatch counts and panel timeouts. Hardware mode uses
`--port /dev/cu.usbmodem…` instead of `--qemu/--flash/--sd` and requires pyserial
and exclusive ownership of that serial port.

Physical flash procedure used for this reader:

1. Read and hash all 16 MiB using the ROM loader (`--no-stub`, 115200 baud).
   Inspect the partition table and OTA selection; verify that app0 at `0x10000`
   is active and that the probe fits. Preserve the backup outside git.
2. Save the original app prefix rounded up to 4096-byte erase sectors. Verify
   current partition/OTA/app headers against the backup immediately before
   upload. Upload **only** `firmware.bin` at the verified app offset; a normal
   multi-image PlatformIO upload is not this procedure.
3. Run the unchanged frozen image on hardware and a disposable emulator flash.
   `sd` creates `0:/EQCAL013.BIN` with `FA_CREATE_NEW`, allocates a contiguous
   4 MiB extent, confines raw I/O to that extent, verifies every transfer, and
   removes the file. An existing file is never overwritten. FAT formatting is
   disabled. An interrupted run can leave the diagnostic file for inspection.
4. In a host `finally` block, restore the entire saved rounded prefix, read that
   range back, and compare bytes/hash. Restore monitor ownership afterward.
   Do this even if measurement collection times out.

On this reader light/deep sleep disconnects USB. Serial reopening also causes
a USB reset. The probe keeps completion records in RTC no-init memory so a
later `info` can distinguish a completed timer wake from that USB reset. Wait
for the port to return before restoration. One v3 restoration attempt initially
failed while the port was absent; retry after re-enumeration succeeded and was
verified. The final v4 session restored and verified 409,600 bytes at `0x10000`;
NVS, OTA selection, bootloader and remaining application bytes were preserved.
The original CrossPoint firmware is installed again.

## Verification and limits

The final native suite runs 100 tests (one optional release fixture skipped);
35 JavaScript tests pass. Native protocol tests cover instruction costs, clock
transitions, SYSTIMER fractional ticks, periodic-alarm phase across sleep, RTC
deadlines/retention, SPI, SDMMC faults, panel identity, LUTs, timing, addressing,
overrides and reset. Panel core tests run with address
and undefined-behavior sanitizers. X3 instruction-cost and drift regressions,
JavaScript tests, native and WebAssembly builds also pass. Firmware acceptance
uses the original CrossPoint release and a generated EPUB/card fixture; native
and Node Home screenshots are byte-identical. Chrome passes boot, touch, key,
Home-return, SD-export and Stop checks with the rebuilt WebAssembly artifacts.
Native and Node also exercise the generated EPUB and page navigation.
This checks firmware behavior,
not physical optical appearance or end-to-end hardware page-turn timing.

Remaining limits are explicit:

- PSRAM cache misses dominate the measured 512 KiB workloads. No data/instruction
  cache, PSRAM bus contention or dual-core contention model was fitted.
- Electrical BUSY is calibrated; optical ghosting, temperature response, charge
  history and unknown OTP waveform bodies still use approximations. No camera,
  photometer, temperature chamber or external logic analyzer was available.
- Timer sleep/reset is exercised; power consumption, battery ADC curves, USB
  power/disconnect timing and complete deep-wake boot latency are not calibrated.
- Touch/key navigation is functionally exercised. Physical touch latency, radio
  RF behavior and network timing were not measured by this diagnostic.
- The installed firmware's screenshot/serial control framing differs from the
  current device monitor. Synthetic checksummed probes supply paired timing;
  the UI screenshots are emulator-only evidence.

Register interpretations were checked against the local ESP-IDF headers and
[Espressif's S3 TRM](https://www.espressif.com/sites/default/files/documentation/esp32-s3_technical_reference_manual_en.pdf).
Panel-specific values above come from the connected controller, not a presumed
UC8179 datasheet.

[verification.json](evidence/verification.json) records the final checks. Native
and Node CPU medians match exactly across 30 groups; SD medians differ by at
most 1 µs across 90 groups, with all checksums matching. The independent final
review found the SD abort and periodic-timer sleep cases described above; both
now have regression tests. The standard browser distribution is rebuilt under
`build-wasm-32limit/web-dist`, and native launchers use refreshed `build/` binaries.
