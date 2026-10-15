# eink-qemu desktop: design

A Tauri 2 app with the browser page's interface (SD card explorer, device
stage, firmware console) and a live heap view, that runs the native `qemu-system-*` binaries
instead of the WebAssembly build. Native QEMU is several times faster than
the Wasm JIT, has no fixed heap, and reads the SD card image from disk at any
size.

It replaces an Electron attempt (branch `serialx/electron-app`) whose
renderer re-created the page with shadcn/ui. That copied the page's colours
but not its look: shadcn's radius scale, control heights, focus rings and
switches made every control a different shape. This app uses the page's own
stylesheet and markup instead.

## Stack

Tauri 2, Rust (tokio, fatfs), Vite, TypeScript without a UI framework, pnpm.
The package lives in `eink-qemu/desktop/`: `src/` is the WebView, `src-tauri/`
the Rust core.

## Theme and layout

- `src/desktop.css` starts with `@import '@web/style.css'` (a Vite alias to
  `../wasm/web`): the stone neutrals, the forest green, Inter, Lora and Geist
  Mono, the controls, the green status bar, the dotted stage and the bezel
  are the site's bytes. Fonts, the cow mark and the firmware modules come
  from the same directory.
- `index.html` keeps the page's markup and class names. The desktop overlay
  adds a canvas where the page had its runtime iframe, buttons where it had
  file inputs (Tauri works with paths from native dialogs and drops), gutters
  that set the grid's panel widths, the card menu, in-page prompt and confirm
  dialogs (`window.prompt` is not available in every system WebView), and a
  traffic-light inset with drag regions for the macOS overlay title bar.
- The window's minimum size is 1000×640, so the stylesheet's phone layout
  never engages.
- `src/app.ts` is the page's `app.js` with the runtime iframe replaced by
  Rust commands: screen zoom (whole physical pixels, Fit, Auto), key rows,
  status milestones and the SD table carry over. The console buffer and
  filter are in `console.ts`; the Heap tab in `heap.ts`; help tips, the
  theme toggle and the panel gutters in `main.ts`.
- Deliberate differences from the page: status milestones also recognise
  the X4's plain `refresh` waits, restart counting when the guest reboots,
  name an SD card the firmware could not mount and the stock X4 Pro
  firmware's wait for a Power press; device keys are released by their own
  key-up whatever the modifiers; the SD row action saves to the computer
  through a save dialog ("Save to computer"); Turbo keeps idle time in real
  time (see the README).
- Without the Tauri runtime (`pnpm dev` in a browser) the page shows mock
  content from `src/demo.ts`, for layout work.

## Processes

```
src-tauri (Rust, tokio)                         WebView (TypeScript)
Session                               invoke    app.ts
  spawns qemu-system-{riscv32,xtensa}  <----->    topbar, SD explorer, stage,
  QMP client     (socket, JSON)        channels   console, status bar
  display reader (socket, ui/eink.c)   ------->   <canvas> at panel resolution
  stdout/stderr  (USB-CDC log)         ------->   console, batched lines
sd.rs  (fatfs on the image file)
```

Rust listens on both sockets before spawning QEMU, and QEMU connects as a
client, so there is no wait for a socket file. Unix sockets on macOS and
Linux, in a private (0700) run directory under `/tmp` so socket paths stay
short; the flash copy in it is 0600. Windows uses loopback TCP, which does
not authenticate the peer: another local user could race QEMU to connect
during startup. Startup succeeds once QMP has negotiated and the display has
sent its first size; it fails after 30 seconds, when QEMU exits, or when
Stop cancels it. Any later loss of QEMU, the monitor or the display ends the
run as crashed. SIGTERM and SIGINT quit the app like the menu does; a run
directory left by a killed app is removed at the next launch.

## Host files

The WebView never names a host path on its own. Native dialogs (firmware,
cards, uploads, saving files and screenshots) run in Rust, and files dropped
on the window are recorded from the window's drop event. Commands that take
a path from the WebView (`firmware_use`, `sd_plan_upload`, `sd_upload`)
accept only paths recorded that way. The capability file grants the WebView
no dialog or file permissions, and the CSP allows no inline script.

## Display: `-display eink,addr=<socket>`

`ui/eink.c` is a 2D `DisplayChangeListener` on the console of the device with
qdev id `panel` (the UC8179, UC8279 or SSD1677). Records are little-endian: a
4-byte type, a 4-byte payload length, the payload.

| Type | Payload | When |
|---|---|---|
| 1 `HELLO` | `"EINK"`, u32 version (1), u32 width, u32 height | on connect |
| 2 `RESIZE` | u32 width, u32 height | surface switch |
| 3 `RECT` | u32 x, y, w, h, then `w*h` gray8 pixels | dirty rectangle |
| 4 `FLUSH` | none | end of a refresh that sent rectangles |

