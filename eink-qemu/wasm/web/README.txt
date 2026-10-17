eink-qemu browser emulator

Build and serve from the repository root:
  eink-qemu/wasm/build-deps.sh
  eink-qemu/wasm/build.sh --web
  python3 eink-qemu/wasm/serve.py build-wasm-32limit/web-dist

If the wasm binaries already exist, package-web.sh assembles the distribution
without rebuilding. WASM64_MODE=32 is the default (MEMORY64=2: 64-bit host
pointers lowered to 32-bit Wasm memory). WASM64_MODE=64 builds native Memory64
in build-wasm/; set the same mode when packaging existing binaries. The build
uses the wasm64 JIT backend, with TCI for cold blocks. It includes both
architectures, the two ROM files, and a blank 64 MiB MBR/FAT32 SD image generated
by mksd.py with an explicit --cluster-size 512 override. This compact card keeps
browser memory and download sizes small; it is not suitable for stock-X3
filesystem timing comparisons. The stock X3 SD card uses 32 KiB clusters,
which is mksd.py's default and requires an image of at least 2050 MiB.
See the repository README's "SD image geometry" section before timing work.
No runtime CDN or npm
packages are used; the page's fonts (Inter, Lora and Geist Mono latin
subsets) are bundled under the OFL, see fonts.LICENSE.txt. Chrome 155 is verified with GPU/WebGL enabled. Native
Memory64 is also available in this Chrome version without experimental flags;
older browsers may require MEMORY64=2. Firefox and Safari are not verified.

The page is the desktop app's front end built for the browser
(eink-qemu/desktop, `pnpm build:browser`; package-web.sh runs it). This
directory holds what the page loads: the runtime iframe (runtime.html,
runtime.js), the FAT explorer (fat.js, sd-ops.js, sd-store.js), the stylesheet,
fonts and marks, and the firmware modules. To work on the page against a
packaged web-dist: `cd eink-qemu/desktop && pnpm dev:browser` serves it at
http://localhost:1421/ with the isolation headers (EINK_WEB_DIST names the
package; the default is build-wasm-32limit/web-dist).

Open http://127.0.0.1:8000 and choose the firmware (the chip at the top of the
page, or the button on the empty screen): any ESP32-C3 or ESP32-S3 app
.bin, merged firmware .bin, an X4 Pro .xota update, or a 16 MiB flash dump;
choosing the firmware starts the emulator (Start reruns it after Stop). The
firmware card shows the file, the devices that take its chip, the X4 Pro's
display panel and, in the desktop app, the ELF for the Heap tab; the browser
has no Heap tab, since reading the guest's memory needs native QEMU. Wi-Fi,
Ghosting and Turbo are the icon toggles beside Start. CrossPoint Reader's
release assets, for example:
  X3/X4:  crosspoint-<ver>-x3-x4.bin
  X4 Pro: crosspoint-<ver>-x4pro.bin
  https://github.com/crosspoint-reader/crosspoint-reader/releases
The browser composes app images into 16 MiB flash using bundled C3/S3
bootloaders and the Arduino-ESP32 16 MB partition table (the Picco's own
layout on the Picco). CrossPoint's NVS entry, its cached X3/X4 detection, is
added only to X3 apps that carry its strings (eink-qemu/profiles/crosspoint.json).
App bytes stay unchanged. No esptool or PlatformIO is needed. Boot asset
provenance and licenses are in eink-qemu/boot/README.md (boot-provenance.md in
the package). Unsupported chips, invalid headers and apps exceeding the selected board partition
are rejected. Full 16 MiB images pass through unchanged.
The hosted copy is https://serialx.github.io/eink-qemu/ (no firmware included).
X4 Pro defaults to UC8279 revision 0x68. Use ?turbo=0 for measured timing.
Select UC8179 under Display panel in the firmware card, or use ?panel=uc8179, for the older panel.
Saved panel choices take precedence; this selection does not affect other boards.
Auto detects the chip, not the panel revision.
UC8279-0x68 has measured panel waits but has not had its optical ghosting fitted.
Ghosting starts ticked. Untick it before Start or use ?ghosting=0 for clean,
stable pixels in automated QA. The setting applies each time the emulator
starts. Ghosting off removes physical ink artifacts and idle drift while
preserving LUT grayscale, animation and refresh timing. Capture after refresh
completion. Native and Node runs default to ghosting off, so use ghosting=0
when comparing browser captures with their default output.

Turbo (fast, not timing-accurate) is the page default and starts ticked;
untick it or open with ?turbo=0 for accurate device timing, as in smoke.sh.
?turbo=1 remains accepted. Both modes appear in the status line.
Alternatively put copies of your images in web-dist and open:
  http://127.0.0.1:8000/?flash=flash.bin&sd=sd.img
  http://127.0.0.1:8000/?flash=crosspoint-1.6.5-x3-x4.bin&sd=sd.img
