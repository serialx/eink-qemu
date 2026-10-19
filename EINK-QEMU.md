# eink-qemu: instructions for helper agents

eink-qemu is based on upstream **QEMU 11.1.2**, with ported Espressif SoC models,
and simulates the **Xteink X4 Pro** (ESP32-S3R8, 16 MB flash, 8 MB octal PSRAM) closely enough to run **any
unmodified X4 Pro firmware**: the stock firmware, CrossInk, CrossDink, or a bare
ESP-IDF/Arduino app. It also simulates the **Xteink X3** (ESP32-C3; see the X3
section) for the CrossPoint X3/X4 binary. The goal is firmware development without the physical device:
the screen, SD card, keys, touch, clock and battery behave like the board.
The tools assume ESP-IDF and nothing else about the firmware; what they know
about particular firmware (CrossPoint's boot milestones, render times and X3
NVS entry, the stock firmware's boot gate) is data in
[eink-qemu/profiles](eink-qemu/profiles/README.md), read by every front end.

Rule zero: **never change a firmware to make it boot in QEMU.** Model the hardware
instead, from the datasheets and the board, not from one firmware's driver: another
firmware may use the same chip differently (hardware SPI CS, DMA, other panel
commands). CrossDink (`prokrypt/CrossDink`, read-only for you) is the reference
firmware for testing because its drivers are readable and it logs a lot.

## Porting layers

See [eink-qemu/porting/README.md](eink-qemu/porting/README.md) for the QEMU 11.1.2
porting branches, layer ownership, build command and commit rules.

## Repo, branches, workflow

- Repo: `serialx/eink-qemu`. Integration branch: **`main`** (base: upstream
  QEMU 11.1.2).
- Upstream project: `prokrypt/x4prosim`.
- Work on your own branch off `main` (name it `ext/<area>`, e.g. `ext/i2c`) and
  open a PR into `main`. Keep each PR to one peripheral.
- Only touch the files your task names plus `hw/xtensa/esp32s3.c` (machine wiring)
  and the matching `meson.build`/`Kconfig`. Ask before editing shared files
  someone else owns (table below).
- Style: QEMU C style, 4-space indent. One-line comment only where the why isn't
  obvious. Each new device: a short header comment naming the registers it models
  and what it fakes.
- Commit messages: `eink-qemu: <what>`.

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

Any app `.bin`, merged image, build directory or flash dump works. An official
CrossPoint release app is an easy start: the `crosspoint-<version>-x3-x4.bin`
asset for X3 (ESP32-C3), or `crosspoint-<version>-x4pro.bin` for X4 Pro
(ESP32-S3):

```sh
python3 eink-qemu/mkflash.py crosspoint-1.6.5-x3-x4.bin flash.bin
python3 eink-qemu/mkflash.py crosspoint-1.6.5-x4pro.bin flash-x4pro.bin
```

This needs only Python 3, detects the chip, adds the bundled bootloader and
Arduino-ESP32's 16 MB partition table (`boot/partitions.csv` is its
`default_16MB.csv`, which CrossPoint uses), and preserves the app bytes. Firmware
with another layout passes its own table: `--partitions partitions.csv` (or the
binary table), read and checked as ESP-IDF's `gen_esp32part.py` does. The app
goes to ESP-IDF's boot-default slot: the factory partition, else the lowest OTA
slot. Bootloader
provenance, licenses and regeneration commands are in
[eink-qemu/boot/](eink-qemu/boot/README.md). CrossPoint X3 images (those that
carry its `cphw`/`dev_det` strings) get its cached device type in NVS; every
other image keeps NVS erased. `--machine x3|x4pro` checks that the image matches
the requested machine.

Other inputs:
- A PlatformIO or ESP-IDF build: `python3 eink-qemu/mkflash.py <build dir> flash.bin`
  merges its `bootloader.bin`, `partition*.bin` and app (`firmware.bin`, or the
  only other `.bin`) at 0x0, 0x8000 and the app's partition. Nested IDF
  boot/table directories work.
