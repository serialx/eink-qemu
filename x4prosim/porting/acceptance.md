# QEMU 11.1 port acceptance

Tested on 2026-10-09, macOS arm64, branch `serialx/qemu-rebase`, source
commit `49f40e8a82`. **The old-versus-new functional comparisons pass**, with
the qualifications below: X4 Pro pixels and normalized logs are not identical,
and the upstream qtest suite has a defect reproduced on both binaries. No
scenario that succeeded on the old emulator failed functionally on the port.
This is not a claim of exact timing or pixel equivalence on X4 Pro.

The coordinator explicitly authorized continuing after the baseline qtest
failure and grading the port on old-versus-new scenarios. No C sources,
firmware, reference images, or files in the reference checkout were changed.

## Results

All evidence links below are relative to
[acceptance-evidence.local](acceptance-evidence.local/), an uncommitted directory
whose internal `.gitignore` ignores every entry. `old/` means the binaries from
`/Users/serialx/workspace/x4prosim/build`; `new/` means this worktree's `build/`.
Their versions are 9.2.2 and 11.1.2; binary hashes and input fingerprints are in
[manifest.json](acceptance-evidence.local/manifest.json).

| Scenario | Old result | New result | Difference | Verdict / evidence |
| --- | --- | --- | --- | --- |
| Build both targets | Existing reference binaries, not rebuilt | Exit 0, no warnings | Not applicable | PASS: [build.log](acceptance-evidence.local/build.log) |
| X3 Home, CrossPoint 1.6.5 | Home | Home | 0 bytes; 0/418176 pixels | PASS: [old](acceptance-evidence.local/old/home.png), [new](acceptance-evidence.local/new/home.png) |
| X3 Down three times | Settings selected | Settings selected | 0 bytes; 0 pixels | PASS: [old](acceptance-evidence.local/old/down3.png), [new](acceptance-evidence.local/new/down3.png) |
| X3 Enter (`confirm`) | Settings screen | Settings screen | 0 bytes; 0 pixels | PASS: [old](acceptance-evidence.local/old/enter.png), [new](acceptance-evidence.local/new/enter.png) |
| X3 Back | Home, Settings selected | Home, Settings selected | 0 bytes; 0 pixels | PASS: [old](acceptance-evidence.local/old/back.png), [new](acceptance-evidence.local/new/back.png) |
| X3 normalized log | 35 lines | 36 lines | 13 removed, 14 added; 2125-byte unified diff | DIFF: [x3-log.diff](acceptance-evidence.local/x3-log.diff); detection/timing differences discussed below |
| SD probe completion | 60 groups, 2880 operations, zero errors, Home | Same | No completion/error-count difference | PASS: [old console](acceptance-evidence.local/old/sdcal/console.log), [new console](acceptance-evidence.local/new/sdcal/console.log) |
| SD median timing versus device | 0/30 rows within documented limits | 30/30 pass; worst read 3.85%, write 11.23% | Old/new median differences up to 5251 us | PASS for new calibration; baseline fails: [full 30-row comparison](acceptance-evidence.local/sdcal-table.md), [new checker output](acceptance-evidence.local/new/sdcal/compare.log) |
| SD normalized log | 2916 lines | 2917 lines | 2886 removed, 2887 added; 565684-byte diff, mostly operation timings | DIFF: [sdcal-log.diff](acceptance-evidence.local/sdcal-log.diff) |
| X4 Pro Home, CrossPoint 1.6.5-x4pro | Home | Home | 613/384000 pixels (0.1596%), max channel delta 1/255 | Functional PASS; exact pixel equality FAIL: [old](acceptance-evidence.local/old/x4pro/home.png), [new](acceptance-evidence.local/new/x4pro/home.png) |
| X4 Pro keyboard-test-exit replay | Runs to Settings | Runs to Settings | `T-kbd`: 11 pixels; `T-after`: 12 pixels; max delta 1/255 | Execution PASS, keyboard ghosting NOT VALIDATED with this firmware: [old after](acceptance-evidence.local/old/x4pro/T-after.png), [new after](acceptance-evidence.local/new/x4pro/T-after.png) |
| X4 Pro normalized log | 83 lines | 86 lines | 19 removed, 22 added; 4670-byte diff | DIFF: [x4pro-log.diff](acceptance-evidence.local/x4pro-log.diff) |
| X4 Pro ROM-only fallback | Not run | Not run | Not applicable | N/A: an S3 image was found, so the image scenario was used |
| Upstream `virt` + icount | Not requested | OpenSBI v1.8.1 banner, clean termination | Not applicable | PASS: [virt.log](acceptance-evidence.local/virt.log) |
| Upstream riscv32 qtest suite | Full suite not rerun; focused RGB diagnostic aborts | 9 pass, 1 fail, 1 skip, 24.93 s | Same RGB abort on old and new | FAIL, pre-existing and excluded from port grade by coordinator: [suite log](acceptance-evidence.local/qtest.log), [full log](acceptance-evidence.local/qtest-full.log) |