Query images must be same-origin or provide appropriate CORS/CORP headers.
The device is remembered between visits. Auto selects X4 Pro for ESP32-S3;
ESP32-C3 requires an explicit X3 or X4 choice in the firmware card because
both use the same chip, and the card offers only the devices that take the
firmware's chip. Use ?device=x3, ?device=x4, ?device=x4pro or ?device=picco to
select a board in a link; a device/chip mismatch is rejected before boot.
Wi-Fi, Ghosting and Turbo are remembered too; ?wifi=off, ?ghosting=0 and
?turbo=0 override them for one visit.

For TRMNL FW1.8.17.bin, choose X4 and load the merged .bin directly. Its
bootloader, partitions and firmware bytes are preserved. The SSD1677 panel
shows the setup QR code and Wi-Fi instructions. X4 has simplified refresh
timing and no ghosting simulation. The display stays in physical portrait
orientation; firmware such as TRMNL may draw its interface in landscape.

For BOOX Picco, select BOOX Picco in the firmware card before loading
picco1.0.2920.bin, or use ?device=picco. Its app is copied unchanged to 0xE0000 using Picco partitions;
the app limit is 0xE70000 bytes. The recovery slot stays erased. Controls are
Up, Down, Power and touch. Frontlight brightness and touch firmware updates
are not simulated.
Picco keeps its guest clock paced even in Turbo mode so clicks retain their
duration. Turbo still speeds up peripheral timing; ?sleep=off disables pacing
for benchmarks.

The server sets COOP and COEP for SharedArrayBuffer and pthreads. For a remote
host use HTTPS and the same headers; plain HTTP works only on localhost. A
vendored coi-serviceworker 0.1.7 supplies isolation on hosts such as GitHub
Pages that cannot set these headers, reloading once on first visit. It requires
HTTPS or localhost and permission to register service workers. The local server
binds only to 127.0.0.1 by default and needs no worker.

coi-serviceworker.js is unchanged from gzuidhof/coi-serviceworker commit
7b1d2a092d0d2dd2b7270b6f12f13605de26f214; its MIT license is included in
coi-serviceworker.LICENSE.txt. No CDN is contacted at runtime.

The Emscripten SDL2 port renders the device's portrait panel. The runtime runs
QEMU main on a pthread; the page remains responsive. An Emscripten-only guard
yields in SDL event polling so queued browser input reaches the application
pthread even while the e-ink screen is idle. The default parallel text console
is disabled because Emscripten SDL has one canvas. Browser Wi-Fi networking is
enabled by default; see the Wi-Fi section below for downloads and limitations.

The canvas and runtime iframe retain the native panel resolution: 528x792 for
X3 and 480x800 for X4/X4 Pro/Picco. The page scales the iframe in whole physical-pixel
multiples, adapting to browser zoom, monitor pixel density, and available width.
On a 2x Retina display at 100% zoom, each panel pixel occupies a 2x2 block.
If a narrow window cannot fit even one physical pixel per panel pixel, the
device section scrolls horizontally. Avoid percentage sizing on the canvas:
SDL adopts that size and resamples the framebuffer, producing moire in dithering.

Use the on-screen controls or arrows, Enter, Backspace/Esc for Back on X3/X4,
and P for Power. X4 Pro has a Home control (keyboard: Esc, H or Home) and
accepts pointer/touch input on its canvas. Picco has no Home key. P holds Power
while the key is down: hold it for a long press, which many firmwares use to
sleep or power off. The action and required hold time depend on the firmware.
Reset resets the machine. Stop pauses
the guest, copies the current SD image, and removes the iframe and its workers.
Start then boots again with that SD image. "Save a copy of the SD card" pauses
the guest, copies the card, and resumes execution. The SD card is saved in this browser
and restored on refresh or your next visit to the same site. File explorer
changes save immediately; running device changes save every 10 seconds and
when you press Stop or Download SD image. Wait for the saved status before
refreshing. A refresh during execution can lose changes since the last save;
Stop saves the current card. Flash changes are not exported or saved.

Browser storage is local to this browser profile and site address. Clearing
site data removes the saved card, and the browser may evict it when storage
is scarce. Save a copy for a portable backup. If storage is unavailable
or full, a warning appears and the card remains usable in memory. Opening
another SD image, or a new blank card, from the card menu (the card button in
the SD panel) replaces the saved card. An explicit ?sd= URL loads that image
instead of restoring the saved card, and saves it as the new card.
The full image is stored atomically in IndexedDB, separately for each hosted
application path, and numbered with each save. Tabs of the page share the
card: an idle tab takes up another tab's save, and a tab whose card is older
than the saved one (one running firmware, say) stops saving and says so, so
it never overwrites the newer card. Download its card to keep its changes.
The whole card lives in memory, so the page takes SD images up to 2 GiB.