- A complete 16 MiB flash image passes through unchanged:
  `python3 eink-qemu/mkflash.py dump.bin flash.bin`.
- A real-device dump: `esptool.py --chip esp32s3 read_flash 0 0x1000000 flash.bin`
  (works only if the device does not use flash encryption).

`mkflash.sh` remains a compatibility wrapper. `run.sh` and `drive.py` select
X3 or X4 Pro from the resulting bootloader header.

CrossDink example: `pio run -e x4-pro-debug` in a CrossDink checkout, then:

```sh
python3 eink-qemu/mkflash.py <CrossDink>/.pio/build/x4-pro-debug flash.bin
eink-qemu/run.sh flash.bin sd.img          # firmware log (USB-CDC) on stdout, SDL window if available
eink-qemu/drive.py flash.bin sd.img log.txt wait:30 shot:home.png   # headless
```

`run.sh`/`drive.py` create a 2050 MiB MBR+FAT32 `sd.img` with 32 KiB clusters
if it doesn't exist. `eink-qemu/mksd.py sd.img 2050 books/` creates an image
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
`bt`. Or `info registers -a` on the HMP socket `/tmp/eink-qemu-mon.sock` and
`xtensa-esp32s3-elf-addr2line -pfiaC -e firmware.elf <PC>`.

