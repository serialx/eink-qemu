# x4prosim

An emulator for the **Xteink X4 Pro** e-reader (ESP32-S3R8, 16 MB flash, 8 MB
octal PSRAM) and the **Xteink X3** (ESP32-C3). It runs **unmodified firmware**,
whether that's the stock firmware, CrossInk, CrossDink, CrossPoint, or a bare
ESP-IDF/Arduino app. The screen, SD card, keys, touch, clock, battery gauge and
Wi-Fi all behave like the board, so you can develop firmware without the device.

<img src="x4prosim/ghosting/sim-screenshots/05-menus-final-clean.png" width="240" alt="CrossDink Home screen in x4prosim">

Built on upstream **QEMU 11.1.2**, with the Espressif SoC models ported over.
The rule is that no firmware is ever changed to make it boot here: the hardware is modeled from datasheets and
from the real board instead.

## Porting layers

See [x4prosim/porting/README.md](x4prosim/porting/README.md) for the QEMU 11.1.2
porting branches, layer ownership, build command and commit rules.

## Downloads

CI builds every push for Linux (x86_64, arm64), macOS (Intel, Apple silicon) and
Windows, one archive per emulator (`xtensa-softmmu` = X4 Pro,
`riscv32-softmmu` = X3). Get them from the latest run under
[Actions](../../actions) (artifacts `dist-qemu-*`), or from
[Releases](../../releases) for tagged versions. Each archive has `bin/`, the
`x4prosim/` scripts and the docs; `x4prosim/run.sh` finds `bin/` by itself.
Linux and macOS archives need the runtime libraries (glib, pixman, SDL2,
libslirp, libgcrypt, libpng) installed; the Windows one bundles them.

## WebAssembly build

The browser and Node builds use a wasm64 JIT, compiling hot guest blocks to
WebAssembly while cold blocks run through TCI. Download the
`x4prosim-<version>-wasm` Actions artifact, or build with Emscripten 6.0.12:

```sh
x4prosim/wasm/build-deps.sh
x4prosim/wasm/build.sh --web
python3 x4prosim/wasm/serve.py build-wasm-32limit/web-dist
```

The default `MEMORY64=2` mode uses 32-bit memory addressing for compatibility
with modern Chrome, Firefox and Safari. `WASM64_MODE=64` selects native
Memory64 (Chrome 133+, Firefox 134+, not Safari); both use 64-bit host pointers.
Chrome 155 and Node 26 are verified; Firefox/Safari emulator runs are untested.
Threads require cross-origin isolation, provided by the local server or the
bundled service worker on HTTPS hosting.

The browser page defaults to **Turbo (fast, not timing-accurate)**: the
checkbox starts ticked, and unticking it or `?turbo=0` selects accurate timing
(`?turbo=1` remains accepted). The Node launcher keeps accurate timing by
default; `TURBO=1 x4prosim/wasm/run-node.sh flash.bin sd.img` opts in.