The last six firmware files chosen are kept in this browser too: after a
reload Start runs the last one, and the firmware card lists the others.

The SD card panel is a file explorer for the in-memory card: browse folders,
upload files (picker or drag-and-drop, folders included), create folders,
rename, delete, and download single files. It works before the first Start
(the blank card or the picked image loads as soon as the page opens), while the
emulator runs, and after Stop. fat.js (plain JavaScript, no dependencies)
edits the FAT32 image in place; exFAT, FAT12 and FAT16 cards are reported as
unsupported and the explorer is disabled, but the emulator still boots them.
While the emulator runs, each operation pauses the guest through the HMP
monitor, applies the change to /sd.img inside the runtime iframe, and then
resets the device, because the firmware caches FAT and directory sectors and
has to mount the modified card again. Listing and single-file downloads only
pause and resume. Uploaded names are normalised to NFC. fat.test.mjs
exercises fat.js under Node against dosfstools and mtools (mkfs.vfat, fsck.fat,
mcopy, mdir): node eink-qemu/wasm/web/fat.test.mjs.

Images occupy browser memory at their full logical size: a sparse 1 GiB file
still needs 1 GiB in the browser, and exporting needs another copy. A 64 MiB
card is a useful starting point. A first boot with books can spend substantial
time generating cover thumbnails after Home first appears. The status remains
'finishing startup' until the CrossPoint memory log checkpoint used by smoke.sh.
Firmware without a profile in eink-qemu/profiles is Ready once its screen has
been quiet for 5 s (as in the CLI), unless the ROM keeps rebooting it.

TRMNL X4 acceptance uses the same server and isolated Chrome setup below:
  TRMNL_FIRMWARE=/absolute/path/FW1.8.17.bin \
    node eink-qemu/wasm/web/trmnl-smoke.mjs http://127.0.0.1:8000/
It verifies C3 ambiguity, chip mismatch rejection, unchanged merged firmware,
480x800 output, Reset, Stop and remembered selection. Optional REFERENCE_PPM
compares the panel bytes against a native X4 capture with
`-nic hubport,hubid=0,model=esp32_wifi`. This keeps the radio present without a
host connection, matching the browser with Wi-Fi downloads off.