To test a firmware without a person at the keys, use the command-line tool
([eink-qemu/desktop/cli](eink-qemu/desktop/cli/README.md)). With CrossPoint on
an X3 and a blank card, this opens the book and keeps its first page and heaps
(the first confirm opens Browse Files, the second the book; `Page render` is
CrossPoint's log line):

```sh
eink-qemu/cli run <build dir> --put book.epub ready press:confirm wait-screen \
  press:confirm 'wait-log:Page render' shot:page1 heap:page1
```

It boots headless, runs the steps, and keeps the log, every settled screen,
screenshots, heaps and a report in a run directory; `compare` runs two builds
side by side; `start`/`do`/`stop` keep a session for exploring step by step.
Each step prints one JSON record. Prefer it to `drive.py` and to on-device
scripting for anything the emulator models.

To watch the firmware's heaps live, use the desktop app (`eink-qemu/desktop`):
its Heap tab and heap window map every ESP-IDF 5.x TLSF heap (DRAM, RTC, and
PSRAM on the X4 Pro) block by block, with a free-memory trend, allocation
sites and call stacks, and a timeline that replays the allocation log. Each
read stops the VM, copies the heap regions with `pmemsave` and resumes; under
`-icount` the guest does not see the pause. The ELF must match the running app;
the app finds one beside the firmware and loads the plugin below by itself.

With `run.sh`, the heaptrace plugin alone logs which code allocated what:
`run.sh flash.bin sd.img -plugin build/contrib/plugins/libheaptrace.so,elf=firmware.elf`
(`.dylib` on macOS, `.dll` on Windows; release archives carry it in `qemu/plugins/`,
and QEMU needs `--enable-plugins`, which the release builds have).
It hooks `poison_allocated_region()` (present with the default light heap
poisoning), `multi_heap_free()` and `multi_heap_realloc()`, and logs every
allocation, free and realloc with its time and call stack to
`/tmp/eink-qemu-heaptrace.bin`, unwinding RISC-V with the ELF's `.debug_frame`
and `.eh_frame` and Xtensa through its register windows. The desktop app's
`core/src/trace.rs` reads that format. Boot to the first X3 `[MEM]` line
took 10.77 s instead of 10.57 s with the plugin (mean of three runs).

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

eink-qemu/wasm/build-deps.sh          # build-wasm-deps/sysroot-wasm64
eink-qemu/wasm/build.sh --web         # default: build-wasm-32limit/web-dist
eink-qemu/wasm/run-node.sh flash.bin sd.img
python3 eink-qemu/wasm/serve.py build-wasm-32limit/web-dist
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
WASM64_MODE=64 eink-qemu/wasm/build.sh --web
WASM64_MODE=64 eink-qemu/wasm/run-node.sh flash.bin sd.img
python3 eink-qemu/wasm/serve.py build-wasm/web-dist
```

`JOBS` sets build parallelism; `EM_CACHE` selects a writable compiler cache.
For a fresh checkout, export `WASM_SYSROOT=/absolute/path/to/sysroot-wasm64`
before both build scripts to reuse dependencies built with the same compiler.
Do not reuse the old TCI port's 32-bit sysroot. Installed dependencies are
skipped; `FORCE=1` rebuilds them. Do not rebuild a shared sysroot concurrently.
The scripts pin dependency sources and keep generated files in ignored build
directories. QEMU configure can download missing Python packages/subprojects.

### Run, package and verify

The page is the desktop app's front end (`eink-qemu/desktop/src`) built for
the browser: `package-web.sh` runs `pnpm build:browser` there (Node 22.12+ and
pnpm 10+) and lays the result over the runtime's files, the wasm binaries and
the boot assets. `pnpm dev:browser` serves the page from source against a
packaged `web-dist` for development; see
[eink-qemu/desktop/README.md](eink-qemu/desktop/README.md#the-browser-page).

Open `http://127.0.0.1:8000`, choose a release app `.bin`, a merged image or a
16 MiB flash dump and optionally an SD image; for example
`crosspoint-<ver>-x3-x4.bin` for X3 or `crosspoint-<ver>-x4pro.bin` for X4 Pro
from the [CrossPoint releases](https://github.com/crosspoint-reader/crosspoint-reader/releases).
The browser adds the bundled bootloader and partition table, and CrossPoint's
NVS entry for a CrossPoint X3 app; app bytes stay unchanged. No esptool or PlatformIO is needed.
`?flash=flash.bin&sd=sd.img` accepts either input kind from a URL; for example,
`?flash=crosspoint-1.6.5-x3-x4.bin&sd=sd.img` with copies in the served directory.
App header chip IDs 5 and 9 select X3 and X4 Pro; unsupported IDs are rejected.
For full dumps, bootloader byte 12 equal to 5 selects X3; otherwise X4 Pro.
The browser has a portrait SDL panel, CDC log, keys and X4 Pro pointer input.
Reset resets the guest; Stop retains the SD in page memory for the next Start.
The card is saved in the browser (explorer changes at once, device changes
every 10 s and on Stop) and "Save a copy" downloads it; flash changes are not
exported. A blank 64 MiB FAT32 card is included.
The SD card panel is a file explorer for the in-memory card: upload (picker or
drag-and-drop, folders included), new folder, rename, delete, download a file,
before Start, while running and after Stop. `fat.js` edits FAT32 in place; exFAT,
FAT12 and FAT16 cards are reported as unsupported (the emulator still boots them).
A change while the emulator runs pauses the guest, edits `/sd.img` in the runtime
iframe and resets the device so the firmware mounts the modified card again.
`node eink-qemu/wasm/web/fat.test.mjs` checks `fat.js` against dosfstools and mtools.

Node mounts host files under `/host`. `run-node.sh` copies flash into `.run`
but writes the supplied SD directly, so use a copy when testing. Ctrl-a c
selects the HMP monitor. Smoke tests copy both input images and save logs,
measurements and PPM panel captures under `build-wasm/evidence/smoke`:

```sh
eink-qemu/wasm/smoke.sh flash.bin sd.img  # X3 or X4 Pro Home, JIT and panel
eink-qemu/wasm/smoke.sh --rom-only       # no firmware; used in CI
eink-qemu/wasm/package-web.sh           # package existing default-mode binaries
```

Use `WASM64_MODE=64` with smoke/packaging for the native Memory64 build.
`WASM_JIT_STATS=1` on the Node launcher logs successfully instantiated TB
modules. `REFERENCE_PPM=/absolute/path/to/native.ppm` makes smoke compare the
firmware panel capture byte for byte. X4 Pro firmware is selected by chip ID 9
in the bootloader/app header; it requires three `8179_DRF` refreshes and the
following `[MEM]` checkpoint. X3 firmware also runs the existing X4 Pro
blank-flash ROM check. Browser automation and controls are documented in
[eink-qemu/wasm/web/README.txt](eink-qemu/wasm/web/README.txt).

The launchers use ordinary TCG blocks with a 64 MiB translation cache and
`-icount shift=0,sleep=on` for both X3 and X4 Pro. The Wasm backend
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
`TURBO=1 eink-qemu/wasm/run-node.sh flash.bin sd.img`. The native launcher also defaults to accurate timing;
`X4TURBO=1 eink-qemu/run.sh flash.bin sd.img` opts
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
`smoke.sh --rom-only`, then uploads `eink-qemu-<version>-wasm`. The wasm64
sysroot/compiler cache key includes the SDK version and dependency recipes.
The artifact contains both emulators, ROMs, browser assets, boot assets from
[eink-qemu/boot/](eink-qemu/boot/README.md) and the blank card;
it contains no firmware apps or user SD images.

Pages uploads and deploys only on **pushes to `main` in
`serialx/eink-qemu`**, to <https://serialx.github.io/eink-qemu/>.
`WASM_PAGES_BRANCH` in the workflow is the single branch setting;
other branches, PRs and forks do not
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

See [what makes it fast and rejected experiments](eink-qemu/wasm/README.md#what-makes-it-fast)
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
[the performance report](eink-qemu/wasm/perf/turbo-profile.md).

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
[Wi-Fi and font downloads](eink-qemu/wasm/README.md#wi-fi-and-downloads).
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
second Power press did not resume in this October 10 check. The October 13
[calibration](eink-qemu/x4cal/README.md) adds timer-based deep-sleep wake and
retention. Since October 16 the RTC controller models ext0/ext1 wake: holding
Power puts the 1.6.5 X4 Pro build into deep sleep (the VM suspends, the guest
clock stops) and a Power press wakes it through EXT1 (`rst:0x5 (DSLEEP)`) back
to Home.

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
python3 eink-qemu/drive.py x4pro.bin sd.img native.log wait:25 \
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

## Implemented features and remaining gaps

Machine `-machine x4pro` (in `hw/xtensa/esp32s3.c`: `x4pro_board_init`) boots the
unmodified `x4-pro-debug` image to the Home screen, takes key presses and renders
the panel.

Done in `eink-qemu`:
- Octal PSRAM 8 MB (default on `x4pro`), plus a PSRAM bounds fix.
- `hw/misc/esp32s3_usb_jtag.c`: USB-CDC console (firmware log) to a chardev.
- `hw/misc/esp32s3_sens.c`: SAR ADC oneshot always "done" (IDF calibration at boot).
- `hw/gpio/esp32s3_gpio.c`: real GPIO model: OUT/ENABLE/IN, edge/level
  interrupts, named lines `pin-in` (drive a pin from outside), `pin-out` (pin level
  to a device), `wake` (light-sleep GPIO wakeup), `rtc-pad` (levels of GPIO0-21,
  the RTC pads, for deep-sleep ext0/ext1 wake). Undriven inputs read 1 (pull-up).
- `hw/ssi/esp32s3_gpspi.c`: GPSPI2 master with CPU-buffer and GDMA
  transfers (USR, TRANS_DONE).
- `hw/display/uc8179.c`: the X4 Pro's UC8279 revision `0x68` panel by default,
  or the legacy UC8179 with `X4PANEL=uc8179`. Both use GPSPI2 with CS/DC/RST and
  BUSY_N on GPIO, plus bit-banged SDA reads for panel identity and OTP.
  The selected profile supplies its own VER, FLG and OTP responses. The graphic
  console is shown portrait like the device (`portrait=false` for raw). `hw/display/ssd1677.c` is the SSD1677 of
  the X4 (C3) and the Picco.
- Every panel model (`uc8179`, `uc8279`, `ssd1677`, at
  `/machine/peripheral/panel`) has read-only QOM properties for scripts and
  firmware-neutral metrics: `busy` (BUSY asserted now), `refreshes` (refreshes
  completed since QEMU started: DRF 0x12 on the UltraChip panels, a master
  activation 0x20 whose update control 2 (0x22) includes the display stage on
  the SSD1677; power on/off, LUT loads and resets don't count) and
  `refresh-ns` (their summed BUSY time in guest ns). The counts survive guest
  and external resets. A refresh that RST cuts short is not counted (the
  UC8253/UC8279 model keeps BUSY through RST, so its refresh completes).
- Legacy UC8179 ink and ghosting (full notes, data and tests in
  `eink-qemu/ghosting/README.md`): each refresh (0x12) is simulated frame by frame
  instead of showing the planes, and plays on screen one frame per `frame-us`
  of virtual time (full refreshes flash, fast ones paint in). BUSY lasts the
  frames' length: boot full 1500 ms (device 1493), 50-frame direct gray 1250 ms
  (device 1189), DU 24 frames 600 ms (device 662).
  To reproduce these legacy measurements with `run.sh`, `drive.py` or
  `wasm/run-node.sh`, set `X4PANEL=uc8179`; in the browser, select UC8179
  with accurate timing (`?panel=uc8179&turbo=0`). The `-global uc8179.*`
  properties below also apply to the default UC8279 profile, so they do not
  select the legacy panel.
  - PSR REG picks the waveform per refresh: REG=1 runs the uploaded register LUTs
    (0x20 VCOM, 0x21 WW, 0x22 KW, 0x23 WK, 0x24 KK); REG=0 runs stand-ins for the
    OTP waveforms (bodies never dumped): fast inside PTIN/PTOUT, full otherwise.
    CDI N2OCP copies NEW to OLD after the refresh.
  - `ghosting` defaults to **off** in native and Node CLI runs. Opt in with
    `-global uc8179.ghosting=on` (X4 Pro) or `-global uc8279.ghosting=on` (X3).
    The web page enables it by default; its **Ghosting** switch applies at start,
    and `?ghosting=0` disables it for browser QA. With ghosting off, ink motion is
    linear with hard saturation: no remnant charge, blooming, soft rails, OTP
    hold drive or idle drift. LUT grayscale, orientation, animation and BUSY
    timing remain active. Compare screenshots after the refresh completes, or
    use `animate=false` to apply all frames immediately.
  - With ghosting on, per pixel: saturating particle motion, two-component remnant voltage,
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
- X4 Pro uses `-icount shift=0` with calibrated instruction costs and CPU clock
  scaling (selected by native, Node and browser launchers). See the measured
  [CPU, SDMMC, SPI and panel profile](eink-qemu/x4cal/README.md). Host execution
  speed and guest virtual time are separate; allow headless steps enough time.
- `eink-qemu/testdata/`: real device logs, the panel OTP dump, battery history to
  seed the SD card, estimated power numbers. See its README.
- `hw/input/x4pro_keys.c`: Up/Down/Power keys (arrow keys + P, or
  `qom-set /machine/x4pro-keys down true`; the Picco's are `picco-keys`).
  `power-boot-ms` (default 1500, 0 = off) holds Power that long after a cold
  power-on, and after a deep-sleep wake by a pad while Power is pressed or was
  pressed in the last 2 s of guest time: the hand that turns the device on is
  still on the button when the firmware checks. A software or watchdog reset,
  a timer wake, or a wake by another key when Power was not pressed in the last 2 s
  of guest time, boots with Power released. The stock
  X4 Pro firmware (7.5.11) wants Power held 600 ms when its app starts (about
  2.5 s after power-on): `-global x4pro-keys.power-boot-ms=3000` boots it with
  no press.
- `hw/misc/esp32s3_rtc_cntl.c`: light sleep waits for an RTC timer, GPIO, ext0
  or ext1 wake. RTC continues while SYSTIMER is suspended, including
  periodic-alarm phase. Deep sleep ends in a DEEPSLEEP reset that retains RTC
  memory and restarts both cores, with the cause in `SLP_WAKEUP_CAUSE`.
  - ext0 (`RTC_IO_EXT_WAKEUP0` pad, `EXT_WAKEUP0_LV` level) and ext1
    (`EXT_WAKEUP1` pad mask, any high or all low, the pads that fired in
    `EXT_WAKEUP1_STATUS`) watch the RTC pads, GPIO0-21, through the GPIO
    block's `rtc-pad` lines. The RTC IO block at 0x60008400 is a plain
    register file with `EXT_WAKEUP0` and `RTC_GPIO_IN` (pad levels) live.
  - Deep sleep that only a pad can end suspends the VM (the guest clock stops);
    the wake resumes it. Deep sleep with no wake source modelled (or none
    armed) is the chip off: the VM stays suspended until a reset. Light sleep
    with no wake source returns at once, as before. Deep sleep with the RTC
    timer armed keeps the VM running until the timer or a pad ends it. Either
    way the RTC's read-only QOM property `deep-sleep` (`/machine/rtccntl` on
    the C3, `/machine/soc/rtc_cntl` on the S3) is true while it lasts.
  - The SoC resets the RTC domain itself: an external reset (QEMU start, QMP
    `system_reset`, also of a suspended VM) is a power cycle with reset reason
    POWERON and the last wake forgotten; guest resets keep theirs (SW_SYS,
    SW_CPU, DEEPSLEEP). The C3's `esp32c3_rtc_cntl.c` follows the same rules.
- SD card: `eink-qemu/mksd.py` makes an MBR + FAT32 image (SdFat needs the MBR);
  the existing `dwc_sdmmc` model mounts it.
- `eink-qemu/drive.py`: headless runner with steps (`wait:S`, `press:down[:ms]`,
  `shot:file.png`, `hmp:cmd`). Example:
  `eink-qemu/drive.py flash.bin sd.img log.txt wait:30 press:down shot:home.png`

- Wi-Fi: the MAC is emulated with a fake AP bridged to a QEMU NIC (port of the
  lcgamboa ESP32-C3 model: `hw/misc/esp32s3_wifi.c`, `esp32_wifi_ap.c`,
  `esp32_wlan_packet.c`; analog/FE in `esp32s3_ana.c`, `esp32_fe.c`). `run.sh`
  joins it to a network:
  - `X4BR=br0 eink-qemu/run.sh ...` bridges to your LAN: the firmware does its own
    DHCP with your router and is reachable at that IP like a real device. Needs a
    host bridge `br0` holding your Ethernet port, `allow br0` in
    `/usr/local/etc/qemu/bridge.conf`, and root (the helper is `build/qemu-bridge-helper`).
    Wi-Fi host NICs can't be bridged; use Ethernet.
  - Default: QEMU NAT (`-nic user,model=esp32_wifi,hostfwd=tcp::8080-:80`), DHCP
    10.0.2.15, host port 8080 reaches the device's port 80 (`curl localhost:8080`).
  - `X4NET="-nic ..."` overrides both; `drive.py` takes the same `-nic` in `QEMU_EXTRA`.
  A scan finds `PICSimLabWifi` (open, ch 1, the one that connects), `Espressif` and
  `MasseyWifi`. With no `model=esp32_wifi` NIC the MAC is a stub and a scan finds 0;
  `-global esp32_wifi.access-points=off` keeps the radio with no access points (the
  apps' Wi-Fi off), so a scan finds 0 too.
  Station MAC: the NIC's `mac=` (default 52:54:00:12:34:56) is burned into efuse
  when no efuse file is given; give each sim on one LAN its own `mac=`.

Still missing: LEDC frontlight, charger STAT (GPIO21 reads 1 = charging), real Wi-Fi
(WPA, signal, other APs),
USB OTG.

Gaps another firmware is likely to hit (CrossDink doesn't need them yet):
- GPSPI: hardware CS,
  command/address/dummy phases.
- GPIO matrix / IO_MUX routing: pins are wired directly, so a peripheral routed to
  a different pin than the X4 Pro's won't reach the device.
- UC8179: the 0x90 partial window is modeled only in the UC8279-0x68 profile.
  LUTBD (0x25), VDHR, VCOM_DC value and temperature (TSSET) aren't modeled;
  the OTP waveforms are stand-ins until the full 0xA2 OTP is dumped.
  Open calibration items: `eink-qemu/ghosting/README.md` section 8.
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
| Sleep | light and deep sleep; deep-sleep wake on GPIO0-5 or the RTC timer | RTC_CNTL | `esp32c3_rtc_cntl.c`: RTC timer, light sleep (timer/GPIO), deep sleep = reset with reason DEEPSLEEP and the wake cause; deep sleep with no wake source = off until a reset; an external reset is POWERON |
| Wi-Fi | MAC + fake open AP bridged to `-nic user` | n/a | the S3 models at the same bases (`esp32s3_wifi.c`, `esp32s3_ana.c`, `esp32_fe.c`) |
| Bluetooth | controller comes up, scans find nothing | n/a | `esp32s3_ble.c` at 0x60031000, 64 KiB exchange memory at 0x3fc00000, BT_LPCK clock in `esp32c3_clk.c`; no link layer (below) |

Notes:
- X3 CPU timing uses `-icount shift=0,sleep=on` (selected by `run.sh` and
  `drive.py`), so each tick is 1 ns. CPU properties on `espressif-riscv-cpu`
  are `cost-rom-ns=6`, `cost-sram-ns=8`, `cost-flash-ns=10`, and additive
  `cost-load=0`, `cost-store=3`, `cost-mul=0`, `cost-div=160`, `cost-branch=2`.
  The clock registers select a factor relative to 160 MHz; 10 MHz costs 16x.
  Override with `-global espressif-riscv-cpu.cost-flash-ns=9`, for example.
  These are effective calibrated costs, not a cache or pipeline simulation.
  See [CPU and SD refit](eink-qemu/sdcal/cpu.md) for probe residuals, page
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
  See [the calibration notes](eink-qemu/sdcal/cpu.md) for all 30 rows,
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
  See [panel calibration](eink-qemu/sdcal/panel.md) for the fit, measured waits,
  LUT repeat semantics, and limitations.
- A power-button boot needs the button still held when the firmware checks it
  (CrossPoint re-sleeps otherwise): `x3-keys.power-boot-ms` (default 1500, 0 =
  off) holds Power after a cold power-on (QEMU start, an external reset) and
  after a deep-sleep wake by a pad while Power is pressed or was pressed in the
  last 2 s. Software and watchdog resets and timer wakes boot with it released.
- CrossPoint up to 1.6.5 deadlocks on a first boot whose NVS has no cached device
  type: the X3 fingerprint probe runs `Wire.begin/end`, then `getWakeupReason`
  reads the gauge through raw `Wire` calls with the bus down, and the failed
  `endTransmission` keeps the Wire lock, so the IMU init blocks forever. A device
  that has booted once has `cphw/dev_det` in NVS and skips the probe, which is why
  nobody sees it. `mkflash.py` seeds exactly that key (`mknvs.py`, from
  `profiles/crosspoint.json`) into C3 images that carry CrossPoint's strings.
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
- Bluetooth: CrossPoint's page turner (C3 builds only; NimBLE over Espressif's
  `libbtdm_app_flash.a`) enables the controller, which asserts on the SYSTEM
  BT_LPCK read-back and then drives a RivieraWaves link layer at 0x60031000 with
  its buffers in an exchange-memory window at 0x3fc00000 (remapped into DRAM by
  the table at 0x60031204). With the clock registers, the register block and the
  window in place the radio comes up and "Scan for Remotes" runs; nothing answers
  because the link layer (exchange table, scan and connection events, RWBLE
  interrupt) is not modeled, so no fake remote exists yet.
- Not modeled: the frontlight (the X3 has none), USB MSC, flash encryption, the
  Bluetooth link layer.

## Xteink X4 (`-machine x4`, ESP32-C3)

The X4 shares the X3's ESP32-C3, so the chip header cannot tell them apart.
The desktop app, browser page, CLI and composer default a C3 image to `x3`.
The desktop app, browser page and Rust CLI also recognize filename tokens:
`x4` without `x3` selects X4; both or neither select X3. Tokens contain ASCII
letters and digits and ignore case; folders and `x4pro` do not
count as `x4`. A saved choice for that firmware wins over the filename.
Select X4 explicitly with `X4MACHINE=x4` in the launchers or `--machine x4`
in the CLI and composer. In the browser, choose **X4** or use `?device=x4`.
TRMNL's merged firmware `.bin` runs here.

```sh
python3 eink-qemu/mkflash.py --machine x4 FW1.8.17.bin flash-x4.bin
X4MACHINE=x4 eink-qemu/run.sh flash-x4.bin sd.img
X4MACHINE=x4 eink-qemu/wasm/run-node.sh flash-x4.bin sd.img
```

Merged images preserve their bootloader, partition table, NVS and app bytes.
App-only X4 images use the bundled bootloader and Arduino-ESP32 partition table
without forcing X3 detection in NVS.

The X4 SSD1677 model supports framebuffer writes, RAM windows and active-high
BUSY. Refresh timing is simplified and ghosting is not modeled. The console
uses the device's 480x800 portrait orientation; TRMNL draws its setup interface
in landscape.

## BOOX Picco (`-machine picco`, ESP32-S3)

ESP32-S3 chip detection alone defaults to the X4 Pro, so the Picco needs an
explicit board selection: `X4MACHINE=picco` and `--machine picco`, or
**BOOX Picco** / `?device=picco` in the browser.

```sh
python3 eink-qemu/mkflash.py --machine picco picco1.0.2920.bin flash-picco.bin
X4MACHINE=picco eink-qemu/run.sh flash-picco.bin sd.img
X4MACHINE=picco eink-qemu/wasm/run-node.sh flash-picco.bin sd.img
```

The composer places the unchanged app at `0xE0000` using Picco's NVS, OTA,
recovery-slot and app layout. The recovery slot stays erased because an app
update does not contain recovery firmware. The remaining flash provides an
empty `spiffs` partition for LittleFS; this allocation is for the emulator.
Merged flash images retain their original layout and contents.

The board includes the SSD1677 display, Up/Down/Power keys, TMA525C touch,
SGM41562 charger, BM8563 RTC, QMI8658 motion sensor and SDMMC card. Display
waveforms and charging are simplified; frontlight brightness, multitouch and
touch-controller firmware updates are not simulated.

Host pointer taps last at least 100 ms of guest time so slower emulation does
not collapse a click between UI polls. Set `-global tma525c.host-touch-ms=0`
to disable this input timing adjustment; scripted QOM `touch` events remain
immediate. The browser also keeps Picco's guest clock paced in Turbo mode so
ordinary clicks do not become long presses; display and bus timing still use
the Turbo settings. `?sleep=off` permits unpaced benchmarks.

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
| Sleep | deep sleep + ext0/ext1 GPIO wake, RTC_NOINIT/RTC_DATA memory | RTC_CNTL | done: ext0/ext1 and timer wake, RTC memory kept (`esp32s3_rtc_cntl.c`) |

## GPSPI2 follow-up

`hw/ssi/esp32s3_gpspi.c` supports GDMA TX/RX transfers; the old
`wip/spi-dma` draft is obsolete. Command/address/dummy phases and hardware CS0
remain unimplemented. GDMA uses the system address space, including mapped
PSRAM; firmware-level PSRAM-buffer coverage is still needed.

The planned `eink-qemu/tests/spi-dma/` firmware should exercise ESP-IDF
`spi_master` DMA and hardware CS with a panel test pattern, with source and a
build script committed alongside a screenshot check. That firmware is not yet
in the tree; existing qtests cover descriptor chains and inactive DMA phases.

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

- Don't modify any firmware (CrossDink, freeink-sdk or others). Keep model and
  board-wiring changes within the task's agreed scope; coordinate hooks in
  other devices in the PR.
- Submit changes through PRs into `main`. Maintainer release version bumps
  have also landed directly on `main` (for example, `eink-qemu: 0.5.0`,
  `2c7443b56b`). Task branches still require a PR.
- No upstream references in PR titles, bodies or commits (no `#N` pointing at other
  repos, no `owner/repo#N`, no "fixes/closes" keywords).
