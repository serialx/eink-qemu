# eink-qemu desktop

A Tauri app with the browser page's interface that runs native QEMU: SD card
explorer, device stage, firmware console and a live heap view. The interface is the page
itself: `src/desktop.css` imports `wasm/web/style.css`, and the fonts, marks
and firmware modules come from the same directory. Design notes are in
[DESIGN.md](DESIGN.md).

## Run

Build QEMU first ([EINK-QEMU.md](../../EINK-QEMU.md#build-and-run)); in
development the app runs `build/qemu-system-riscv32` and
`build/qemu-system-xtensa` from the repository root, or the directory in
`EINK_QEMU_BIN`. QEMU must include the `eink` display (`ui/eink.c`). Then,
with Node 22.12+, pnpm 10+ and Rust 1.90+:

```sh
cd eink-qemu/desktop
pnpm install
pnpm tauri dev
```

`pnpm dev` and `pnpm build` first run `scripts/prepare-resources.sh`, which
needs Python 3, and dosfstools and mtools for the blank card. It writes the boot assets for composing release apps to
`public/boot/`, and the turbo overrides and a compressed blank 64 MiB card to
`src-tauri/resources/`, which the Rust binary embeds.

Checks (after `sh scripts/prepare-resources.sh`, which the Rust build needs):

```sh
pnpm exec tsc --noEmit                         # front end
cargo test --manifest-path src-tauri/Cargo.toml  # display reader, locks, heaps, SD card
```

`pnpm dev` alone serves the front end in a browser at
http://localhost:1420 with mock content, for layout work without Rust;
`?demo` shows the same mock content inside the app.

## Behaviour

Device, panel and the three switches persist in `settings.json`, with the
chosen firmware and SD card, in the app's config directory. Theme and zoom
stay in the WebView's storage, as on the page, with the desktop's panel
widths; that storage is shared by every instance. Choosing a
firmware starts it, as in the browser; changing Device or the X4 Pro panel
restarts it.

The SD card is an image file that QEMU opens directly: `sd.img` in the app's
data directory unless another card is opened from the card menu, which also
makes new blank cards (64 MiB FAT32, 512-byte clusters; see the repository
README's SD image geometry section before timing comparisons). The explorer
edits the image in Rust with the `fatfs` crate. While the emulator runs, each
operation pauses the guest; a change resets the device so the firmware mounts
the card again. Uploads take files from the native dialog, or files and
folders dropped on the panel; hidden entries and symbolic links in folders
are skipped. A card is used by one app instance at a time; an instance that
cannot lock its card opens without one and says so. A saved card that is
missing or invalid, a saved firmware file that is missing, and a settings
file that cannot be read are reported when the window opens; firmware
contents are checked at Start.

The output panel's Heap tab maps the running firmware's ESP-IDF 5.x heaps
block by block: internal DRAM, RTC and PSRAM, each wrapped into rows at a
scale the tab's − and + choose (every heap at the same scale, or Fit), with
new allocations and frees highlighted, requested sizes and corrupt canaries
(with the default light heap poisoning) and fragmentation. A heap's header
folds its map to a single bar; empty heaps (the C3's retention and boot-ROM
heaps, until the main one fills) start folded. Above the maps, a trend shows
internal RAM's free memory, lowest point since boot and largest free block
for the run. Clicking a block opens its details under its map: addresses,
sizes, age and, with allocation stacks, the whole call stack, which can be
copied. It needs the ELF the
firmware was built from, chosen in the tab or dropped on the window, and
remembered in `settings.json`; a rebuilt ELF is read again. The ELF must be
the running app's: the image records its ELF's SHA-256 (PlatformIO and
ESP-IDF builds both write it), and a different ELF is refused with both
builds named. While the tab or the heap window is open, each second Rust
stops the guest, copies the heap regions through QMP `pmemsave` and resumes;
under `-icount` the guest does not see the pause, and the tab shows how long
the read took. "Largest block" is the TLSF block size, slightly above the
firmware's `heap_caps_get_largest_free_block()`.

When the ELF is chosen before Start and matches the firmware, QEMU also loads
the heaptrace plugin (`contrib/plugins/heaptrace.c`), which logs every
allocation and free with its call stack. The tab then colours each block by
the function that allocated it, naming it on hover, and the heap window (the tab's expand button) is
heapview.py's full page: blocks colored by allocation site, live bytes and
churn by site, the newest events, a stack for any block, and a timeline that
replays the heaps to any moment since boot. Stacks are symbolized in Rust from
the ELF's DWARF, so no toolchain is needed. Without the plugin (an ELF chosen
after Start, a firmware without heap poisoning, or a QEMU built without
`--enable-plugins`) the tab says why, and the window shows the heaps alone.

Wi-Fi uses QEMU's user network, so the firmware reaches the internet; turning
it off keeps the radio present without a host connection. Turbo uses the
overrides from `turbo-args.sh`, like the other launchers, but keeps idle time
in real time (`-icount sleep=on`): native QEMU would otherwise run a resting
device through its 10-minute auto-sleep in under a minute at a full CPU core.

### Test hooks

| Variable | Effect |
|---|---|
| `EINK_QEMU_DATA_DIR` | Settings and the default card in this directory instead of the app's, so instances are isolated (card locks stay shared) |
| `EINK_QEMU_AUTOSTART=1` | Start the saved firmware as soon as the window loads |
| `EINK_QEMU_PANE` | Open the output panel on this tab: `console` or `heap`; `window` also opens the heap window |
| `EINK_QEMU_LOG` | Also append console lines, including the window's own errors, to this file |
| `EINK_QEMU_SCREEN_DUMP` | Write the panel as a binary PGM to this file after every update |
| `EINK_QEMU_HEAP_DUMP` | Write every heap snapshot, with the allocation log's events, as JSON to this file |
| `EINK_QEMU_BIN`, `EINK_QEMU_SHARE` | QEMU binaries and `-L` data directory |
| `EINK_QEMU_PLUGINS` | Directory with the heaptrace plugin (default: `contrib/plugins` beside the binaries, or the package's `qemu/plugins`) |

`scripts/capture-window.sh out.png [pid]` saves a PNG of the window on macOS
without raising it.

## Packaging

The app ships QEMU inside its resource directory and, in a packaged build,
runs `<resources>/qemu/bin/qemu-system-{riscv32,xtensa}` with
`-L <resources>/qemu/share`. Stage that directory, then build with the config
fragment that adds it to the bundle:

```sh
# from the repository root: build QEMU first (EINK-QEMU.md), then
cd eink-qemu/desktop
scripts/bundle-qemu.sh            # writes src-tauri/qemu (about 48 MiB)
pnpm tauri build --config src-tauri/tauri.bundle.conf.json
```

`scripts/bundle-qemu.sh` takes the binaries from `<repo>/build`
(`EINK_QEMU_BUILD` selects another build directory) and produces:

```
src-tauri/qemu/
  COPYING                       QEMU's licence (GPL-2.0)
  bin/qemu-system-riscv32       X3, X4 (ESP32-C3)
  bin/qemu-system-xtensa        X4 Pro, Picco (ESP32-S3)
  lib/*.dylib                   every non-system library, recursively
  share/esp32c3-rom.bin         mask ROMs found through qemu_find_file()
  share/esp32s3_rev0_rom.bin
```

Library references are rewritten to `@loader_path/../lib` (binaries) and
`@loader_path` (libraries), absolute rpaths are removed, and every file is
ad-hoc signed again, since `install_name_tool` invalidates signatures and
arm64 refuses unsigned code. The script fails if any reference still points
outside the bundle or the system.

Homebrew's `sdl2-compat` provides `libSDL2` and opens SDL3 from its load-time
initializer, trying `@loader_path/libSDL3.dylib` first. QEMU links SDL2 even
with `-display eink`, so SDL3 is bundled under that name. Without it the
process blocks before `main` on an invisible modal "Failed loading SDL3
library" alert.

Verified on macOS arm64: a copy of the directory, run from another location
with `env -i` and `DYLD_PRINT_LIBRARIES`, boots CrossPoint 1.6.5 on `x4pro`
and `x3` and loads all 19 libraries from `qemu/lib` and none from
`/opt/homebrew`. `tauri build` with the fragment produces a working `.app`
and `.dmg`.

### Windows

The Windows QEMU is a MinGW build: the repository CI's
`dist-qemu-*-x86_64-w64-mingw32` archives (static), or a cross build such as
QEMU's `tests/docker/dockerfiles/fedora-win64-cross.docker` with
`--target-list=xtensa-softmmu,riscv32-softmmu --enable-pixman --enable-gcrypt
--enable-slirp`. `scripts/bundle-qemu-windows.sh <install dir>` stages it
into `src-tauri/qemu` the same way, with the DLLs beside the `.exe` files
(Windows finds them there). It strips debug information and, with
`llvm-objdump` or `x86_64-w64-mingw32-objdump`, fails if an imported DLL is
neither bundled nor part of Windows.

On Windows, `pnpm tauri build --config src-tauri/tauri.bundle.conf.json`
builds the NSIS and MSI installers. From macOS or Linux the NSIS installer
can be cross-built with `cargo-xwin`, LLVM (`llvm-rc`, `lld`) and NSIS
(`brew install llvm lld nsis`, `cargo install --locked cargo-xwin`):

```sh
scripts/bundle-qemu-windows.sh /path/to/windows/qemu
pnpm tauri build --runner cargo-xwin --target x86_64-pc-windows-msvc \
  --bundles nsis --config src-tauri/tauri.bundle.conf.json
```

The app links the C runtime statically and needs no Visual C++
redistributable; the installer fetches WebView2 when Windows lacks it.
Installers are unsigned, so SmartScreen warns on first run.

Verified so far without Windows itself: the Windows QEMU (Fedora 43 MinGW
cross build), run under Wine 10 from the installer's layout in
`C:\Program Files`, connects to the app's loopback sockets, boots CrossPoint
1.6.5 to Home on `x3` and `x4pro` (Turbo included), and takes keys, `stop`,
`cont` and `quit`; the Rust unit and SD tests pass there except the symbolic
link test (Wine). The app's window, dialogs and drops have not run on
Windows yet.

### Distribution notes

- **Signing:** ad-hoc signatures run locally but do not pass Gatekeeper on
  other machines. For a Developer ID build, sign every file in `qemu/bin` and
  `qemu/lib` with the same identity before `tauri build`. The hardened runtime
  needs `com.apple.security.cs.allow-jit`, because TCG generates code into
  `MAP_JIT` memory. HVF is not used, so `com.apple.security.hypervisor` is not
  needed.
- **Licences:** the bundle carries GPL and LGPL libraries (QEMU, glib,
  libgcrypt, libslirp, pixman, SDL, libX11 and others). A release should add
  their licence files and a source offer next to `COPYING`.
- **Size:** the libX11/xcb chain, libusb and SDL come from the build's
  configure options (`--enable-sdl`). A QEMU configured with `--disable-sdl`
  for the app would drop SDL2, SDL3 and possibly X11.
- **Linux:** collect `ldd` dependencies outside the base system into
  `qemu/lib`, set `RUNPATH` to `$ORIGIN/../lib` with `patchelf`, or build QEMU
  with fewer dependencies. Not implemented.