At most one update is queued in QEMU, so the guest never waits for the UI.
`src-tauri/src/display.rs` validates every record (sizes up to 4096², rects
inside the surface, at most 1024 per flush) and sends one raw binary channel
message per flush: `u32 1, width, height` for a size, or `u32 2, count`
followed by each rect. The WebView writes the gray pixels into an
`ImageData` and puts the dirty bounding box. Rust keeps a copy of the panel,
so a reloaded WebView gets the current screen when it attaches.

## Control: QMP

`qmp_capabilities`, `stop`, `cont`, `system_reset`, `input-send-event`,
`qom-set` and `quit`. Keys are qcodes (`esc ret left right up down p`);
the X4 Pro Home key is `qom-set /machine/gt911 home`. Touch on the X4 Pro and
Picco is absolute 0..32767 with the left button as the finger. Key and touch
commands are synchronous Tauri commands that queue onto one task per run, so
a release never overtakes its press.

## SD card

The card is an image file QEMU opens directly. `src-tauri/src/sd.rs` edits
it with the `fatfs` crate (FAT12/16/32, long names, bare or MBR-partitioned
images), streaming file contents rather than loading the card, so stock
2 GiB cards work. Name rules, folder-first ordering and the common error
codes follow `fat.js`; the deviations (generated short aliases, which damaged
cards are refused and with which code, partition and FSInfo handling, upload
of nested dotfiles and empty folders) are listed in `sd.rs`'s header. The
crate is vendored with fixes (`src-tauri/vendor/fatfs/EINK-QEMU.md`) so the
folders it writes pass `fsck_msdos` and `fsck.fat`, which every SD test runs.

While the emulator runs, every operation is bracketed: QMP `stop`, the
operation, then `system_reset` if it may have changed the card (the firmware
caches FAT and directory sectors), then `cont`. Edits are transactional: fatfs
writes into a journal (spilled to a temporary file), which reaches the image
only when the whole operation has succeeded. A failure before that leaves the
card byte-for-byte unchanged; a write failure while committing new data to
free clusters leaves it logically unchanged; only a failure while committing
folders or the FAT can leave it inconsistent, and then the emulator is
stopped rather than reset onto it. Input
sent while the guest is paused waits for it to resume.

One card is used by one app instance at a time. A reservation locks two
files in the user's app data directory, shared by every instance whatever
its data directory: one named after the card's path (so it also covers a
card being created) and one after its identity (device and inode, so hard
links share an owner; volume serial and file index on Windows). Without
its reservation the card is not used at all; the app opens without a card
and says why. Saving a copy, an exported file
or a screenshot reserves its destination for the whole write, so it can
never land on an open card, and the default card is reserved before it is
created. Device input and the pause for an SD operation take one gate, so
no key event reaches a stopped guest.

## Heap view

`src-tauri/src/heap.rs` ports heapview.py's snapshot: the ELF's
`registered_heaps` lists the heaps (`heap_t`, with or without
CONFIG_HEAP_TASK_TRACKING's name pointer), and each region is walked as a
TLSF physical block list, whose start is found once past the TLSF control
structure. A snapshot takes the gate and the stop like an SD operation (and
is skipped with EBUSY while one runs), reads with QMP `pmemsave` into the
run directory, and retries up to eight times when the guest was stopped
mid-update, leaving out a heap that never walks. Before walking, the guest's
`esp_app_desc` (at the ELF's address, through the flash cache) must carry
the ELF's SHA-256, or, when the image has none, the ELF's version, name and
build time. A failed stop or resume ends the run, as for the SD card.
`heap.ts` polls once a second while the tab is open, and keeps the trend and
when each allocation was first seen; the map is heapview.html's, without
zoom, allocation sites or replay.

## Firmware

Release apps and X4 Pro `.xota` updates are composed into 16 MiB flash in the
WebView with the page's `flash-image.js` and `xota.js`, with boot assets
served from `public/boot`; merged images and full dumps pass through after
validating their partition table and application checksums. The composed
image goes to Rust as a raw request body, and QEMU runs a temporary copy,
because it writes NVS and OTA data to flash.

## Packaging

`scripts/bundle-qemu.sh` stages QEMU, its non-system libraries and the two
ESP32 ROMs into `src-tauri/qemu`; `tauri.bundle.conf.json` adds that folder
to the bundle's resources, and a packaged app runs `qemu/bin` with
`-L qemu/share`. `scripts/bundle-qemu-windows.sh` does the same for a MinGW
QEMU, with its DLLs beside the executables. Linux is not packaged yet; see
the README.