Five quiet alternating pairs on an Apple M5 Max (Node 26, Chrome 155,
`MEMORY64=2`, `sleep=off`) compare `5ea1b8ef2e` with the six-change performance
stack. CrossPoint 1.6.5 X3 accurate startup settles in **7.518 s in Node** and
**8.961 s in Chrome**, with paired post-Home reductions of **20.8%** and **30.2%**.
Chrome's gain includes settling before the ten-second autosave; the baseline
incurred one 60–63 ms save pause. X3 waits, first MEM and whole SD contents are
unchanged, and panels match at equal virtual time. Turbo changes guest timing
and is measured separately.
Official X4 Pro firmware also passed native, Node and Chrome acceptance;
Safari/Firefox remain untested.
See [the full WebAssembly guide](X4PROSIM.md#webassembly-build) for build modes,
measurement methods, limits and upstream candidates.

## Quick start

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
ninja qemu-system-xtensa qemu-system-riscv32 && cd ..

x4prosim/mkflash.sh <pio or idf build dir> flash.bin   # bootloader + partitions + app -> 16 MB image
x4prosim/run.sh flash.bin sd.img                       # SDL window, firmware log on stdout
```

`run.sh`, `drive.py` and `mkflash.sh` pick the machine from the image's chip:
an ESP32-S3 image runs on `x4pro`, an ESP32-C3 image (the CrossPoint X3/X4
binary) on `x3`.

A flash dump of a real device works too, as long as it doesn't use flash
encryption: `esptool.py --chip esp32s3 read_flash 0 0x1000000 flash.bin`.
`sd.img` is created on first run (1 GB FAT32); `x4prosim/mksd.py sd.img 1024 books/`
copies a folder of books in.

**Controls:** arrow keys are Up/Down and `P` is Power. Click in the window to
touch. On the X3, Left/Right, Enter (Confirm) and Backspace (Back) are keys too.
Monitor: `socat - unix:/tmp/x4prosim-mon.sock`.

**Headless / scripted:**
```sh
x4prosim/drive.py flash.bin sd.img log.txt wait:30 press:down shot:home.png \
  "hmp:qom-set /machine/gt911 tap 240,529" wait:5 shot:settings.png
```

## What's emulated

| Part | Model | Status |
| --- | --- | --- |
| CPU, flash, PSRAM | ESP32-S3 dual core, 16 MB flash, 8 MB octal PSRAM | done |
| Display | UC8179 800x480 e-ink: identity/OTP probe, register LUTs, animated refreshes, physical ghosting fitted to device photos | done |
| SD card | SDMMC, FAT32 image | done |
| Keys | Up, Down, Power (GPIO, wake from light sleep) | done |
| Touch | GT911 on I2C | done |
| Clock | BM8563 RTC on I2C | done |
| Battery | CW2017 fuel gauge on I2C | done |
| Wi-Fi | emulated MAC + a fake open AP (`PICSimLabWifi`), bridged to the host network | done |
| Light sleep | RTC timer and GPIO wake | done |
| Frontlight, charger STAT, deep sleep, USB OTG | | not yet |

The **X3** (`-machine x3`, `qemu-system-riscv32`): UC8279d or UC8253 792x528
e-ink, SD card in SPI mode, the six keys on their ADC ladder plus Power,
BQ27220 gauge, DS3231 clock, QMI8658 IMU, the same Wi-Fi, and deep sleep with
power-button wake. The panel uses the X4 Pro's ink and ghosting model,
including animated refreshes, with X3 waveform decoding and calibrated
refresh timing. Details in [X4PROSIM.md](X4PROSIM.md).

**Wi-Fi.** Join `PICSimLabWifi` on the device. By default it uses QEMU's NAT:
the device gets 10.0.2.15, and `curl localhost:8080` reaches its port 80. To
put it on your LAN, so it gets an address from your router, run with
`X4BR=br0` on a host bridge (setup in [X4PROSIM.md](X4PROSIM.md)).

**Panel.** Every ghosting and timing parameter can be tuned or turned off with
`-global uc8179.<name>=N`. For quick scripted runs, `-global uc8179.animate=false`
skips the animation. See [x4prosim/ghosting/README.md](x4prosim/ghosting/README.md).

## Docs

- [X4PROSIM.md](X4PROSIM.md): the full guide. It covers building firmware for
  the sim, debugging a hang with gdb, every model and its options, the X4 Pro
  pin map, what's still missing, and how to contribute a peripheral.
- [x4prosim/testdata/](x4prosim/testdata/README.md): real device logs, the
  panel OTP and battery history, for checking a simulated run against the
  device.
- [x4prosim/ghosting/](x4prosim/ghosting/README.md): how the e-ink model works,
  its calibration against photos, and step files and tools for repeating it.

## Contributing

Branch off `x4prosim`, keep one peripheral per PR, and use commit messages of
the form `x4prosim: <what>`. Read the workflow section of
[X4PROSIM.md](X4PROSIM.md) first.

## License and credits

- QEMU is GPL-2.0; see [COPYING](COPYING) and [LICENSE](LICENSE). The original
  QEMU README is [README.rst](README.rst).
- The ESP32 SoC models come from Espressif's QEMU fork.
- The Wi-Fi MAC and access-point emulation is adapted from lcgamboa's ESP32
  QEMU work. The AP code is by Clemens Kolbitsch, modified for the ESP32 by Martin
  Johnson (MIT); see the file headers.
