# x4prosim: instructions for helper agents

x4prosim is based on upstream **QEMU 11.1.2**, with ported Espressif SoC models,
and simulates the **Xteink X4 Pro** (ESP32-S3R8, 16 MB flash, 8 MB octal PSRAM) closely enough to run **any
unmodified X4 Pro firmware**: the stock firmware, CrossInk, CrossDink, or a bare
ESP-IDF/Arduino app. It also simulates the **Xteink X3** (ESP32-C3; see the X3
section) for the CrossPoint X3/X4 binary. The goal is firmware development without the physical device:
the screen, SD card, keys, touch, clock and battery behave like the board.

Rule zero: **never change a firmware to make it boot in QEMU.** Model the hardware
instead, from the datasheets and the board, not from one firmware's driver: another
firmware may use the same chip differently (hardware SPI CS, DMA, other panel
commands). CrossDink (`prokrypt/CrossDink`, read-only for you) is the reference
firmware for testing because its drivers are readable and it logs a lot.

## Porting layers

See [x4prosim/porting/README.md](x4prosim/porting/README.md) for the QEMU 11.1.2
porting branches, layer ownership, build command and commit rules.

## Repo, branches, workflow

- Repo: `prokrypt/x4prosim`. Integration branch: **`x4prosim`** (base: upstream
  QEMU 11.1.2).
- Work on your own branch off `x4prosim` (name it `ext/<area>`, e.g. `ext/i2c`) and
  open a PR into `x4prosim`. Keep each PR to one peripheral.
- Only touch the files your task names plus `hw/xtensa/esp32s3.c` (machine wiring)
  and the matching `meson.build`/`Kconfig`. Ask before editing shared files
  someone else owns (table below).
- Style: QEMU C style, 4-space indent. One-line comment only where the why isn't
  obvious. Each new device: a short header comment naming the registers it models
  and what it fakes.
- Commit messages: `x4prosim: <what>`.

## Build and run

Use Python 3.9 or newer, GLib 2.66 or newer, and libgcrypt 1.9.4 or newer
(Debian 12 satisfies these requirements). QEMU configure selects Meson 1.5 or
newer, installing the bundled 1.11.1 wheel when needed. Rust is disabled by
default in this QEMU 11.1.2 tree; no Rust toolchain is needed for these builds.
A fresh configure needs network access for missing Python packages and subprojects.

```sh
# deps (Debian/Ubuntu)
sudo apt install build-essential git pkg-config ninja-build libglib2.0-dev \
  libpixman-1-dev libpng-dev libgcrypt20-dev libslirp-dev libsdl2-dev \
  dosfstools mtools python3 python3-venv
python3 -m venv .venv && . .venv/bin/activate
python3 -m pip install esptool
mkdir build && cd build
../configure --target-list=xtensa-softmmu,riscv32-softmmu --enable-gcrypt --enable-slirp --enable-sdl --enable-png \
  --disable-gnutls --disable-strip --disable-user --disable-capstone --disable-vnc --disable-gtk --disable-docs
ninja qemu-system-xtensa qemu-system-riscv32
```
macOS: `brew install glib pixman ninja pkg-config python3 libgcrypt libpng libslirp sdl2 dosfstools mtools`.

### Firmware image

The easiest route is an official CrossPoint release app `.bin`. Use the
`crosspoint-<version>-x3-x4.bin` asset for X3 (ESP32-C3), or
`crosspoint-<version>-x4pro.bin` for X4 Pro (ESP32-S3):

```sh
python3 x4prosim/mkflash.py crosspoint-1.6.5-x3-x4.bin flash.bin
python3 x4prosim/mkflash.py crosspoint-1.6.5-x4pro.bin flash-x4pro.bin
```

This needs only Python 3, detects the chip, adds the bundled bootloader and
CrossPoint partition table, and preserves the app bytes. Bootloader provenance,
licenses and regeneration commands are in [x4prosim/boot/](x4prosim/boot/README.md).
C3 images include the cached X3 device type in NVS; S3 NVS stays erased.
`--machine x3|x4pro` checks that the image matches the requested machine.

Other inputs:
- A PlatformIO or ESP-IDF build: `python3 x4prosim/mkflash.py <build dir> flash.bin`
  merges its `bootloader.bin`, `partition*.bin` and app (`firmware.bin`, or the
  only other `.bin`) at 0x0/0x8000/0x10000. Nested IDF boot/table directories work.
- A complete 16 MiB flash image passes through unchanged:
  `python3 x4prosim/mkflash.py dump.bin flash.bin`.
- A real-device dump: `esptool.py --chip esp32s3 read_flash 0 0x1000000 flash.bin`
  (works only if the device does not use flash encryption).

`mkflash.sh` remains a compatibility wrapper. `run.sh` and `drive.py` select
X3 or X4 Pro from the resulting bootloader header.

CrossDink example: `pio run -e x4-pro-debug` in a CrossDink checkout, then:

```sh
python3 x4prosim/mkflash.py <CrossDink>/.pio/build/x4-pro-debug flash.bin
x4prosim/run.sh flash.bin sd.img          # firmware log (USB-CDC) on stdout, SDL window if available
x4prosim/drive.py flash.bin sd.img log.txt wait:30 shot:home.png   # headless
```

