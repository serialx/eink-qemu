# eink-qemu desktop: design

An Electron app with the browser page's interface (SD card explorer, device
stage, firmware console) that runs the native `qemu-system-*` binaries instead
of the WebAssembly build. Native QEMU is several times faster than the Wasm
JIT, has no fixed heap, and reads the SD card image from disk at any size.

## Stack

Electron, electron-vite, React, TypeScript, Tailwind v4, shadcn/ui (Base UI
primitives), pnpm. The package lives in `eink-qemu/desktop/` and imports the
browser page's dependency-free modules (`fat.js`, `sd-ops.js`, `xota.js`,
`flash-image.js`) from `../wasm/web` so both front ends share one copy.

## Processes

```
main process                              renderer (React)
EmulatorSession                    IPC    App state
  spawns qemu-system-{riscv32,xtensa}  <->  TopBar, SdExplorer, DeviceStage,
  QMP client       (socket, JSON)            Console, StatusBar
  display server   (socket, ui/eink.c)       <canvas> at panel resolution
  stdout           (USB-CDC log)
SdCard  (fat.js on the image file)
Firmware (compose release apps, .xota)
```

Electron listens on both sockets before it spawns QEMU, and QEMU connects as a
client, so there is no wait for a socket file to appear. Unix sockets on macOS
and Linux; loopback TCP on Windows.

## Display: `-display eink,addr=<socket>`

`ui/eink.c` is a 2D `DisplayChangeListener` on the console of the device with
qdev id `panel` (the UC8179, UC8279 or SSD1677; `device=ID` picks another).
The boards also realize Espressif's RGB framebuffer, so the first graphic
console is not always the panel. The panels rasterise their ink state,
ghosting included, into a 32 bpp surface on each GUI refresh and report the
whole surface as dirty. The backend forwards those pixels over one stream.

Records are little-endian: a 4-byte type, a 4-byte payload length, the payload.

| Type | Payload | When |
|---|---|---|
| 1 `HELLO` | `"EINK"`, u32 version (1), u32 width, u32 height | on connect |
| 2 `RESIZE` | u32 width, u32 height | surface switch |
| 3 `RECT` | u32 x, y, w, h, then `w*h` gray8 pixels | dirty rectangle |
| 4 `FLUSH` | none | end of a refresh that sent rectangles |

Pixels are 8-bit gray: the panels write R = G = B. The green channel is sent.
At most one update is queued: while the front end reads, dirty areas
accumulate, and the next update sends their bounding box with the newest
pixels, so the guest never waits for the UI. Closing the connection shuts
QEMU down, like closing its window (`window-close=off` keeps it running).
Screenshots are taken from the canvas, as in the browser.

Input does not go through the display. `x3-keys`, `x4pro-keys` (also Picco)
and the GT911 touch panel are QEMU input handlers without routing, reached by
QMP `input-send-event`: keys as qcodes (`up`, `down`, `left`, `right`, `ret`,
`esc`, `p`), touch as absolute axes scaled to 0..32767 with the left button
as the finger and the right button as the X4 Pro Home key.

## Control: QMP

`stop`, `cont`, `system_reset`, `input-send-event`, `query-status`, and the
`STOP`, `RESUME`, `RESET` and `SHUTDOWN` events. The firmware console is the
USB-CDC chardev on stdout, as in `run.sh`.

## SD card

The card is an image file on disk that QEMU opens directly; there is no
autosave and no IndexedDB. Explorer edits keep the browser's sequence: QMP
`stop`, edit the image with `fat.js`, `system_reset` (the firmware caches FAT
and directory sectors), `cont`. Listing and downloads only stop and resume.

## Firmware

Release apps and X4 Pro `.xota` updates are composed into 16 MiB flash with
`flash-image.js` and the bundled boot assets, as in the browser; merged
images and full dumps pass through. QEMU runs a temporary copy, because it
writes NVS and OTA data to flash.

## Interface

The browser page's layout and look: stone neutrals, one forest-green accent,
Inter, Lora and Geist Mono. The three panels are `Resizable`; controls map to
shadcn components (`Select`, `Switch`, `Popover`, `Button`, `Breadcrumb`,
`Table`, `Progress`, `Dialog`, `AlertDialog`, `Empty`, `Badge`, `sonner`).
Screen sizing keeps the browser's whole-physical-pixel zoom steps.

## Order of work

1. `ui/eink.c` and its QAPI entry; a script that dumps the first frame.
2. Scaffold, theme and static layout.
3. EmulatorSession: spawn, QMP, display, console, input.
4. SD explorer on the image file; firmware composition; status milestones.
5. Packaging with electron-builder and the release `bin/` archives.

Follow-up: the turbo overrides exist in `turbo-args.sh` and `runtime.js`; the
desktop app should read `turbo-args.sh` rather than add a third copy.
