<h1 align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="eink-qemu/logo/eink-qemu-dark.svg">
    <img src="eink-qemu/logo/eink-qemu-light.svg" width="260" alt="eink-qemu">
  </picture>
</h1>

eink-qemu emulates **ESP32-based e-ink readers** so you can run and debug their
firmware without the device. It runs **unmodified firmware**: the stock
firmware, CrossPoint, CrossInk, CrossDink, TRMNL, or your own ESP-IDF or
Arduino app. The screen, SD card, keys, touch, clock, battery gauge and Wi-Fi
behave like the real board. No firmware is ever patched to boot here; the
hardware is modeled from datasheets and measurements of real devices. The
tools assume ESP-IDF and nothing more about the firmware; what they know
about particular firmware (CrossPoint's boot milestones and render times,
the stock firmware's boot gate) is data in [eink-qemu/profiles](eink-qemu/profiles/README.md).

**Try it now in your browser:** https://serialx.github.io/eink-qemu/

<a href="https://serialx.github.io/eink-qemu/"><picture>
  <source media="(prefers-color-scheme: dark)" srcset="eink-qemu/screenshots/browser-dark.png">
  <img src="eink-qemu/screenshots/browser-light.png" alt="The eink-qemu browser emulator running CrossPoint on an Xteink X4 Pro, showing a page of Aesop's Fables, with the SD card files on the left and the firmware console on the right">
</picture></a>

eink-qemu is upstream QEMU 11.1.2 with Espressif's ESP32 SoC models, the Xteink
X4 Pro board and panel models from [prokrypt/x4prosim](https://github.com/prokrypt/x4prosim),
and Kohei Tokunaga's WebAssembly JIT. Where each piece came from is in
[Origins](#origins).

## Supported boards

| Board | SoC | Machine | Emulator binary | Fidelity |
| --- | --- | --- | --- | --- |
| Xteink X4 Pro | ESP32-S3 | `x4pro` | `qemu-system-xtensa` | Full board; CPU, PSRAM, SD, SPI and UC8279 timing [calibrated on a real X4 Pro](eink-qemu/x4cal/README.md) |
| Xteink X3 | ESP32-C3 | `x3` | `qemu-system-riscv32` | Full board, SD, panel and CPU timing calibrated on a real X3 |
| Xteink X4 | ESP32-C3 | `x4` | `qemu-system-riscv32` | Runs firmware; panel refresh timing simplified, no ghosting |
| BOOX Picco | ESP32-S3 | `picco` | `qemu-system-xtensa` | Runs firmware; panel waveforms and charging simplified |

The board defaults to X4 Pro for ESP32-S3 firmware and X3 for ESP32-C3.
The desktop app, browser and Rust CLI also recognize X4 and Picco filename
hints, as described below. Select the board explicitly when the default or
filename hint does not match the device.

## Try it in the browser

1. Open https://serialx.github.io/eink-qemu/ in Chrome. Firefox and Safari are
   untested.
2. Choose the **Firmware**: a release app `.bin` (a CrossPoint release, for
   example), a merged firmware `.bin`, an X4 Pro `.xota` update, or a 16 MiB
   flash dump. The
   emulator starts as soon as the file loads.
3. New firmware defaults to X3 for ESP32-C3 and X4 Pro for ESP32-S3. An `x4`
   filename token without `x3` selects X4; `picco` selects Picco on S3.
   Tokens are ASCII alphanumeric runs and ignore case. A remembered choice
   for that firmware wins over the filename. Change the device in the
   firmware card (the chip at the top of the page), or use `?device=x4`
   to override the choice in a link.

Use the arrow keys, Enter, Esc and `P` (Power). On boards with a touchscreen,
click the screen to touch it. The SD card panel lets you add books, swap in
your own card image, and download the card.

The page starts with **Turbo** on, which runs fast but not with real device
timing. Turn it off, or open the page with `?turbo=0`, for accurate timing.
**Ghosting** is on for X3 and X4 Pro to show real e-ink artifacts. Turn it off,
or use `?ghosting=0`, when you need clean pixels.

## Desktop app

The same interface as a macOS and Windows app (one front end serves both,
[eink-qemu/desktop](eink-qemu/desktop/README.md)), running native QEMU, several
times faster than the browser. Its SD card explorer edits card images of any
size in place. Download `eink-qemu-desktop-*.dmg` (macOS 12.3+) or
`eink-qemu-desktop-*-setup.exe` (Windows 10/11) from
[Releases](../../releases). The builds are not signed: on macOS run
`xattr -dr com.apple.quarantine /Applications/eink-qemu.app` once after
copying the app; on Windows choose *More info*, then *Run anyway*. Building
it yourself is covered in [eink-qemu/desktop](eink-qemu/desktop/README.md).

![The eink-qemu desktop app running CrossPoint on an Xteink X3, with SD card files, a page of Aesop's Fables, and live heap maps and memory usage](eink-qemu/screenshots/desktop-light.png)

## Run it on your computer

### Get the emulator

Download an archive from [Releases](../../releases), or from the latest
[Actions](../../actions) run (artifacts `dist-qemu-*`). Builds cover Linux
(x86_64, arm64), macOS (Intel, Apple silicon) and Windows. Each archive holds
one emulator: `xtensa-softmmu` for the X4 Pro and Picco, `riscv32-softmmu` for
the X3 and X4. Linux and macOS need glib, pixman, SDL2, libslirp, libgcrypt and
libpng installed; the Windows archives bundle them.

To build from source instead, use Python 3.9+, GLib 2.66+ and libgcrypt 1.9.4+
(Debian 12 has all three). The first configure needs network access to fetch
Python packages and subprojects. No Rust toolchain is needed.

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
ninja qemu-system-xtensa qemu-system-riscv32 && cd ..
```

### Run your firmware

Compose a 16 MiB flash image, then start the emulator. `mkflash.py` accepts an
app `.bin`, a merged `.bin`, a PlatformIO or ESP-IDF build directory, or a flash
dump. A merged image keeps its own bootloader, partitions and data; a build
directory brings its own bootloader and partition table; an app gets the bundled
bootloader and Arduino-ESP32's 16 MB partition table (the Picco's stock layout
with `--machine picco`), or the firmware's own with `--partitions partitions.csv`.

```sh
python3 eink-qemu/mkflash.py crosspoint-1.6.5-x3-x4.bin flash.bin
eink-qemu/run.sh flash.bin sd.img        # SDL window, firmware log on stdout
```

X4 Pro defaults to the measured UC8279 revision `0x68` panel in native, Node,
and browser runs. For a reader with the older panel, use
`X4PANEL=uc8179 eink-qemu/run.sh flash.bin sd.img`; the same variable works
with `drive.py` and the Node launcher. In the browser and the desktop app, the
firmware card's **Display panel** choice is remembered; turn off Turbo to use
measured panel delays.
See the [calibration report](eink-qemu/x4cal/README.md)
for measured CPU, SDMMC, SPI, panel and sleep timing, and the remaining cache
and optical limits. X4 Pro launchers use `icount shift=0` with these defaults.

For the X4 or the Picco, name the board in both steps:

```sh
python3 eink-qemu/mkflash.py --machine picco picco1.0.2920.bin flash-picco.bin
X4MACHINE=picco eink-qemu/run.sh flash-picco.bin sd.img
```

A dump of a real device works too, as long as it doesn't use flash encryption:
`esptool.py --chip esp32s3 read_flash 0 0x1000000 flash.bin`.

`run.sh` creates `sd.img` on first run, an empty card shaped like the stock X3
card. To start with books on it, build one from a folder with
`python3 eink-qemu/mksd.py sd.img 2050 books/`.

**Controls:** arrow keys are Up and Down, and `P` is Power. Click in the window
to touch. On the X3, Left, Right, Enter (Confirm) and Backspace (Back) work too.
The QEMU monitor is at `socat - unix:/tmp/eink-qemu-mon.sock`.

### Scripted runs and tests

The command-line tool boots a firmware headless and runs a script of steps
against it: key presses and touches, waits on the log or the screen,
screenshots, heap snapshots and allocation traces. It takes a build
directory or a release `.bin` directly, and keeps everything a run saw in a
run directory with a report:

```sh
eink-qemu/cli run build/ ready shot:boot press:down wait-screen shot:next heap:next
eink-qemu/cli run .pio/build/default --put book.epub \
  ready press:confirm wait-screen press:confirm 'wait-log:Page render' \
  shot:page1 heap:page1                    # CrossPoint: open a book
eink-qemu/cli compare base/.pio/build/default pr/.pio/build/default \
  --sleep off --put book.epub --script reading.txt
```

The CLI defaults to `--sleep off`, skipping idle host waits while preserving
device timing. Leave `--turbo` off for firmware benchmarks. Older CLIs without
`--sleep` accept `--qemu-arg=-icount --qemu-arg=sleep=off`. Compare guest or
firmware timings; host elapsed time depends on pacing. Pass `--sleep on`
for interactive CLI sessions, including `start`/`do` and `--gui`.

`ready` waits for a quiet screen (for CrossPoint, its Home and memory
report). `compare` runs two builds side by side and writes a table for a
pull request: boot time, panel refreshes and guest time per marked window,
heaps and screenshot differences, and CrossPoint's render times when it logs
them. `start`, `do` and
`stop` drive a long-lived session step by step, and `--gui` shows a run in
the desktop app's window. The tool needs Rust to build; the guide is
[eink-qemu/desktop/cli](eink-qemu/desktop/cli/README.md). The older
`eink-qemu/drive.py` still takes `wait:`, `press:`, `shot:` and `hmp:` steps
without Rust.

## What's emulated

The X4 Pro:

| Part | Model | Status |
| --- | --- | --- |
| CPU, flash, PSRAM | ESP32-S3 dual core, 16 MB flash, 8 MB octal PSRAM | done |
| Display | UC8279 (revision 0x68, the default) or UC8179 800x480 e-ink: identity/OTP probe, register LUTs, animated refreshes, physical ghosting fitted to device photos | done |
| SD card | SDMMC, FAT32 image, measured command/DMA/card delays | done |
| Keys | Up, Down, Power (GPIO, wake from light and deep sleep) | done |
| Touch | GT911 on I2C | done |
| Clock | BM8563 RTC on I2C | done |
| Battery | CW2017 fuel gauge on I2C | done |
| Wi-Fi | emulated MAC + a fake open AP (`PICSimLabWifi`), bridged to the host network | done |
| Light sleep | RTC timer and GPIO wake | done |
| Deep sleep | RTC timer and ext0/ext1 pad wake (the VM suspends until a pad wakes it), retained memory, both-core restart; no wake source means off until a reset; boot latency approximate | done |
| Frontlight, charger STAT, USB OTG | | not yet |

**The X3** has a UC8279d or UC8253 792x528 panel, an SD card in SPI mode, six
keys on an ADC ladder plus Power, a BQ27220 gauge, a DS3231 clock, a QMI8658
IMU, the same Wi-Fi, and deep sleep with power-button wake. Its panel uses the
X4 Pro's ink and ghosting model with X3 waveforms. SD card and SPI bus
latencies, panel refresh times and CPU instruction costs were measured on a
real X3 and fitted; see [eink-qemu/sdcal](eink-qemu/sdcal/README.md).

**The X4 and the Picco** are covered in their own sections of
[EINK-QEMU.md](EINK-QEMU.md), with their peripherals and limits.

**Wi-Fi.** Join `PICSimLabWifi` on the device. `run.sh`, the desktop app and
the Rust CLI (with `--wifi`) use NAT by default, without host setup.
`run.sh` also forwards `localhost:8080` to the device's port 80.
For a device address directly on your LAN, select **Bridged** in the desktop
firmware card, add `--wifi --wifi-mode bridge` to the CLI, or set
`X4WIFI_MODE=bridge` for `run.sh`. Desktop packages with managed bridge support
provide a setup wizard; cross-platform native acceptance is still in progress.
The CLI and shell retain manual setup. See
[host setup and validation limits](EINK-QEMU.md#native-wi-fi).

**Ghosting** is off by default in native and Node runs, so screenshots compare
pixel for pixel. Turn it on with `-global uc8179.ghosting=on` on the X4 Pro or
`-global uc8279.ghosting=on` on the X3. With ghosting off the panel still
renders LUT grayscale and keeps real refresh timing. `-global uc8179.animate=false`
skips the refresh animation, with the `uc8279` prefix on the X3. When comparing
pixels, capture after the refresh completes. The model is explained in
[eink-qemu/ghosting](eink-qemu/ghosting/README.md).

## Comparing timing with a real device

Match the SD card's filesystem, not just its files. The firmware walks FAT
cluster chains when it seeks in large font files, so smaller clusters make it
slower. A 128 MiB image with 512-byte clusters made font prewarming slower than
on the stock card.

`mksd.py` defaults to a 2050 MiB FAT32 image with **32 KiB clusters**, the
geometry of the stock X3 card. FAT32 needs at least 65,525 clusters, so that is
also the smallest size it accepts at 32 KiB. Smaller test images must choose a
smaller cluster size and are not valid for timing:

```sh
python3 eink-qemu/mksd.py compact-sd.img 64 --cluster-size 512
```

The browser's bundled 64 MiB card is one of these compact images. Sizes are in
MiB. Images are sparse where the host filesystem supports it, but copying one
can still take its full size. Use accurate timing, not Turbo, and record the
card's geometry with any measurement.

For identical input trees and geometry, `mksd.py` produces byte-identical images
with the same dosfstools/mtools versions. It fixes the FAT volume ID to
`45494e4b`, pins all FAT timestamps to 1980-01-01 00:00:00 UTC, and copies entries
in sorted order at every directory level. Source timestamps are unchanged;
the builder overrides `SOURCE_DATE_EPOCH` and `TZ` for its child processes.
Timestamp pinning requires mtools 4.0.20 or newer.

## Building for the browser and Node

The WebAssembly build compiles hot guest code to WebAssembly with a JIT and
runs cold code in an interpreter. Download the `eink-qemu-<version>-wasm`
Actions artifact, or build it with Emscripten 6.0.12:

```sh
eink-qemu/wasm/build-deps.sh
eink-qemu/wasm/build.sh --web
python3 eink-qemu/wasm/serve.py build-wasm-32limit/web-dist
```

`eink-qemu/wasm/run-node.sh flash.bin sd.img` runs the same build in Node 26,
with accurate timing unless you set `TURBO=1`. Chrome 155 and Node 26 are
verified; Firefox and Safari runs are untested. Build modes, browser
requirements, benchmarks and limits are in the
[WebAssembly guide](EINK-QEMU.md#webassembly-build).

## Docs

- [EINK-QEMU.md](EINK-QEMU.md): the full guide. It covers building firmware
  for the emulator, debugging a hang with gdb, every model and its options, the
  X4 Pro pin map, the WebAssembly build, and what's still missing.
- [eink-qemu/desktop](eink-qemu/desktop/README.md): the desktop app, how
  to build and package it, and its [design](eink-qemu/desktop/DESIGN.md).
- [eink-qemu/desktop/cli](eink-qemu/desktop/cli/README.md): the
  command-line tool for scripted runs, heap profiling and comparisons.
- [eink-qemu/sdcal](eink-qemu/sdcal/README.md): how the X3's SD, panel and CPU
  timing was measured and fitted.
- [eink-qemu/ghosting](eink-qemu/ghosting/README.md): how the e-ink model
  works, its calibration against photos, and tools for repeating it.
- [eink-qemu/testdata](eink-qemu/testdata/README.md): real device logs, the
  panel OTP and battery history, for checking a run against the device.

## Contributing

Read the workflow section of [EINK-QEMU.md](EINK-QEMU.md) first. Branch off
`main`, keep one peripheral per pull request, and write commit messages as
`eink-qemu: <what>`. How the QEMU 11.1.2 port is layered is in
[eink-qemu/porting](eink-qemu/porting/README.md).

## Origins

eink-qemu descends from several lines of work. All of it is GPL-2.0 QEMU code
except where noted.

- **QEMU.** The tree is upstream [QEMU](https://www.qemu.org/) v11.1.2,
  released 2026-09-28.
- **Espressif's QEMU fork.** The ESP32, ESP32-S3 and ESP32-C3 SoC models and
  their peripherals come from [espressif/qemu](https://github.com/espressif/qemu)
  (branch `esp-develop`). Espressif wrote them against QEMU 9.2.2; eink-qemu
  re-ported them onto 11.1.2 in October 2026, layer by layer, as described in
  [eink-qemu/porting](eink-qemu/porting/README.md). The pre-port history is
  kept on the `legacy-esp-develop` branch. That fork also carries community
  models: ESP32 I2C by Peter Buchegger, RSA by Sachin Billore, AES and LEDC by
  Maksim Naumenko, and the ESP32-S3 SDMMC controller by Throows.
- **Wi-Fi.** The access-point code is Clemens Kolbitsch's 2008 QEMU work (MIT),
  adapted to the ESP32 by Martin Johnson in 2022 and to the ESP32-C3, ESP-NOW
  and SoftAP by lcgamboa for [PICSimLab](https://github.com/lcgamboa/picsimlab)
  in 2023, which is why the fake network is called `PICSimLabWifi`. x4prosim
  ported it to the ESP32-S3. See the file headers in `hw/misc`.
- **prokrypt/x4prosim.** prokrypt started
  [x4prosim](https://github.com/prokrypt/x4prosim) on 2026-10-08 as a fork of
  Espressif's QEMU, to run CrossDink on the Xteink X4 Pro without the device:
  the `x4pro` machine, its GPIO, SPI, I2C, touch, RTC and gauge models, and the
  UC8179 panel with its refresh animation and ghosting model fitted to device
  photos. eink-qemu forked it the same day, after contributing the Xteink X3
  machine and its SD, panel and CPU timing calibration upstream.
- **qemu-wasm.** The browser build rests on Kohei Tokunaga's
  [qemu-wasm](https://github.com/ktock/qemu-wasm). His Emscripten and wasm64
  host support is in upstream QEMU; his wasm64 TCG JIT backend, which compiles
  hot guest code to WebAssembly at runtime and makes the browser build fast, is
  not upstream yet and is merged here from his `wasm64-tcg-b` branch.

Much of the code in both x4prosim and eink-qemu was written with Claude Code
and Codex under human direction. Commit trailers name the sessions.

## License and credits

- QEMU is GPL-2.0; see [COPYING](COPYING) and [LICENSE](LICENSE). The original
  QEMU README is [README.rst](README.rst).
- The Wi-Fi access-point code and the wasm64 TCG backend are MIT; see the file
  headers.
- The cow logo is in [eink-qemu/logo](eink-qemu/logo).