The acceptance tests below drive the page's own controls. File pickers are
hidden inputs the page waits on once a button asks for a file (#flash-file
after Choose… in the firmware card, #sd-file after "Open SD image…" in the
card menu, #sd-files after Upload), which the tests set through DevTools.
window.einkQemu exposes the explorer's operations and the card for them, and
window.bootTimeSeconds the measured boot.

Automated acceptance (Node 22+ and Chrome; no npm dependencies):
  # In separate terminals, start serve.py as above and Chrome:
  "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" \
    --headless=new --remote-debugging-port=9224 \
    --user-data-dir=/tmp/eink-qemu-chrome --no-first-run about:blank
  INPUT_SD=/absolute/path/to/sd.img node eink-qemu/wasm/web/smoke.mjs \
    'http://127.0.0.1:8000/?flash=flash.bin&sd=sd.img'

SD persistence acceptance uses the same server and an isolated Chrome profile
(it replaces that profile's saved card):
  PICKER_FLASH=/absolute/path/to/crosspoint-1.6.5-x3-x4.bin \
    node eink-qemu/wasm/web/persistence-smoke.mjs http://127.0.0.1:8000/
It checks reloads after edits, failed saves, unavailable storage, running
explorer edits, periodic device snapshots, Stop, and image replacement.
Omit PICKER_FLASH to run only the checks that do not boot firmware.

Official release acceptance also compares the browser-composed buffer's SHA256
(using crypto.subtle before transfer to QEMU) with mkflash.py output:
  PICKER_FLASH=/absolute/path/to/crosspoint-1.6.5-x3-x4.bin \
    PICKER_SD=/absolute/path/to/sd.img INPUT_SD=/absolute/path/to/sd.img \
    node eink-qemu/wasm/web/smoke.mjs 'http://127.0.0.1:8000/'
For a release app URL, set RELEASE_APP to the same file's local path:
  RELEASE_APP=/absolute/path/to/crosspoint-1.6.5-x3-x4.bin \
    INPUT_SD=/absolute/path/to/sd.img node eink-qemu/wasm/web/smoke.mjs \
    'http://127.0.0.1:8000/?flash=crosspoint-1.6.5-x3-x4.bin&sd=sd.img'
Python 3 is required for this comparison; no Python packages are needed.
Remove private firmware copies from web-dist after testing.

With file pickers, omit the query string and set PICKER_FLASH and PICKER_SD
to local paths; this tests turbo, the page default. Repeat with ?turbo=0
to test accurate timing. ?turbo=1 remains accepted; ?sleep=off gives deterministic
accurate guest timestamps for comparison with RUN_NODE_SLEEP=off in Node.
INPUT_SD must name the same source card. Omit PICKER_SD to test
the packaged blank card, using its blank-sd.img path for INPUT_SD.

The test waits for the third completed X3_DRF refresh and the settled [MEM]
checkpoint, saves web-home.png, presses Down and requires the canvas to change,
saves web-down.png, exercises keyboard Up, Reset and Stop, and checks that the
SD download differs from the input. WebGL must be available; do not disable
the GPU in Chrome acceptance runs. Evidence defaults to build-wasm/evidence. Set
INPUT_SD to the input card's local path to verify the download contains guest
writes, CHROME_PID to the isolated browser's main PID for sampled aggregate CPU/RSS,
CDP_PORT to use another debug port, and TIMEOUT to change the 600-second limit.
WEB_MACHINE=x4pro detects an ESP32-S3 firmware header (chip ID 9 at the
bootloader or app offset) and waits for the third 8179_DRF refresh plus the
following [MEM] checkpoint. It saves web-x4pro-home.png and web-x4pro.ppm,
checks Down and keyboard Up change the panel, and taps the Library row on the
blank-card Home screen. It then checks that the Home control and keyboard Home
(with page or canvas focus) return to the same Home screen. The touch capture
is web-x4pro.png; the return capture is web-x4pro-home-control.png. Stop must remove
the runtime. Other firmware needs equivalent CrossPoint log milestones.
A blank 16 MiB flash still tests only ROM boot and pointer delivery.
Both device tests also require native canvas and iframe dimensions at pixel
ratios 1, 1.25, 1.5, 2, and 3 across desktop and narrow viewports, with whole
physical-pixel display scales when the screen size is set to Auto (Fit, the
default, fills the available space and may resample). The X4 Pro touch check runs at fractional DPR.

Test the official X4 Pro app directly through the picker with the blank card:
  WEB_MACHINE=x4pro PICKER_FLASH=/absolute/path/to/crosspoint-1.6.5-x4pro.bin \
    node eink-qemu/wasm/web/smoke.mjs 'http://127.0.0.1:8000/'
This also checks composition against mkflash.py. For a URL, set RELEASE_APP
to the local app and use ?flash=crosspoint-1.6.5-x4pro.bin&sd=blank-sd.img.

Node takes a composed flash image:
  python3 eink-qemu/mkflash.py crosspoint-1.6.5-x4pro.bin /tmp/x4pro.bin
  eink-qemu/wasm/smoke.sh /tmp/x4pro.bin build-wasm-32limit/web-dist/blank-sd.img

For Node, smoke.sh /tmp/x4pro.bin sd.img detects the same chip and tests X4 Pro
Home, settled [MEM], JIT activity and a nonblank 480x800 panel. --turbo also
compares against a fresh native turbo capture. --rom-only and X3 firmware
smoke retain their ROM and X3 coverage. Firmware is never included in the
package; remove temporary firmware copies before distributing web-dist.

For header-less static hosting, run:
  python3 -m http.server 8001 --bind 127.0.0.1 \
    --directory build-wasm-32limit/web-dist
Then repeat the smoke command using port 8001 and EXPECT_SERVICE_WORKER=1.
The test requires a controlling service worker and cross-origin isolation before boot. Page-load-to-Home timing includes
the initial reload; home_milestone_s is measured from image loading after reload.

Wi-Fi / downloads
-----------------
Leave the Wi-Fi switch on and join PICSimLabWifi from the firmware's own
Wi-Fi settings. Downloads run entirely in the browser; no relay is needed.
A package may bundle a wifi-assets directory (by default CrossPoint Reader's
font catalog, for its Settings > Reader > Manage Fonts flow) whose files
answer requests for their upstream URLs; without it, every download goes
through the browser's fetch. Files the firmware saves stay on the SD card in
this browser. Use Download SD image to export a copy. ?wifi=off disables Wi-Fi.

The default catalog contains Alef, Literata, Noto Sans Extended, and Pretendard.
To change it, run prepare-wifi-assets.py with repeated --family arguments, then
package-web.sh. Font license notices are in wifi-assets/licenses. lwIP and
mbedTLS notices are included alongside the emulator.

This is an HTTP download adapter, not general internet access. Guest HTTPS
terminates locally with an untrusted certificate; firmware that does not
verify certificates, such as CrossPoint 1.6.5's wolfSSL downloader, accepts it
without changes. Firmware requiring a trusted
certificate will reject it. Other GET/HEAD downloads depend on the source's
CORS policy. Uploads, incoming servers, and arbitrary TCP/UDP are unsupported.

See eink-qemu/wasm/README.md in the source tree for preparation and Node usage.