`run.sh`/`drive.py` create a 2050 MiB MBR+FAT32 `sd.img` with 32 KiB clusters
if it doesn't exist. `x4prosim/mksd.py sd.img 2050 books/` creates an image
populated from a folder. This matches the measured stock X3 card's cluster size;
see [SD image geometry](README.md#sd-image-geometry) before timing comparisons.

### CrossDink build gotchas (PlatformIO, pioarduino 6.1.19)

- **`ModuleNotFoundError: No module named 'SCons.Tool.FortranCommon'`** at the
  `firmware.elf` link step: the `tool-scons` package is half-installed. Delete it
  and rebuild; pio reinstalls it: `rm -rf ~/.platformio/packages/tool-scons`.
- **`*** Reinstall Arduino framework ***`** on the first build in a new checkout is
  normal (pioarduino keys its hybrid-compiled IDF on `sdkconfig.defaults`). It
  re-downloads `framework-arduinoespressif32` from GitHub releases and recompiles
  the IDF libs, so the first build takes several minutes.
- **`CERTIFICATE_VERIFY_FAILED` / `self-signed certificate in certificate chain`**
  during that download: pio's own Python uses its bundled certifi, which doesn't
  trust a corporate or sandbox proxy CA. Point it at the system bundle:
  `ln -sf /etc/ssl/certs/ca-certificates.crt "$(~/.platformio/penv/bin/python -c 'import certifi; print(certifi.where())')"`
  (or your proxy's CA file).
- **Never run two `pio run` at once**, even in different checkouts: they share
  `~/.platformio` and one will wipe the framework under the other.
- CI artifacts carry `firmware.bin` only. You need a local build for
  `firmware.elf` (symbols), `bootloader.bin` and `partitions.bin`.

Keep `firmware.elf` beside you for symbols. To find a hang: run with `-s -S`, attach
`xtensa-esp32s3-elf-gdb firmware.elf` (`target remote :1234`), let it run, Ctrl-C,
`bt`. Or `info registers -a` on the HMP socket `/tmp/x4prosim-mon.sock` and
`xtensa-esp32s3-elf-addr2line -pfiaC -e firmware.elf <PC>`.

Unmapped peripheral registers read as 0 and ignore writes (catch-all
`esp32s3.iomem`), so a missing model usually shows up as a busy-wait on a status
bit, an `ESP_ERR_INVALID_STATE`, or a timeout in the log. Map new devices with
`memory_region_add_subregion_overlap(..., 1)` so they win over the catch-all.

## WebAssembly build

The Emscripten build uses Kohei Tokunaga's **wasm64 TCG JIT backend**: cold
translation blocks run through TCI, and hot blocks become Wasm modules at
runtime. Both X3 (`qemu-system-riscv32.js`) and X4 Pro
(`qemu-system-xtensa.js`) are built. This replaces the older QEMU 9.2.2 TCI
port on `serialx/wasm`; firmware images need no changes.

### Build and choose an address mode

Use Emscripten **6.0.12** (also pinned in CI), Python 3.9+ and Node **26**.
The local verification used Homebrew Emscripten 6.0.12 and Node 26.11.0 on
macOS arm64. On Linux, install and activate emsdk 6.0.12 before building.

```sh
# macOS prerequisites
brew install emscripten ninja pkg-config autoconf automake libtool dosfstools mtools
# Ubuntu prerequisites (plus activated emsdk 6.0.12)
sudo apt install build-essential autoconf automake libtool libltdl-dev pkg-config ninja-build \
  meson python3-venv texinfo gettext dosfstools mtools

x4prosim/wasm/build-deps.sh          # build-wasm-deps/sysroot-wasm64
x4prosim/wasm/build.sh --web         # default: build-wasm-32limit/web-dist
x4prosim/wasm/run-node.sh flash.bin sd.img
python3 x4prosim/wasm/serve.py build-wasm-32limit/web-dist
```

Both modes use 64-bit host pointers and the same wasm64 dependency sysroot.
The mode controls the emitted Wasm memory addressing, not the guest CPU:

| Selection | Emscripten setting | Output directory | Browser requirement |
| --- | --- | --- | --- |
| `WASM64_MODE=32` (default) | `MEMORY64=2`, 32-bit address limit | `build-wasm-32limit/` | No Memory64 required; compatible with modern Chrome, Firefox and Safari |
| `WASM64_MODE=64` | `MEMORY64=1`, native Memory64 | `build-wasm/` | Chrome 133+, Firefox 134+; not Safari |

Memory64 support is tracked in [MDN's compatibility data](https://github.com/mdn/browser-compat-data/blob/main/webassembly/memory64.json).
The default mode is the portable option across those browsers. Both modes
still require Wasm threads, exception handling and cross-origin isolation.
Actual emulator acceptance was run in **Chrome 155**, with GPU/WebGL enabled,
and Node 26; Firefox and Safari have not been tested with this emulator.

```sh
# Native Memory64: keep the mode consistent for build, launch and packaging.
WASM64_MODE=64 x4prosim/wasm/build.sh --web
WASM64_MODE=64 x4prosim/wasm/run-node.sh flash.bin sd.img
python3 x4prosim/wasm/serve.py build-wasm/web-dist
```

`JOBS` sets build parallelism; `EM_CACHE` selects a writable compiler cache.
For a fresh checkout, export `WASM_SYSROOT=/absolute/path/to/sysroot-wasm64`
before both build scripts to reuse dependencies built with the same compiler.
Do not reuse the old TCI port's 32-bit sysroot. Installed dependencies are
skipped; `FORCE=1` rebuilds them. Do not rebuild a shared sysroot concurrently.
The scripts pin dependency sources and keep generated files in ignored build
directories. QEMU configure can download missing Python packages/subprojects.

### Run, package and verify

Open `http://127.0.0.1:8000`, choose an official CrossPoint release app `.bin`
or a 16 MiB flash dump and optionally an SD image. Use
`crosspoint-<ver>-x3-x4.bin` for X3 or `crosspoint-<ver>-x4pro.bin` for X4 Pro
from the [CrossPoint releases](https://github.com/crosspoint-reader/crosspoint-reader/releases).
The browser adds the bundled bootloader, partition table and C3 NVS seed;
app bytes stay unchanged. No esptool or PlatformIO is needed.
`?flash=flash.bin&sd=sd.img` accepts either input kind from a URL; for example,
`?flash=crosspoint-1.6.5-x3-x4.bin&sd=sd.img` with copies in the served directory.
App header chip IDs 5 and 9 select X3 and X4 Pro; unsupported IDs are rejected.
For full dumps, bootloader byte 12 equal to 5 selects X3; otherwise X4 Pro.
The browser has a portrait SDL panel, CDC log, keys and X4 Pro pointer input.
Reset resets the guest; Stop retains the SD in page memory for the next Start.
Download SD image saves guest writes. Closing or reloading loses unsaved
changes; flash changes are not exported. A blank 64 MiB FAT32 card is included.
The SD card panel is a file explorer for the in-memory card: upload (picker or
drag-and-drop, folders included), new folder, rename, delete, download a file,
before Start, while running and after Stop. `fat.js` edits FAT32 in place; exFAT,
FAT12 and FAT16 cards are reported as unsupported (the emulator still boots them).
A change while the emulator runs pauses the guest, edits `/sd.img` in the runtime
iframe and resets the device so the firmware mounts the modified card again.
`node x4prosim/wasm/web/fat.test.mjs` checks `fat.js` against dosfstools and mtools.

Node mounts host files under `/host`. `run-node.sh` copies flash into `.run`
but writes the supplied SD directly, so use a copy when testing. Ctrl-a c
selects the HMP monitor. Smoke tests copy both input images and save logs,
measurements and PPM panel captures under `build-wasm/evidence/smoke`:

```sh
x4prosim/wasm/smoke.sh flash.bin sd.img  # X3 or X4 Pro Home, JIT and panel
x4prosim/wasm/smoke.sh --rom-only       # no firmware; used in CI
x4prosim/wasm/package-web.sh           # package existing default-mode binaries
```

Use `WASM64_MODE=64` with smoke/packaging for the native Memory64 build.
`WASM_JIT_STATS=1` on the Node launcher logs successfully instantiated TB
modules. `REFERENCE_PPM=/absolute/path/to/native.ppm` makes smoke compare the
firmware panel capture byte for byte. X4 Pro firmware is selected by chip ID 9
in the bootloader/app header; it requires three `8179_DRF` refreshes and the
following `[MEM]` checkpoint. X3 firmware also runs the existing X4 Pro
blank-flash ROM check. Browser automation and controls are documented in
[x4prosim/wasm/web/README.txt](x4prosim/wasm/web/README.txt).

The launchers use ordinary TCG blocks with a 64 MiB translation cache and
`-icount shift=0,sleep=on` for X3 (`shift=2` for X4 Pro). The Wasm backend
learns recurring MMIO boundaries to avoid repeated mid-block exits. Native
Wasm longjmp and fixed 256 MiB linear memory reduce runtime overhead.
For larger workloads, rebuild with `WASM_INITIAL_MEMORY=<bytes>` or
`WASM_MEMORY_GROWTH=1`; growth can cost performance. `WASM_ACCEL` overrides
the Node accelerator setting; `RUN_NODE_SLEEP=off` disables idle-time warping
for deterministic timing comparisons in accurate mode (`sleep=on` otherwise).

**The browser page defaults to turbo**: the **Turbo (fast, not
timing-accurate)** checkbox starts ticked; untick it or use `?turbo=0` for
accurate timing (`?turbo=1` remains accepted). **The Node launcher defaults to
accurate timing** (`run-node.sh` behaves as `TURBO=0`); opt in with
`TURBO=1 x4prosim/wasm/run-node.sh flash.bin sd.img`. The native launcher also defaults to accurate timing;
`X4TURBO=1 x4prosim/run.sh flash.bin sd.img` opts
in. Turbo retains `-icount` with `sleep=off`, minimizes SD and panel delays,
and opts into GPSPI `zero-wire-time` synchronous transfers; X3 BUSY pulses
retain a 1 ms minimum. Removing `icount` made transfer timers depend on host
scheduling, slowing native boot and stalling Node. Turbo is faster but
**changes guest timing and must not be used for timing
measurements**. `smoke.sh` runs accurate mode (it passes `TURBO=0`);
`smoke.sh --turbo flash.bin sd.img` compares the panel against a fresh native
turbo reference without requiring timestamp equality.

### CI and hosting

The separate `wasm` job leaves the native build matrix unchanged. It runs
`build-deps.sh`, `build.sh --web` (which calls `package-web.sh`) and
`smoke.sh --rom-only`, then uploads `x4prosim-<version>-wasm`. The wasm64
sysroot/compiler cache key includes the SDK version and dependency recipes.
The artifact contains both emulators, ROMs, browser assets, boot assets from
[x4prosim/boot/](x4prosim/boot/README.md) and the blank card;
it contains no firmware apps or user SD images.

Pages uploads and deploys only on **pushes to `serialx/wasm-jit` in
`serialx/x4prosim`**, to <https://serialx.github.io/x4prosim/>.
`WASM_PAGES_BRANCH` in the workflow is the single branch setting (change it to
`x4prosim` once the branch is merged); other branches, PRs and forks do not
deploy. The repository has **Settings > Pages > Source: GitHub Actions** enabled
and the branch allowed in the `github-pages` environment. The hosted page ships
no firmware: choose a flash image with the file picker.

`serve.py` sets COOP/COEP headers for SharedArrayBuffer and pthreads. On Pages,
the bundled MIT-licensed `coi-serviceworker` supplies isolation and reloads
once on first visit. Use HTTPS or localhost; service workers must be allowed
when the server cannot set those headers. Local header-less hosting passed
with this service worker. The package makes no runtime CDN requests.

### Measurements and limits

CrossPoint 1.6.5 X3 on an Apple M5 Max, Emscripten 6.0.12, Node 26 and
Chrome 155, using `MEMORY64=2` and `sleep=off`. Five quiet alternating pairs
per runtime/mode compare untouched `5ea1b8ef2e` with the six-commit stack
ending at `02ad73311d` (2026-10-10). Runs use fresh image copies, Wi-Fi off,
and no builds or profilers; Chrome uses normal GPU/WebGL and autosave.

Home is the third completed `X3_DRF`; post-Home runs from Home to the first
`[MEM]`, the settled checkpoint. Node Home and settled start at process launch.
Chrome Home starts at image loading, while settled starts at navigation.
Each phase is a separate median, so Home plus post-Home need not equal settled.
Reduction is `100 × (1 − median(stack/base))` across paired post-Home times.

| Runtime / mode | Baseline Home / post-Home / settled | Combined stack Home / post-Home / settled | Post-Home reduction |
| --- | ---: | ---: | ---: |
| Node accurate | 2.074 / 7.330 / 9.422 s | 1.712 / 5.803 / 7.518 s | 20.8% |
| Chrome accurate | 2.671 / 8.979 / 12.181 s | 2.185 / 6.251 / 8.961 s | 30.2% |
| Node turbo | 1.201 / 2.814 / 4.022 s | 1.194 / 2.605 / 3.800 s | 7.9% |
| Chrome turbo | 1.547 / 3.005 / 5.078 s | 1.521 / 2.813 / 4.843 s | 6.5% |

Chrome accurate now settles before the ten-second autosave; each baseline run
incurred one save pause (60–63 ms) before settling. Its improvement includes
avoiding that pause; no save-disabled or matched-save-phase comparison was run.
Native performance was not measured in this series.

X3 guest-visible first-eight waits, full first `[MEM]` line (accurate: 10049 ms)
and whole SD contents are unchanged within each mode. Native/Node comparisons
are exact, as are Chrome completed-refresh panels and resumed waits/MEM/SD.
Panel ink drift advances in fixed virtual-time steps, so a capture at a
given virtual time is identical regardless of how often the host redraws (it
used to differ by one grayscale level after `[MEM]`). Turbo separately changes
guest timing.

See [what makes it fast and rejected experiments](x4prosim/wasm/README.md#what-makes-it-fast)
for the changes behind this comparison. These X3 timings make no X4 speed claim.

The following measurements are **from the earlier round-2 integration**, before both the
three-change stack and the six changes above. They are
three-run medians except the older TCI results, which were not rerun and compare
different QEMU versions. The `sleep=on` rows describe the former stack, not the
current default's performance.

| Runtime / mode | Home | Home → settled | Settled total |
| --- | ---: | ---: | ---: |
| Native, accurate `sleep=off` | 1.785 s | 8.789 s | 10.574 s |
| Native, accurate `sleep=on` | 4.124 s | 11.695 s | 15.819 s |
| Node, old TCI port, `sleep=on` (historical) | 16.209 s | — | 108.470 s |
| Node JIT, accurate `sleep=off` | 2.394 s | 10.064 s | 12.464 s |
| Node JIT, accurate `sleep=on` (`TURBO=0`) | 4.635 s | 13.073 s | 17.708 s |
| Chrome, old TCI port (historical) | 19.79 s | — | 145.46 s |
| Chrome JIT, accurate `sleep=off` | 3.069 s | 11.656 s | 14.758 s |
| Chrome JIT, accurate `sleep=on` (`?turbo=0`) | 5.264 s | 14.091 s | 19.355 s |
| Node JIT, turbo (opt-in) | 1.097 s | 3.706 s | 4.789 s |
| Chrome JIT, turbo (opt-in) | 1.636 s | 3.917 s | 5.558 s |

Accurate `sleep=on` can vary with host scheduling, including natively. Both
address modes build and pass smoke; native Memory64 was not rebenchmarked in
the combined series. Earlier measurements found higher Node RSS with the JIT
(835 MiB versus TCI 541 MiB); RSS was not remeasured as a controlled comparison.
Earlier experiments and rejected candidates are in
[the performance report](x4prosim/wasm/perf/turbo-profile.md).

### What to upstream

Keep these as separate reviewable changes, with their correctness and A/B evidence:

- Reload the current CPU context on every shared Wasm TB entry for SMP guests
  (`tcg/wasm64/tcg-target.c.inc`).
- Round UC8179 animation deadlines upward and keep pending timers in the future
  (`hw/display/uc8179.c`).
- Use SDL renderer logical coordinates without scaling pointer input twice
  (`ui/sdl2.c`).
- Empty auxiliary timer-list wakeup guard (`util/qemu-timer.c`).
- Direct dispatch for simple subpage MMIO (`system/physmem.c`, `accel/tcg/cputlb.c`).
- Synchronous SD media checks for ordinary images (`block/block-backend.c`, `hw/sd/sd.c`).
- Learned MMIO block boundaries (`accel/tcg/translate-all.c`).
- Exact SHA callback signatures and the intmatrix `BIT_ULL` fix in the ESP32 models.
- SDL browser input polling that drains the proxy queue without yielding under BQL
  (`ui/sdl2.c`), for the Emscripten port.

The combined stack retains amortized BQL locking with native non-regression
confirmed in the paired runs above. Turbo changes timing and remains separate from the
accurate-mode optimizations. Safari and Firefox remain untested; the later
official X4 Pro release verification is recorded below.

Wasm Wi-Fi downloads use an in-browser HTTP/TLS adapter and static font assets,
with no relay or native QEMU dependency; see
[Wi-Fi and font downloads](x4prosim/wasm/README.md#wi-fi-and-font-downloads).
Browser images consume their full logical size:
a sparse 1 GiB SD still takes 1 GiB, with extra copies for loading and export.
Start with the packaged 64 MiB card when possible. The browser saves the SD
card in IndexedDB and restores it on the next visit. X4 Pro acceptance now
includes the official 1.6.5 firmware
as well as blank-flash ROM boot. These tests do not establish coverage for
every firmware or all self-modifying-code/remapping cases.

## Official CrossPoint X4 Pro verification (2026-10-10)

The unmodified **1.6.5 X4 Pro release** composed with `mkflash.py` boots to Home
natively, including a blank 64 MiB SD first run. `drive.py` verified Down/Up
selection, GT911 taps into Library and Settings, and the capacitive Home key.
The same screens work with `testdata/sdcard` copied onto the card; that fixture
contains battery history but no books. Power draws the sleep screen, but a
second Power press does not resume; S3 deep sleep/ext0/ext1 wake remains open.

The same composed image reaches Home and the following `[MEM]` checkpoint
under Node in default accurate mode and `TURBO=1`, and in Chrome with either mode.
The browser smoke verifies Down/Up, a pointer tap that opens Library, and Stop.
The turbo Home PPM matches the native capture byte for byte. Before the
2026-10-10 stack, with the tested blank card, Home took about 2.5 seconds in
Node/browser turbo and 7.8–8.0 seconds
in accurate mode on the M5 Max; these are individual acceptance runs.

Three emulator fixes were needed: shared Wasm translation blocks now reload
the current CPU context on entry; UC8179 rounds animation deadlines upward
so turbo cannot rearm an already expired timer; SDL 2D pointer coordinates
use the renderer's logical size rather than being scaled a second time.
Existing Wasm binaries must be rebuilt to include these fixes.

Touch scripting uses the existing monitor interface:

```sh
python3 x4prosim/drive.py x4pro.bin sd.img native.log wait:25 \
  press:down wait:2 press:up wait:2 \
  'hmp:qom-set /machine/gt911 tap 240,420' wait:4 shot:library.png \
  'hmp:qom-set /machine/gt911 tap home' wait:3 \
  'hmp:qom-set /machine/gt911 tap 240,560' wait:4 shot:settings.png
```

The coordinates select Library and Settings on the blank-card Home layout.
`tap home` activates the capacitive Home key. A held contact can instead use
`qom-set /machine/gt911 touch X,Y`, followed by `touch up`.

The X4 Pro **has no IMU in the supported board profile** (`imuAddr=0`,
`ImuType::None`), consistent with the supplied real-device boot log's
`[GYR] No IMU on this board`. Release 1.6.5 logs `SDK IMU not found` for the
same absent capability; it is not a missing emulator peripheral. Tilt page
turns are unavailable on this board, and the IMU wrapper returns before
sensor wake/sleep or polling. It does not provide the physical wake source.
I2C0 models GT911 (0x5D), BM8563 (0x51) and CW2017 (0x63), matching the profile.

The `Wire.cpp` lock/NULL TX warnings in the X3 log come from release 1.6.5's
raw BQ27220 current reads in `HalGPIO::getWakeupReason()` before `Wire.begin()`.
The calls fail on uninitialized Wire state before reaching the I2C controller;
later SDK sensor initialization brings the bus up. They are firmware startup
ordering diagnostics, not missing-device NACKs. These X4 Pro runs produce no
Wire lock/NULL TX warnings. No firmware or peripheral model was changed to
suppress diagnostics.

## Where it stands (2026-10-08, 1007e firmware)

Machine `-machine x4pro` (in `hw/xtensa/esp32s3.c`: `x4pro_board_init`) boots the
unmodified `x4-pro-debug` image to the Home screen, takes key presses and renders
the panel.

Done on `x4prosim`:
- Octal PSRAM 8 MB (default on `x4pro`), plus a PSRAM bounds fix.
- `hw/misc/esp32s3_usb_jtag.c`: USB-CDC console (firmware log) to a chardev.
- `hw/misc/esp32s3_sens.c`: SAR ADC oneshot always "done" (IDF calibration at boot).
- `hw/gpio/esp32s3_gpio.c`: real GPIO model: OUT/ENABLE/IN, edge/level
  interrupts, named lines `pin-in` (drive a pin from outside), `pin-out` (pin level
  to a device), `wake` (light-sleep GPIO wakeup). Undriven inputs read 1 (pull-up).
- `hw/ssi/esp32s3_gpspi.c`: GPSPI2 CPU-mode master (W0..W15 buffer, USR, TRANS_DONE).
- `hw/display/uc8179.c`: the X4 Pro's UC8179 panel on GPSPI2 with CS/DC/RST and
  BUSY_N on GPIO, plus the bit-banged SDA reads the firmware uses for its panel
  probe and OTP. Answers like the real panel (VER, FLG, OTP), so the firmware logs
  `controller=UC8179 ... promoted=1`. Graphic console shown portrait like the
  device (`portrait=false` for raw). `hw/display/ssd1677.c` is an unused SSD1677
  model kept for reference.
- UC8179 ink and ghosting (helper agent; full notes, data and tests in
  `x4prosim/ghosting/README.md`): each refresh (0x12) is simulated frame by frame
  instead of showing the planes, and plays on screen one frame per `frame-us`
  of virtual time (full refreshes flash, fast ones paint in). BUSY lasts the
  frames' length: boot full 1500 ms (device 1493), 50-frame direct gray 1250 ms
  (device 1189), DU 24 frames 600 ms (device 662).
  - PSR REG picks the waveform per refresh: REG=1 runs the uploaded register LUTs
    (0x20 VCOM, 0x21 WW, 0x22 KW, 0x23 WK, 0x24 KK); REG=0 runs stand-ins for the
    OTP waveforms (bodies never dumped): fast inside PTIN/PTOUT, full otherwise.
    CDI N2OCP copies NEW to OLD after the refresh.
  - Per pixel: saturating particle motion, two-component remnant voltage,
    blooming (edge loss), drift toward gray, shown linear in L*. Defaults are
    fitted to device photos: a typed-then-deleted word leaves ~11% ghost (device
    ~11%) that fades x0.53 over 6 fast refreshes (device x0.51); menus and gray
    book pages come out clean.
  - Every parameter is `-global uc8179.<name>=N` and 0 turns that mechanism off:
    `swing-frames` 6, `rail-soft` 100, `otp-fast-frames` 10,
    `otp-hold-drive` 6, `remnant-fast`/`-ms` 30/1000, `remnant-slow`/`-ms`
    10/30000, `bloom` 35, `drift`/`drift-s` 50/1800 (uncalibrated), `frame-us`
    25000. `otp-full-frames` is 30 (not 0 = off). Units and what each was fitted
    to: the notes, 3.4.
  - `busy-ms` 0 derives BUSY from the frames; nonzero fixes it. `animate=false`
    applies all frames at the DRF (BUSY still lasts the frames), for fast
    scripted runs. To capture the animation with `drive.py` (a screendump costs
    ~0.3 s), slow it: `-global uc8179.frame-us=100000`. A DRF during playback
    finishes the previous refresh first; RST stops it where it is.
  - Ink state is 16 B/pixel (6 MB) and the frame loop runs on QEMU's main loop.
  - Measure: `test/*-steps.txt` replay a scenario with `drive.py`,
    `tools/pepmeasure.py` / `seqmeasure.py` / `hist.py` read the screenshots,
    `tools/photomeasure.py` the device photos.
- `hw/display/esp_rgb.c`: the RGB console is created at realize, so `x4pro`
  (which never realizes it) no longer opens a stray black 800x600 SDL window.
- `hw/misc/esp32s3_sens.c`: temperature sensor always ready, 25 C (Goodies >
  Battery & Stats hung a core on it). `run.sh` shows the host cursor in SDL.
- X4 Pro uses `-icount shift=2` (set by `run.sh`/`drive.py`): guest time follows
  instructions, not host speed; without it FreeRTOS can assert after light sleep on a slow host.
  Guest runs slower than real time, so give `wait:` steps generous values.
- `x4prosim/testdata/`: real device logs, the panel OTP dump, battery history to
  seed the SD card, estimated power numbers. See its README.
- `hw/input/x4pro_keys.c`: Up/Down/Power keys (arrow keys + P, or
  `qom-set /machine/x4pro-keys down true`).
- `hw/misc/esp32s3_rtc_cntl.c`: light sleep: `SLEEP_EN` waits for the RTC timer
  alarm or a GPIO wake, and hides the sleep from the RTC counter so esp_timer
  isn't advanced twice.
- SD card: `x4prosim/mksd.py` makes an MBR + FAT32 image (SdFat needs the MBR);
  the existing `dwc_sdmmc` model mounts it.
- `x4prosim/drive.py`: headless runner with steps (`wait:S`, `press:down[:ms]`,
  `shot:file.png`, `hmp:cmd`). Example:
  `x4prosim/drive.py flash.bin sd.img log.txt wait:30 press:down shot:home.png`

- Wi-Fi: the MAC is emulated with a fake AP bridged to a QEMU NIC (port of the
  lcgamboa ESP32-C3 model: `hw/misc/esp32s3_wifi.c`, `esp32_wifi_ap.c`,
  `esp32_wlan_packet.c`; analog/FE in `esp32s3_ana.c`, `esp32_fe.c`). `run.sh`
  joins it to a network:
  - `X4BR=br0 x4prosim/run.sh ...` bridges to your LAN: the firmware does its own
    DHCP with your router and is reachable at that IP like a real device. Needs a
    host bridge `br0` holding your Ethernet port, `allow br0` in
    `/usr/local/etc/qemu/bridge.conf`, and root (the helper is `build/qemu-bridge-helper`).
    Wi-Fi host NICs can't be bridged; use Ethernet.
  - Default: QEMU NAT (`-nic user,model=esp32_wifi,hostfwd=tcp::8080-:80`), DHCP
    10.0.2.15, host port 8080 reaches the device's port 80 (`curl localhost:8080`).
  - `X4NET="-nic ..."` overrides both; `drive.py` takes the same `-nic` in `QEMU_EXTRA`.
  A scan finds `PICSimLabWifi` (open, ch 1, the one that connects), `Espressif` and
  `MasseyWifi`. With no `model=esp32_wifi` NIC the MAC is a stub and a scan finds 0.
  Station MAC: the NIC's `mac=` (default 52:54:00:12:34:56) is burned into efuse
  when no efuse file is given; give each sim on one LAN its own `mac=`.

Still missing: LEDC frontlight, charger STAT (GPIO21 reads 1 = charging), real Wi-Fi
(WPA, signal, other APs),
deep sleep with ext0/ext1 wake, USB OTG.

Gaps another firmware is likely to hit (CrossDink doesn't need them yet):
- GPSPI: DMA transfers (IDF `spi_master` uses DMA above 64 bytes), hardware CS,
  command/address/dummy phases, and half-duplex reads on SDA.
- GPIO matrix / IO_MUX routing: pins are wired directly, so a peripheral routed to
  a different pin than the X4 Pro's won't reach the device.
- UC8179: the 0x90 partial window, LUTBD (0x25), VDHR, VCOM_DC value and
  temperature (TSSET) aren't modeled; the OTP waveforms are stand-ins until the
  full 0xA2 OTP is dumped.
  Open calibration items: `x4prosim/ghosting/README.md` section 8.
- Flash encryption and secure boot.

## Xteink X3 (`-machine x3`, ESP32-C3)

The X3 is a different SoC: ESP32-C3 (RISC-V, 400 KB SRAM, no PSRAM), so it is a
second machine in `hw/riscv/esp32c3.c` (`x3_board_init`), built as
`qemu-system-riscv32`. `run.sh`, `drive.py` and `mkflash.sh` pick the machine
from the image's chip (byte 12 of the bootloader header), `X4MACHINE=x3|x4pro`
overrides. It runs the unmodified CrossPoint `crosspoint-<ver>-x3-x4.bin` (the dual
X3/X4 C3 binary) to Home, Library and Settings, with key presses, the battery from
the gauge and the SD card readable and writable. Hardware (freeink-sdk
`BoardConfig.h` `XTEINK_X3` / `XTEINK_X3_UC8279`):

| Area | Hardware | Pins / bus | Model |
| --- | --- | --- | --- |
| SoC blocks | the S3 models fit the C3's register maps: GPIO (`esp32s3_gpio.c`), GPSPI2 (`esp32s3_gpspi.c`), I2C0 (`esp32s3_i2c.c`), USB-CDC console (`esp32s3_usb_jtag.c`) | | wired for every C3 machine |
| Display | UC8279d 792x528 (newer units) or UC8253 (older), SPI 10 MHz, VER probed bit-banged | SCLK 8, SDA 10, CS 21, DC 4, RST 5, BUSY_N 6 | `hw/display/uc8279.c`; X3 defaults to UC8253 (VER floats); `-global uc8279.uc8253=false` selects UC8279d |
| SD | SPI mode on the same bus, SdFat; GPIO13 = rail enable | MISO 7, CS 12 | QEMU `ssi-sd` + `sd-card-spi`, `-drive if=sd` |
| Keys | ADC ladder: Back/Confirm/Left/Right on GPIO1 (ADC1 ch1), Up/Down on GPIO2 (ch2), raw 3512/2694/1493/5 and 2242/5, idle 4095; Power GPIO3 active-LOW | SAR ADC | `hw/misc/esp32c3_saradc.c` (one-shot), `hw/input/x3_keys.c`: arrows, Enter, Backspace/Esc, P; `qom-set /machine/x3-keys confirm true` |
| Fuel gauge | BQ27220 at 0x55: Voltage, Current, SOC, DesignCapacity, CFGUPDATE flow | I2C0 SDA 20, SCL 0 | `hw/misc/bq27220.c`: `-global bq27220.soc=80,voltage-mv=3900,current-ma=-80` (Current > 0 = USB power: the firmware goes back to sleep at boot) |
| RTC | DS3231 at 0x68 | I2C0 | `hw/rtc/ds3231.c`, host clock (UTC) |
| IMU | QMI8658 at 0x6B (WHO_AM_I 0x05) | I2C0 | `hw/misc/qmi8658.c`, lying flat, at rest |
| Sleep | no light sleep in CrossPoint; deep sleep with GPIO3 wake, RTC timer | RTC_CNTL | `esp32c3_rtc_cntl.c`: RTC timer, light sleep (timer/GPIO), deep sleep = reset with reason DEEPSLEEP and the wake cause |
| Wi-Fi | MAC + fake open AP bridged to `-nic user` | n/a | the S3 models at the same bases (`esp32s3_wifi.c`, `esp32s3_ana.c`, `esp32_fe.c`) |

Notes:
- X3 CPU timing uses `-icount shift=0,sleep=on` (selected by `run.sh` and
  `drive.py`), so each tick is 1 ns. CPU properties on `espressif-riscv-cpu`
  are `cost-rom-ns=6`, `cost-sram-ns=8`, `cost-flash-ns=10`, and additive
  `cost-load=0`, `cost-store=3`, `cost-mul=0`, `cost-div=160`, `cost-branch=2`.
  The clock registers select a factor relative to 160 MHz; 10 MHz costs 16x.
  Override with `-global espressif-riscv-cpu.cost-flash-ns=9`, for example.
  These are effective calibrated costs, not a cache or pipeline simulation.
  See [CPU and SD refit](x4prosim/sdcal/cpu.md) for probe residuals, page
  rendering measurements, clock validation, and the current 30-row SD table.
- GPSPI2 completes CPU transfers on a virtual-clock timer. `SPI_CLOCK` selects
  an 80 MHz APB clock divided by `(clkdiv_pre + 1) * (clkcnt_n + 1)`, or 80 MHz
  directly with `clk_equ_sysclk`. The rate is sampled for each transaction, so
  the SD card's 40 MHz and the panel's 10 MHz both consume their wire time.
  `CMD.USR` stays set until the bytes reach the slave and `TRANS_DONE` is raised.
  All GPSPI setup properties are uint32 and default to zero on other boards.
  The X3 adds 700 ns per transaction (`transaction-overhead-ns`) and 300 ns
  for transfers longer than one byte (`buffer-overhead-ns`). These effective
  setup costs were re-fitted with the calibrated CPU driver time: commands
  take about 22 µs and read transfers about 147 µs per sector. The older
  `transaction-overhead-us` property remains additive. Setting either base
  overhead property explicitly suppresses the X3's default base overhead.
  For example, `-global driver=ssi.esp32s3.gpspi,property=transaction-overhead-us,value=0`
  clears the base cost; also set `buffer-overhead-ns=0` with the same explicit
  syntax to remove the buffered cost. The explicit form is needed because the
  device type contains dots. Both SD and panel transactions use these costs.
- SD timing uses virtual-clock deadlines, including under
  `-icount shift=0,sleep=on`. Polling reads return `0xff` until a data token is
  ready; polling writes return busy (`0x00`) after the data response token.
  The following `ssi-sd` properties are in microseconds (uint32 except the
  signed `write-stop-decrement-us`). Their generic defaults are zero; the X3 values are calibrated against three boots of the
  supplied card at 40 MHz, using the unchanged SdFat SHARED-mode probe.
  See [the calibration notes](x4prosim/sdcal/cpu.md) for all 30 rows,
  residual errors, raw emulator output and reproduction steps.

  | Property | X3 default (µs) | Interval |
  | --- | ---: | --- |
  | `read-access-us` | 207 | R1 to the first CMD17/CMD18 token for a random start |
  | `read-seq-access-us` | 136 | Same interval immediately after the last completed read block |
  | `read-repeat-us` | 108 | Same interval when repeating the previous read command's start |
  | `read-next-us` | 9 | End of one CMD18 block's CRC to the next token |
  | `write-busy-us` | 554 | CMD24 busy after a sequential write's data response |
  | `write-random-busy-us` | 621 | CMD24 busy for a random start |
  | `write-repeat-busy-us` | 479 | CMD24 busy for a repeated start |
  | `write-block-busy-us` | 10 | Busy after every CMD25 block's data response |
  | `write-stop-busy-us` | 480 | Base busy time after CMD25 STOP_TRAN |
  | `write-stop-decrement-us` | -40 | Signed reduction per CMD25 block; negative adds busy time |
  | `write-stop-random-extra-us` | 100 | Extra stop busy for a random command start |

  STOP_TRAN busy is `max(0, base + random_extra - decrement * blocks)`, clamped
  to UINT32_MAX microseconds. SdFat does not wait after STOP_TRAN: any remaining
  busy time appears in the next command's pre-wait. The firmware also spends
  time preparing that operation, so the measured command residual is shorter
  than the card's deadline. This deterministic model fits median totals; it does
  not reproduce the card's long write stalls or every phase distribution.
  `write-busy-us` and `write-random-busy-us` now apply only to CMD24;
  `write-block-busy-us` independently controls CMD25.

  Override any value with, for example,
  `-global ssi-sd.read-access-us=200 -global ssi-sd.write-block-busy-us=15`.
  Read and write histories are independent. Repeated start is checked before
  sequential start; the first access is random. No R1 response delay is added.
  Trace decisions and bus transactions with
  `-d 'trace:ssi_sd_*,trace:esp32s3_gpspi_*' -D spi-timing.log`, or use
  `--trace 'enable=ssi_sd_*' --trace 'enable=esp32s3_gpspi_*'`.
  Setting all card latency properties to zero disables only card delays;
  transfers still consume their wire time and configured GPSPI setup time.
- The panel uses the UC8179's full ink model and default strengths: per-frame
  saturation, fast/slow remnant charge, blooming, drift, L* grayscale mapping,
  OTP waveform approximations, and animation. X3's six- and seven-byte LUTs
  are decoded in phase/repeat order and include VCOM drive. The former
  `dead-frames` approximation is removed. CDI N2OCP copies NEW to OLD after
  latching a refresh. The partial window (0x90) and TRES constrain RAM writes.
  BUSY uses the longest of VCOM and the four
  transition rows, with a fixed refresh overhead. X3 UC8253 defaults are
  `frame-us=12850`, `refresh-overhead-us=138000`, `pon-ms=127`, `pof-ms=2`:
  fast 382 ms, grayscale pre-BW 485 ms, gray 228 ms, full 935 ms. Every setting
  accepts a `-global uc8279.<property>=<value>` override; `busy-ms` fixes DRF
  duration when nonzero. UC8279d uses 25000/0/2/2 defaults; its generic frame
  period now matches the UC8179. The UC8253 machine timing remains calibrated.
  `animate=false` applies the same ink simulation immediately. `frame-us=0`
  also renders immediately for X3 turbo mode, using a 1 us ink step.
  The copied ghosting strengths have not been fitted to X3 device photos.
  PLL (0x30) scales the frame period relative to the driver's init value
  (0x09 UC8253, 0x0f UC8279d); its family-table mapping remains provisional.
  Trace per-row totals with `-d trace:uc8279_refresh -D panel.trace`.
  See [panel calibration](x4prosim/sdcal/panel.md) for the fit, measured waits,
  LUT repeat semantics, and limitations.
- A cold boot needs the power button held (the firmware re-sleeps otherwise):
  `x3-keys.power-boot-ms` (default 1500) holds it from reset.
- CrossPoint up to 1.6.5 deadlocks on a first boot whose NVS has no cached device
  type: the X3 fingerprint probe runs `Wire.begin/end`, then `getWakeupReason`
  reads the gauge through raw `Wire` calls with the bus down, and the failed
  `endTransmission` keeps the Wire lock, so the IMU init blocks forever. A device
  that has booted once has `cphw/dev_det` in NVS and skips the probe, which is why
  nobody sees it. `mkflash.py` seeds exactly that key (`mknvs.py`) on C3 images.
- `mksd.py` needs `mkfs.vfat` and `mcopy` (`brew install dosfstools mtools`; the
  build needs `brew install libslirp` too).
- Serial control (CrossPoint's `scripts/debugging_monitor.py` / `device_control.py`
  from the serial-device-control branch): give the console a pty instead of stdio,
  `-chardev pty,id=cdc` (QEMU prints the `/dev/ttysN` it made on stdout), and start
  the monitor on that path: `debugging_monitor.py --serve --headless /dev/ttysN`.
  PRESS/STATE/CANCEL/SCREENSHOT work as on the device; a 52 KB screenshot takes ~0.5 s.
  Verified 2026-10-09 with the X3/X4 develop build plus that branch (28-step scenario:
  Home, Browse Files, reader page turns, reader menu, hold/cancel, Settings, Library).
- `hw/sd/ssi-sd.c` fixes for SdFat: R3/R7 carry the real idle bit (CMD58 after
  ACMD41 is 0x00), a write's data token right after R1 is taken (no fill byte),
  CMD13 answers an SPI R2.
- Wi-Fi: the S3's radio models (regstub, ana, fe, Wi-Fi MAC with the fake AP) are
  wired for every C3 machine too; `run.sh` adds the NIC like on the X4 Pro. SYSCON's
  first 0x20 bytes are a register stub so the PHY's clock-enable assert passes
  (Settings > Manage Fonts starts Wi-Fi and crashed on it).
- Not modeled: the frontlight (the X3 has none), USB MSC, flash encryption.

## Hardware to model (X4 Pro pin map, from freeink-sdk BoardConfig.h `XTEINK_X4_PRO`)

| Area | Hardware | Pins / bus | Owner |
| --- | --- | --- | --- |
| Display | UC8179 800x480 1-bit e-ink over GPSPI2, 10 MHz; SDA also read bit-banged | SCLK 12, SDA 11, CS 13, DC 18, RST 14, BUSY_N 6 | done |
| SD | SDMMC slot 1, 1-bit, 40 MHz; GPIO5 = power enable, active-LOW | CLK 41, CMD 42, D0 40 | done |
| Buttons | active-LOW, pull-up; Up 0 (strap), Down 7, Power 3 | GPIO | done |
| **I2C bus** | **ESP32-S3 I2C0 controller, 400 kHz** | **SDA 39, SCL 38** | done |
| Touch | **GT911** at 0x5D (alt 0x14), INT 10, RST 4, power-enable GPIO2 active-LOW; reports X 0..480, Y 0..800 (portrait, firmware swaps XY and flips Y); has a capacitive Home key | on I2C | done |
| RTC | **BM8563** (PCF8563-compatible) at 0x51 | on I2C | done |
| Fuel gauge | **CW2017** at 0x63 | on I2C | done |
| Frontlight | LEDC PWM 25 kHz 10-bit, cool GPIO8 (ch4), warm GPIO9 (ch5) | LEDC | open |
| Charger | STAT GPIO21, active-HIGH = charging | GPIO | open |
| Wi-Fi | MAC + fake open AP bridged to a QEMU NIC: NAT (`-nic user`, hostfwd) or LAN (`X4BR=br0`) | n/a | done |
| Sleep | deep sleep + ext0/ext1 GPIO wake, RTC_NOINIT/RTC_DATA memory | RTC_CNTL | open (later) |

## Helper agent task: GPSPI2 DMA + hardware CS (current)

Goal: firmware that drives the panel with ESP-IDF `spi_master` or Arduino `SPI`
using DMA and the controller's own CS works like CrossDink's CPU-mode SPI does
today. Branch `ext/spi-dma` off `x4prosim`, PR into `x4prosim`. You own
`hw/ssi/esp32s3_gpspi.c` for this task (and its wiring in `x4pro_board_init`).

Starting point: branch **`wip/spi-dma`** (598cb63) has an untested draft. It
compiles, but nothing has exercised it: GDMA out/in through the `esp_gdma`
API (`esp_gdma_get_channel_periph(GDMA_SPI2)`, read/write channel), command,
address and dummy phases, MOSI-only/MISO-only/full-duplex, a `cs0` output
(CS0_DIS, CS_KEEP_ACTIVE), and the panel CS = GPIO13 AND SPI CS0. Review it
against the S3 TRM and IDF `hal/esp32s3/include/hal/spi_ll.h` and fix what's wrong.

1. **DMA**: `spi_master` uses GDMA above 64 bytes (`SPI_DMA_CONF` TX/RX enable,
   GDMA PERI_SEL = 0 for SPI2). Check `esp_gdma_get_channel_periph`: it also
   matches any started channel, so a second DMA user (AES/SHA) could be picked.
   GDMA here only reaches internal DRAM; PSRAM buffers (EDMA) are a known gap:
   either add PSRAM to the GDMA address space or log a guest error.
2. **Interrupts and status**: TRANS_DONE plus whatever `spi_master`'s ISR and
   polling path read (check `spi_ll_usr_is_done`, `SPI_DMA_INT_*`, CMD.UPDATE,
   CMD.USR clearing). Transactions that queue back-to-back must work.
3. **Hardware CS**: CS0 goes low per transaction unless disabled or kept
   active. The GPIO matrix isn't modeled; CS0 is wired to the panel as if routed
   to GPIO13.
4. **Test firmware**: a minimal ESP-IDF (or Arduino) app in
   `x4prosim/tests/spi-dma/` that inits the X4 Pro panel pins
   (SCLK 12, MOSI 11, CS 13 as hardware CS, DC 18, RST 14, BUSY_N 6), sends
   UC8179 init + a 48000-byte DTM2 plane with one DMA transaction, refreshes
   (0x12), and prints "done". Commit its source and a build script, not the
   binary. Done when: `drive.py` screenshot shows the test pattern, and
   CrossDink 1007e still boots to Home with UC8179 promoted and a key press
   moving the selection (no regressions).

## Done helper task (merged): I2C bus + GT911 + BM8563 + CW2017

1. **ESP32-S3 I2C controller** `hw/i2c/esp32s3_i2c.c`: start from
   `hw/i2c/esp32_i2c.c` (ESP32 classic) and adapt it to the S3 register map (IDF
   `components/soc/esp32s3/register/soc/i2c_reg.h`, `hal/esp32s3/include/hal/i2c_ll.h`).
   Notable S3 changes: command registers at a new offset, the `CTR.CONF_UPGATE` bit
   that latches config, FIFO via `I2C_DATA_REG`, and different interrupt bits.
   Arduino 3.3.9 uses the IDF **i2c_master (ng) driver**, which is interrupt-driven.
   Wire both I2C0 and I2C1 into `hw/xtensa/esp32s3.c` with their interrupt-matrix
   sources. Done when: no more `ESP_ERR_INVALID_STATE` in the log and an I2C probe of
   a missing address NACKs cleanly.
2. **BM8563** `hw/rtc/bm8563.c` (PCF8563 register set, BCD time from the host clock,
   writes kept). Done when: the firmware's RTC read gives host time.
3. **CW2017** `hw/misc/cw2017.c`: chip ID, VCELL, SOC, and a profile/config
   register set that accepts the driver's init writes. Battery % and voltage as QOM
   properties (`-global`) and settable at runtime via `qom-set`. Read the
   freeink-sdk driver for the exact registers it touches (`rg -n "CW2017|Cw2017"` in
   the CrossDink `freeink-sdk/` submodule). Done when: the status bar shows the
   configured %.
4. **GT911** `hw/input/gt911.c`: product ID "911", config at 0x8047.., status 0x814E,
   points at 0x8150, buffer-status clear on write 0x814E=0, INT pulse on GPIO10: give
   the GT911 a named GPIO out and connect it to the GPIO model's `pin-in` line 10 in
   `x4pro_board_init`. Its power enable is GPIO2 and reset GPIO4: take them from
   `pin-out` lines 2 and 4. Touch input: QEMU mouse events (`qemu_input_handler_register`
   with absolute pointer) mapped to the 480x800 portrait raw space, plus an HMP/QMP-
   friendly QOM property or `-device` option for scripted taps. The Home key: one
   key code in the GT911 key area (check the freeink-sdk GT911 driver for how it reads
   it). Respect power: no ACKs while GPIO2 is HIGH (powered off). Done when: a scripted tap moves
   the firmware's selection (the log prints activity/input lines).

Read the freeink-sdk drivers in the CrossDink checkout before modeling:
`freeink-sdk/libs/hardware/` (touch, gauge, RTC, BoardConfig). Model what the driver
actually uses; don't implement whole datasheets.

Verification for every PR: build clean (no new warnings), boot the 1007e-or-newer
`x4-pro-debug` image with `run.sh` for 60 s, paste the relevant log lines in the PR,
and confirm no new hang (`info registers -a` PCs keep moving).

## Don'ts

- Don't modify any firmware (CrossDink, freeink-sdk or others). Don't change the GPIO, UC8179, keys or
  RTC_CNTL models (owned by the project thread; GPSPI is yours for the SPI DMA task); adding your devices' wiring lines to
  `x4pro_board_init` is fine. Ask in the PR if you need a hook elsewhere.
- Don't push to `x4prosim` directly. PRs only.
- No upstream references in PR titles, bodies or commits (no `#N` pointing at other
  repos, no `owner/repo#N`, no "fixes/closes" keywords).