## Inputs and reproducible commands

The X3 image and both unmodified input copies have SHA-256
`80b8d1de62ff9ade1bf5770177e23283677ce13427b724a3e7091409fab8b12a`.
Each run received a fresh 512 MiB FAT32 card made with `mksd.py`. `drive.py`
creates another `.run` flash copy for guest writes. The reference `sd.img` was
not used or modified. The old and new `drive.py` files are identical.

The commands below used the `mksd.py` revision from this historical acceptance,
which chose cluster size automatically. Current `mksd.py` defaults to stock-X3
32 KiB clusters and requires a larger image; see
[SD image geometry](../../README.md#sd-image-geometry) for new comparisons.

From the worktree root at the recorded revision:

```sh
ninja -C build qemu-system-xtensa qemu-system-riscv32
E="$PWD/x4prosim/porting/acceptance-evidence.local"
OLD=/Users/serialx/workspace/x4prosim
# Run once with LABEL=old TREE="$OLD", once with LABEL=new TREE="$PWD".
cp "$OLD/x3-flash.bin" "$E/$LABEL/x3-flash.bin"
python3 x4prosim/mksd.py "$E/$LABEL/sd.img" 512
QEMU_EXTRA='-nic none' python3 "$TREE/x4prosim/drive.py" \
  "$E/$LABEL/x3-flash.bin" "$E/$LABEL/sd.img" "$E/$LABEL/firmware.log" \
  wait:25 "shot:$E/$LABEL/home.png" \
  press:down press:down press:down wait:3 "shot:$E/$LABEL/down3.png" \
  press:confirm wait:5 "shot:$E/$LABEL/enter.png" \
  press:back wait:5 "shot:$E/$LABEL/back.png"
```

The driver's X3 key property is `confirm`, not `enter`. Commands and environment
are also stored in each run's `command.json`. The driver uses `-icount
shift=0,sleep=on` for X3 and `shift=2,sleep=on` for X4 Pro, `-display none`,
`-serial null`, a file USB-CDC chardev, and a private Unix monitor socket.

Pillow is absent from the host Python. `cmp -s` verifies byte equality for all
four X3 PNG pairs. For nonidentical X4 PNGs, the evidence-only
[png_compare.py](acceptance-evidence.local/png_compare.py) decodes their 8-bit
RGB/RGBA scanlines using Python's standard library and counts differing pixels.
It also confirms zero differing pixels for X3. Results are in
[x3-pixel-comparison.json](acceptance-evidence.local/x3-pixel-comparison.json) and
[x4pro-comparison.json](acceptance-evidence.local/x4pro-comparison.json).
Logs are normalized only by replacing bracketed decimal timestamps with
`[TIME]` and normalizing line endings. Timing measurements, memory values,
controller names, and operation results remain intact; they are not filtered
away to manufacture equality.

## X3 and SD observations

The new X3 firmware log through initial display updates includes:

```text
ESP-ROM:esp32c3-api1-20210207
[530] [INF] [HW] Using cached device type: X3
[642] [XTDET] X3 stock probe VER=FF FF FF BUSY-timeout=0 -> UC8253
[658] [INF] [GYR] SDK IMU initialized
[659] [INF] [CLK] SDK RTC found[659] [INF] [MAIN] Hardware detect: X3
[671] [SD] SD card detected
[1065]   Wait complete:  X3_PON (127 ms)
[2000]   Wait complete:  X3_DRF (935 ms)
[2675]   Wait complete:  X3_DRF (383 ms)
[3164]   Wait complete:  X3_DRF (383 ms)
[3650]   Wait complete:  X3_DRF (382 ms)
```

The supplied release image has no literal Home activity log; the Home screenshot
is the acceptance evidence. Both runs emit the same six Wire lock/NULL-buffer
errors during boot; the full logs retain them. The reference executable reports
`VER=00 03 66`, promotes the panel to UC8279, and has different refresh timings.
The port uses the current source's UC8253 response. Periodic memory samples also
differ despite identical Settings pixels. Thus the old executable is not an
exact behavioral baseline for the latest source defaults.

The path named by `sdcal/README.md` for the original probe build is absent.
An archived, already merged probe image was found instead:

```text
/Users/serialx/workspace/x4prosim-evidence/sd-timing-evidence.local/calibration/worker-artifacts/probe-flash.bin
SHA-256 b9fac07061bc42013a18dd8870fdf7e8debcb74ba6d86e6aef213c06235efc38
```

The archived device reference at the same tree's
`calibration/device-results.json` matches the README fingerprint exactly:
`627c26ebb1b2c2ab98659710968b36f9514e6bc5c989786a74419cb2265793b1`.
Each probe run used a copied image and fresh card in `$E/$LABEL/sdcal/`:

```sh
python3 x4prosim/sdcal/run_qemu.py "$TREE" "$E/$LABEL/sdcal"
python3 x4prosim/sdcal/compare.py \
  /Users/serialx/workspace/x4prosim-evidence/sd-timing-evidence.local/calibration/device-results.json \
  "$E/$LABEL/sdcal/console.log"
```

Both runners exit 0 with `Verified DONE groups=60 ops=2880 errors=0 and Entering
activity: Home`. New `compare.py` exits 0, meeting the documented 5% read / 15%
write limits with 96 samples per row. The old checker exits 1 immediately:

```text
AssertionError: (('r', 'rep', 1), -98.00664451827242)
```

The old median for that row is 6 us versus device 301 us and new 303 us.
All 30 old rows fail these limits. This is an improvement relative to the supplied
binary, not a port regression. The scripted SD probe covers reads, writes and
verification; the separate Aesop/Demian page-turn measurements in `sdcal/cpu.md`
were not repeated because that document does not supply a self-contained reading
replay with its book/card inputs.

## X4 Pro image and replay

An actual ESP32-S3 factory image was found at:

```text
/Users/serialx/orca/workspaces/crosspoint-reader/feat-add-serial-device-control-for-hardware-test/.pio/build/x4pro/firmware.factory.bin
SHA-256 13d21a8ee91f3f0aa4ab0948fd962d4849ccb6e747886104157c5b7c277baa5a
```

Its 5,884,928 bytes were copied verbatim to each `x4pro/flash.bin`, followed by
`0xff` padding to the board's 16 MiB flash size. The entire original prefix was
verified equal; no firmware code, partition, or NVS bytes were changed.
Each run used another fresh SD card and the same `drive.py` command shape with
the steps from `ghosting/test/keyboard-test-exit-steps.txt`. Only screenshot
destinations were remapped into evidence, and a Home shot was added after the
initial `wait:25`. Exact argument arrays are stored in
[old command](acceptance-evidence.local/old/x4pro/command.json) and
[new command](acceptance-evidence.local/new/x4pro/command.json).

Both logs identify CrossPoint `1.6.5-x4pro`, UC8179, and `Entering activity: Home`
(old guest time 2647 ms, new 2762 ms). The touch sequence enters Settings on both.
The replay was written for CrossDink's keyboard UI; this CrossPoint firmware
does not enter that keyboard. Its `T-kbd`/`T-after` names therefore describe
capture positions in the script, not proof of a keyboard ghost-removal test.

All 16 public panel properties are identical across the binaries, including
drift and remnant settings; see [panel-properties.json](acceptance-evidence.local/panel-properties.json).
The source diff from `x4prosim-pre-rebase` changes UC8179 QEMU APIs, not its ink
model. The small nonzero differences are consistent with the time-dependent
drift/remnant model and different refresh timestamps; this is an explanation
supported by the model, not proof of their exact cause. No tolerance was added
and no panel parameter was overridden to make the images match.

## Upstream checks and baseline defect

The OpenSBI smoke command was run for five seconds, then terminated cleanly:

```sh
./build/qemu-system-riscv32 -M virt -bios default -icount shift=2 -nographic
```

It prints `OpenSBI v1.8.1` and `Platform Name : riscv-virtio,qemu`.
There is no `check-qtest-riscv32` Ninja target in this build. Its Meson suite
equivalent ran all 11 registered tests in 24.93 seconds:

```sh
build/pyvenv/bin/meson test -C build --suite qtest-riscv32 \
  --print-errorlogs --num-processes 4
```

Exit 1: nine tests pass, `cdrom-test` skips (zero applicable subtests), and
`device-introspect-test` aborts at `display.esp.rgb`:

```text
RAMBlock "esp-rgb-vram" already registered, abort!
Broken pipe
../tests/qtest/libqtest.c:210: kill_qemu() detected QEMU death from signal 6 (Abort trap: 6)
TAP parsing error: Too few tests run (expected 6, got 5)
```

A focused reproduction starts either old or new riscv32 binary with
`-M none -display none -monitor none -qmp stdio -S`, then sends:

```json
{"execute":"qmp_capabilities"}
{"execute":"device-list-properties","arguments":{"typename":"display.esp.rgb"}}
{"execute":"device-list-properties","arguments":{"typename":"display.esp.rgb"}}
{"execute":"quit"}
```

Both processes abort with signal 6 and the identical RAMBlock error; see
[old diagnostic](acceptance-evidence.local/old/rgb-introspect.log) and
[new diagnostic](acceptance-evidence.local/new/rgb-introspect.log).
`esp_rgb_init()` allocates fixed-name VRAM and initializes an address space
whose root holds an owner reference, without matching teardown. Temporary
introspection instances consequently retain the RAM registration. The old source
contains the same allocation/lifetime pattern. A follow-up should move runtime
allocation to realization and provide appropriate teardown, then rerun this
suite; merely changing the name would not address the lifetime issue.

Acceptance initially stopped here as instructed. Coordinator message
`msg_6319323fa617` authorized resuming SD and S3 comparisons and explicitly
excluded this pre-existing failure from the port grade. No fix is included in
this acceptance commit.

## Follow-up: exact baseline comparison after review fixes

On 2026-10-09 the exact `x4prosim-pre-rebase` sources
(`f9339150cd030918e72c8470800fe5ef5cc86e1d`) were built in temporary
`build/pre-rebase` using the documented host configuration. Its unmodified
Xtensa binary and the fixed port ran the same X4 Pro Home scenario with copies
of the same S3 image and fresh SD cards. Result: **0 / 384000 differing pixels,
and byte-identical PNGs**. The earlier 613-pixel, one-level difference was due
to the stale reference binary; no UC8179 or display-path change was needed.

The final X3 Home is also byte-identical to this report's original
`acceptance-evidence.local/new/home.png`. Both upstream suites now pass:
RISC-V 10 pass / 0 fail / 1 skip; Xtensa 8 pass / 0 fail / 2 skip, including
both `device-introspect-test` runs. The baseline worktree was removed after
testing. See [fixes.md](fixes.md) for commits, reset tables, exact commands,
evidence paths, and the inherited concerns that remain open.
