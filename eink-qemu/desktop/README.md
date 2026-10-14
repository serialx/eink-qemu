# eink-qemu desktop

A Tauri app with the browser page's interface that runs native QEMU: SD card
explorer, device stage and firmware console. The interface is the page
itself: `src/desktop.css` imports `wasm/web/style.css`, and the fonts, marks
and firmware modules come from the same directory. Design notes are in
[DESIGN.md](DESIGN.md).

## Run

Build QEMU first ([EINK-QEMU.md](../../EINK-QEMU.md#build-and-run)); in
development the app runs `build/qemu-system-riscv32` and
`build/qemu-system-xtensa` from the repository root, or the directory in
`EINK_QEMU_BIN`. QEMU must include the `eink` display (`ui/eink.c`). Then,
with Node 22+, pnpm and Rust 1.89+:

```sh
cd eink-qemu/desktop
pnpm install
pnpm tauri dev
```

`pnpm dev` and `pnpm build` first run `scripts/prepare-resources.sh`, which
needs Python 3. It writes the boot assets for composing release apps to
`public/boot/`, and the turbo overrides and a compressed blank 64 MiB card to
`src-tauri/resources/`, which the Rust binary embeds.

Checks:

```sh
pnpm exec tsc --noEmit                         # front end
cargo test --manifest-path src-tauri/Cargo.toml  # display reader, SD card
```

`pnpm dev` alone serves the front end in a browser at
http://localhost:1420 with mock content, for layout work without Rust.

## Behaviour

Device, panel and the three switches persist in `settings.json`, with the
chosen firmware and SD card, in the app's config directory. Theme, zoom and
panel widths stay in the WebView's storage, as on the page. Choosing a
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
cannot lock its card opens without one and says so. A saved card or firmware
that is missing or invalid is reported when the window opens.

Wi-Fi uses QEMU's user network, so the firmware reaches the internet; turning
it off keeps the radio present without a host connection. Turbo uses the
overrides from `turbo-args.sh`, like the other launchers.

### Test hooks

| Variable | Effect |
|---|---|
| `EINK_QEMU_DATA_DIR` | Settings and the default card in this directory instead of the app's, so instances are isolated (card locks stay shared) |
| `EINK_QEMU_AUTOSTART=1` | Start the saved firmware as soon as the window loads |
| `EINK_QEMU_LOG` | Also append console lines, including the window's own errors, to this file |
| `EINK_QEMU_SCREEN_DUMP` | Write the panel as a binary PGM to this file after every update |
| `EINK_QEMU_BIN`, `EINK_QEMU_SHARE` | QEMU binaries and `-L` data directory |

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
`/opt/homebrew`. The full `tauri build` with the fragment has not been run yet.

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
- **Windows:** the MSYS2/MinGW build needs its DLLs next to the `.exe` in
  `qemu/bin`; `ntldd -R` lists them. Not implemented.
