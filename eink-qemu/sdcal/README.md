# X3 SD/SPI calibration

The current X3 defaults use nanosecond CPU costs and a re-fitted SPI setup and
SD stop-busy model. See [CPU calibration and SD refit](cpu.md) for the current
values, 30-row validation, and reading measurements; the values below document
the original plain-instruction-count calibration at `icount shift=2`.

For new firmware timing comparisons, use the stock X3 card's **32 KiB FAT32
clusters**, now the `mksd.py` default, or explicitly match the card under test.
Copying the same files into an image with smaller clusters changes the number
of FAT reads needed for font seeks. Record geometry alongside the workload
before fitting emulator timing parameters. See [SD image geometry](../../README.md#sd-image-geometry).

All 30 median totals meet the requested limits: the largest absolute error is
**2.92% for reads** (limit 5%) and **13.12% for writes** (limit 15%). The unchanged
probe completed 60 groups / 2,880 operations with zero errors and reached Home.
The device reference pools three boots, 288 operations per row; the emulator
run has 96 operations per row. This calibration targets 40 MHz SPI with
`-icount shift=2,sleep=on` and the probe's SdFat SHARED-mode access pattern.

The implementation extends commits `8796d32dc6` and `9e4b81d58d`; the calibration
commit is titled `x4prosim: calibrate SPI SD timing against the X3 card`.

## Median totals

Each row compares the same operation, start-sector pattern and sector count.
`rep` repeats the previous start, `seq` starts after the previous end, and `rnd`
uses a nonadjacent start. Error is `(emulator / device - 1) * 100`.

| Op | Pattern | Sectors | Device median (us) | Emulator median (us) | Error |
| --- | --- | ---: | ---: | ---: | ---: |
| r | rep | 1 | 301 | 296 | -1.66% |
| r | rep | 2 | 457 | 454 | -0.66% |
| r | rep | 4 | 770 | 772 | +0.26% |
| r | rep | 8 | 1398 | 1406 | +0.57% |
| r | rep | 32 | 5374 | 5217 | -2.92% |
| r | rnd | 1 | 398 | 392 | -1.51% |
| r | rnd | 2 | 555 | 551 | -0.72% |
| r | rnd | 4 | 876 | 868 | -0.91% |
| r | rnd | 8 | 1523.5 | 1503 | -1.35% |
| r | rnd | 32 | 5329.5 | 5313 | -0.31% |
| r | seq | 1 | 331 | 323 | -2.42% |
| r | seq | 2 | 482 | 481 | -0.21% |
| r | seq | 4 | 798 | 799 | +0.13% |
| r | seq | 8 | 1404 | 1434 | +2.14% |
| r | seq | 32 | 5307 | 5244 | -1.19% |
| w | rep | 1 | 671 | 666 | -0.75% |
| w | rep | 2 | 705 | 697 | -1.13% |
| w | rep | 4 | 913 | 904 | -0.99% |
| w | rep | 8 | 1352 | 1317 | -2.59% |
| w | rep | 32 | 5280 | 5175 | -1.99% |
| w | rnd | 1 | 813 | 803 | -1.23% |
| w | rnd | 2 | 810 | 899 | +10.99% |
| w | rnd | 4 | 1066 | 1104 | +3.56% |
| w | rnd | 8 | 1746 | 1517 | -13.12% |
| w | rnd | 32 | 5279 | 5175 | -1.97% |
| w | seq | 1 | 747 | 738 | -1.20% |
| w | seq | 2 | 748.5 | 698 | -6.75% |
| w | seq | 4 | 909 | 904 | -0.55% |
| w | seq | 8 | 1352 | 1317 | -2.59% |
| w | seq | 32 | 5280 | 5175 | -1.99% |

PASS: 30/30 rows; worst read 2.92%, write 13.12%.

## Final defaults and mechanism

Generic defaults remain zero. Only X3 receives the calibrated defaults below;
X4 Pro continues to use its clock-derived bus duration with zero setup costs.

| Device | Property | X3 value | Units / scope |
| --- | --- | ---: | --- |
| GPSPI | `transaction-overhead-us` | 0 | Microseconds, additive base setup |
| GPSPI | `transaction-overhead-ns` | 1450 | Nanoseconds on every transaction |
| GPSPI | `buffer-overhead-ns` | 1875 | Extra nanoseconds when the transaction has more than one byte |
| SSI SD | `read-access-us` | 207 | Random first-token deadline from R1 |
| SSI SD | `read-seq-access-us` | 136 | Sequential first-token deadline |
| SSI SD | `read-repeat-us` | 108 | Repeated-start first-token deadline |
| SSI SD | `read-next-us` | 12 | CRC end to next CMD18 token |
| SSI SD | `write-busy-us` | 554 | Sequential CMD24 data-response busy |
| SSI SD | `write-random-busy-us` | 621 | Random CMD24 busy |
| SSI SD | `write-repeat-busy-us` | 479 | Repeated-start CMD24 busy |
| SSI SD | `write-block-busy-us` | 10 | Every CMD25 block's data-response busy |
| SSI SD | `write-stop-busy-us` | 480 | STOP_TRAN base busy |
| SSI SD | `write-stop-decrement-us` | 26 | Busy reduction per completed CMD25 block |
| SSI SD | `write-stop-random-extra-us` | 205 | Extra STOP busy for a random-start command |

STOP busy is `max(0, 480 + (random ? 205 : 0) - 26 * blocks)` microseconds,
with the general property calculation saturated at UINT32_MAX. The command's
start pattern is retained through every block and STOP. Repeated starts take
precedence over sequential starts, and read/write histories are independent.
CMD24 now uses the existing `write-busy-us` / `write-random-busy-us` names plus
`write-repeat-busy-us`; CMD25 uses the independent `write-block-busy-us`.

The guest's existing instruction and polling time is part of each measured
phase, so configured deadlines are not identical to observed phase durations.
A bus-only probe with 2 us fixed setup produced 27 us commands but 137 us read
transfers, versus 22/147 us on the device. One constant could not fit both:
the final base setup plus buffered setup produces 22 us commands and about
146–147 us per read block without changing SCLK or firmware.

The initial device estimates of roughly 107/135/204 us first-token waits and
470/545/610 us single-write waits guided the respective properties. The
12 us CMD18 gap and 10 us CMD25 block busy come from the measured sustained
block behavior. The post-stop fit differs from the proposed B=445/D=57 because
the firmware prepares the next write buffer while the deadline elapses:
roughly 85/150/275 us is already gone before the next 2/4/8-sector command.
The emulator must not charge that preparation a second time.

## Phase comparison and limits

Pairs below are **device / emulator median microseconds**. Meeting total-time
tolerances does not imply an exact fit to every phase or to the device's tails.

| Op | Pattern | Sectors | Command | Wait | Transfer | Stop |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| r | rep | 1 | 22 / 22 | 107 / 106 | 147 / 146 | 19 / 19 |
| r | rep | 2 | 22 / 22 | 114 / 118 | 294 / 292 | 19 / 19 |
| r | rep | 4 | 22 / 21 | 128 / 141 | 588 / 585 | 19 / 19 |
| r | rep | 8 | 22 / 22 | 156 / 187 | 1179 / 1170 | 19 / 19 |
| r | rep | 32 | 22 / 21.5 | 539 / 467 | 4715 / 4680 | 19 / 19 |
| r | rnd | 1 | 22 / 22 | 204 / 203 | 147 / 146 | 19 / 19 |
| r | rnd | 2 | 22 / 22 | 211 / 215 | 294 / 292 | 19 / 19 |
| r | rnd | 4 | 22 / 22 | 229 / 238 | 588 / 585 | 19 / 19 |
| r | rnd | 8 | 22 / 22 | 283 / 284 | 1178 / 1170 | 19 / 19 |
| r | rnd | 32 | 22 / 22 | 494 / 563 | 4716 / 4679.5 | 19 / 19 |
| r | seq | 1 | 22 / 22 | 135 / 133 | 147 / 146 | 19 / 19 |
| r | seq | 2 | 22 / 22 | 138 / 145 | 294 / 292.5 | 19 / 19 |
| r | seq | 4 | 22 / 21 | 155 / 168 | 588 / 585 | 19 / 19 |
| r | seq | 8 | 22 / 22 | 159.5 / 214 | 1179 / 1170 | 19 / 19 |
| r | seq | 32 | 22 / 22 | 467 / 494 | 4715 / 4680 | 19 / 19 |
| w | rep | 1 | 39 / 40 | 470 / 468 | 150 / 150 | 0 / 0 |
| w | rep | 2 | 353 / 366 | 23 / 12 | 300 / 301 | 19 / 16 |
| w | rep | 4 | 236 / 251 | 43 / 31 | 601 / 601 | 19 / 16 |
| w | rep | 8 | 22 / 22 | 83 / 68 | 1203 / 1203 | 19 / 16 |
| w | rep | 32 | 22 / 22 | 323 / 294 | 4829 / 4813 | 19 / 16 |
| w | rnd | 1 | 39 / 40 | 610.5 / 606 | 150 / 150 | 0 / 0 |
| w | rnd | 2 | 458 / 568 | 23 / 12 | 300 / 301 | 19 / 16 |
| w | rnd | 4 | 386 / 451 | 43 / 31 | 601 / 602 | 19 / 16 |
| w | rnd | 8 | 411 / 222 | 83 / 68 | 1204 / 1203 | 19 / 15 |
| w | rnd | 32 | 22 / 22 | 323 / 294 | 4828 / 4813 | 19 / 16 |
| w | seq | 1 | 39 / 40 | 545 / 540 | 150 / 150 | 0 / 0 |
| w | seq | 2 | 396 / 366 | 23 / 12 | 300 / 301 | 19 / 15 |
| w | seq | 4 | 230 / 251 | 43 / 31 | 601 / 602 | 19 / 16 |
| w | seq | 8 | 22 / 22 | 83 / 68 | 1203 / 1203 | 19 / 16 |
| w | seq | 32 | 22 / 22 | 323 / 293 | 4829 / 4812.5 | 19 / 16 |

The largest write residuals are random 2-sector writes (+10.99%) and random
8-sector writes (-13.12%). The supplied table's random 8-sector command phase
is 411 us, contradicting the preliminary suggestion that all n>=8 writes have
no residual stop busy. A single linear stop-credit rule with a random-start
premium cannot reproduce that shape exactly: this fit gives 222 us there and
568 us for random n=2 (device 458 us), while keeping every total within 15%.
No per-row lookup table or random noise was added to hide the discrepancy.

Repeated 32-sector reads are 2.92% fast; their wait phase is 467 us versus
539 us on the device. Sequential 8-sector reads are 2.14% slow; their wait
phase is 215 us versus 159.5 us. The common 12 us inter-block deadline fits the
required totals without pretending to reproduce the card's complete read-ahead
behavior. Single-write phase differences also include the guest's instruction
and poll granularity. Long device programming stalls, including the pathological
sequential single-write boot, are not simulated; pooled medians were used.

The probe reports `cpu=160,type=3` (SDHC) on the device and `cpu=80,type=2`
(SDSC) in this emulator configuration. No CPU-clock or firmware changes were
made to conceal that difference. Guest microsecond measurements, not the
reported CPU frequency, are the calibration target; SDSC/SDHC address handling
is covered by the adapter checks. Other firmware, icount settings, DMA paths,
cards, occupancy, wear and clock rates require separate validation.

## Reproduce and evidence

The raw emulator console (`calibration-console.log`, kept with the session evidence, not in git) contains all 2,880 operations,
`DONE groups=60 ops=2880 errors=0`, and `Entering activity: Home`.
The copied runner changes only the host launcher to use no shared monitor
socket or GUI, accepts optional `SD_TIMING_EXTRA` QEMU arguments and allows
600 seconds for host execution. It never edits or flashes the probe firmware.

These historical results used a 512 MiB SDSC image. The command below selects
4 KiB clusters explicitly to retain the old helper's geometry. The new 2050 MiB
default is advertised as SDHC and changes the card's addressing mode, so it
cannot reproduce this configuration.

From this worktree after `ninja -C build`:

```sh
mkdir -p /tmp/sdcal
eink-qemu/mkflash.sh /Users/serialx/orca/workspaces/crosspoint-reader/sd-timing-probe/.pio/build/default /tmp/sdcal/flash.bin
python3 eink-qemu/mksd.py /tmp/sdcal/sd.img 512 --cluster-size 4096
python3 eink-qemu/sdcal/run_qemu.py "$PWD" "$PWD//tmp/sdcal"
python3 eink-qemu/sdcal/parse.py /tmp/sdcal/console.log --json /tmp/sdcal/results.json
python3 eink-qemu/sdcal/compare.py device-results.json /tmp/sdcal/console.log
```

The reference inputs remain unchanged in the worktree root. Their pooled JSON
sets `complete=false` because its parser expects exactly one DONE marker; the
input actually contains three successful DONE records, totaling 8,640 operations.
The comparison checks 30 rows and the emulator's 96 samples per row explicitly.
All writable flash/card images and logs are in this worktree. No physical device
or other checkout was modified.

Both targets build with no warnings in the changed files; the linker still
prints the pre-existing duplicate `-liconv` warning. Exact X4 Pro qtest checks
cover additive microsecond/nanosecond setup, buffered-only setup, USR and timer
boundaries, and UC8179 delivery/BUSY. QMP verifies all X3 defaults and explicit
overrides, including zero. Isolated adapter tests with a mock SDBus cover repeated,
sequential and random SDSC/SDHC access, the CMD24/CMD25 split, STOP credit,
zero defaults, and uint32 saturation under ASan/UBSan. The probe itself exercises
the actual SD backend, including written-sector verification. Existing X3 CPU
initialization still prevents the qtest accelerator from running that machine;
its firmware runs under TCG. End-to-end migration was not tested.

## Input fingerprints

- `device-table.txt` SHA-256: `4d273c374357c7e9948254215e59905fd7497479afff711ad63a0acc61b62291`
- `device-results.json` SHA-256: `627c26ebb1b2c2ab98659710968b36f9514e6bc5c989786a74419cb2265793b1`
- `all-boots.log` SHA-256: `3bc1e93291830f632842f88a3ceabf234e16282d6f46716b9f36660483e549c6`
- `calibration-console.log` SHA-256: `6406a66e5800578cb95debf1046a9d3014ee8e2b2104a9549bb3a1c790c3a060`
- `firmware.bin` SHA-256: `69580efd64905cbb374c41bd3cf0087507b2002f76b8241b6853e89dc47be3f1`
