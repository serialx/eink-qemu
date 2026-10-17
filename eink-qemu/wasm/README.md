# WebAssembly JIT scripts

Build and run instructions, both address modes, CI/Pages setup, measurements
and limits are in [EINK-QEMU.md](../../EINK-QEMU.md#webassembly-build).
Browser controls and Chrome automation are in [web/README.txt](web/README.txt).
The page itself is the front end shared with the desktop app
([../desktop](../desktop/README.md)); `web/` holds the runtime iframe, the FAT
explorer, the stylesheet and the modules both front ends import.

From the repository root, with Emscripten 6.0.12 activated:

```sh
eink-qemu/wasm/build-deps.sh
eink-qemu/wasm/build.sh --web
eink-qemu/wasm/run-node.sh flash.bin sd.img
python3 eink-qemu/wasm/serve.py build-wasm-32limit/web-dist
```

The default `WASM64_MODE=32` uses `MEMORY64=2`, lowering 64-bit host pointers
to 32-bit Wasm memory addressing. It needs no browser Memory64 support.
Set `WASM64_MODE=64` for build/run/smoke/package to use native Memory64 in
`build-wasm/` instead. Both modes use the wasm64 JIT, with TCI for cold blocks,
and share `build-wasm-deps/sysroot-wasm64`; the old wasm32 TCI sysroot cannot
be reused. Native Memory64 needs Chrome 133+ or Firefox 134+, not Safari;
the lowered mode is the portable option. Emulator acceptance covers Chrome
155 and Node 26, not Firefox/Safari.

`smoke.sh flash.bin sd.img` detects X3 or X4 Pro and verifies Home, settled
startup, JIT activity and the panel; X3 also checks the X4 Pro blank-flash ROM.
`smoke.sh --rom-only` needs no firmware. Smoke copies both input images;
`run-node.sh` copies flash but writes the supplied SD directly. Use copies.
Evidence defaults to `build-wasm/evidence/smoke` in either mode.
`package-web.sh` packages existing binaries without rebuilding, copies the
C3/S3 bootloaders and licenses, and generates the partition tables and
CrossPoint's NVS entry with the Python helpers in `eink-qemu/`.

The browser accepts any ESP32-C3 or ESP32-S3 firmware through its picker or
`?flash=<url>`: an app `.bin`, a merged `.bin`, a 16 MiB flash dump, or an
official X4 Pro `.xota` update. For example, `crosspoint-<ver>-x3-x4.bin` for
the X3 and `crosspoint-<ver>-x4pro.bin` for the X4 Pro from the
[CrossPoint releases](https://github.com/crosspoint-reader/crosspoint-reader/releases).
App images are composed in browser memory with the bundled boot assets and
the Arduino-ESP32 16 MB partition table (the Picco's own layout on the Picco);
no esptool or PlatformIO is needed. CrossPoint's NVS entry (its cached X3/X4
detection) goes only into C3 apps that carry its strings, as its profile in
[eink-qemu/profiles](../profiles/README.md) says. Merged images and full
flash dumps pass through unchanged.

The page says a boot is finished as the CLI does: firmware with a profile
(CrossPoint's refresh milestones and memory report) by its log, any other
firmware once its screen has been quiet for 5 s, unless the ROM keeps
rebooting it.
The browser smoke test accepts `PICKER_FLASH=<app.bin>` or
`RELEASE_APP=<local-app.bin>` for a URL input, verifies composition status,
and compares SHA256 against `mkflash.py` output before checking Home and controls. Set
`WEB_MACHINE=x4pro` for X4 Pro firmware and its Library touch check. See [web/README.txt](web/README.txt) for complete commands.

`WASM_SYSROOT=/absolute/path/to/sysroot-wasm64` lets both build scripts reuse
compatible dependencies. `WASM_JIT_STATS=1` enables Node JIT module counts;
`RUN_NODE_SLEEP=off` gives deterministic idle timing for comparisons. See the
full guide before changing the fixed 256 MiB heap or 64 MiB translation cache.

Ghosting defaults off in Node and on in the browser. Untick **Ghosting** before
starting or add `?ghosting=0` for browser pixel comparisons against CLI captures.
To enable it in Node, append `-global uc8179.ghosting=on` (X4 Pro) or
`-global uc8279.ghosting=on` (X3) after the flash and SD arguments.
Ghosting off preserves LUT grayscale and refresh timing, but removes physical
ink artifacts and idle drift; capture after refresh completion.

For the X4 Pro UC8279 revision `0x68`, use
`X4PANEL=uc8279-68 eink-qemu/wasm/run-node.sh flash.bin sd.img`, or choose
**X4 Pro panel → UC8279 (0x68)** in the browser. The URL equivalent is
`?panel=uc8279-68&turbo=0`. Panel selection is separate from chip detection;
Auto cannot determine a panel revision from a flash image. The default is
UC8179. See the [calibration report](../x4cal/README.md) before
using timing or ghosting results as hardware predictions.

The browser page defaults to turbo (checkbox ticked); `?turbo=0` or unticking
it selects accurate timing, and `?turbo=1` remains accepted. `run-node.sh` keeps
accurate timing by default; opt in with `TURBO=1 eink-qemu/wasm/run-node.sh
flash.bin sd.img` (`TURBO=0` remains accepted). Turbo keeps `-icount`
with `sleep=off`, minimizes device delays and uses synchronous GPSPI transfers.
It also reduces Wi-Fi RX frame spacing from 5 ms to 0.5 ms, leaving time for
the guest to service receive interrupts. Accurate mode keeps the 5 ms default.
It changes guest timing; `smoke.sh` tests accurate mode and
`smoke.sh --turbo flash.bin sd.img` compares against native turbo without
requiring timestamp equality.

## Measured performance

For native QEMU and Node on Linux x86-64, see the
[amd64 measurements](perf/README.md#amd64-measurements).

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

### What makes it fast

These six changes build on inline file I/O, amortized BQL locking, lock-free
GPSPI CMD reads and direct TB tail chaining from the earlier stack:

- A guarded MMIO word cache avoids repeated dispatch for the polled GPSPI CMD
  register; each read still fetches its live atomic value.
- Host GUI deadlines no longer limit icount execution budgets; BQL handoff
  lets host waiters interrupt a retained single-vCPU lock and service the UI.
- Each JavaScript realm caches its clock origin while reading the live clock.
- Asynchronous Wasm AIO notifier wakeups avoid synchronous JavaScript handoffs.
- Generated translation blocks (TBs) query Asyncify state directly in Wasm.

### Tried and rejected

- Shared browser SD image: the cleaner repeat was slower, with no reproducible win.
- Exact panel kernel specialization: bit-exact work brought no qualifying gain;
  more aggressive variants failed Wasm NaN-state equality.
- Exact busy-poll fast-forward: a timer-publication race could cross a newly
  inserted virtual deadline before its notification reached the vCPU.
- V8/LTO/SIMD flags: earlier screens found startup or throughput regressions,
  or gains too small and noisy to qualify.

### Compatibility

Both address modes have passed build/smoke checks; this timing series uses only
`MEMORY64=2`. The hosted page is <https://serialx.github.io/eink-qemu/>.
Official CrossPoint 1.6.5 X4 Pro now reaches Home in native, Node and Chrome,
including accurate and turbo modes; the browser verifies Down/Up and a pointer
tap into Library. This requires the shared-TB CPU context reload, UC8179 timer
rounding and SDL logical-coordinate fixes in the current source. S3 timer-based
deep-sleep wake now preserves RTC memory and restarts both cores; ext0/ext1
wake and complete boot-latency calibration remain open. X4 Pro uses
`icount shift=0` with measured CPU, SDMMC and SPI defaults. This X4 Pro verification used `MEMORY64=2`;
`MEMORY64=1` and Safari/Firefox were not retested with these fixes.

## Wi-Fi and downloads

The browser gives the firmware an open Wi-Fi network whose HTTP and HTTPS
downloads the page fetches, without a relay, native QEMU, or a running
backend. A package can also bundle static files that answer requests for
their upstream URLs from the page itself: by default CrossPoint Reader's font
catalog, so its **Manage Fonts** downloader works. The bundle is optional;
without it the network still starts and every download goes through the
browser's fetch. Build and serve the static package:

```sh
eink-qemu/wasm/build.sh --web
python3 eink-qemu/wasm/serve.py build-wasm-32limit/web-dist
```

Leave the **Wi-Fi** switch on, start an official CrossPoint 1.6.5
image, and open **Settings > Reader > Manage Fonts**. Join the open
**PICSimLabWifi** network. Installed fonts are saved with the SD card in this
browser. Use **Download SD image** to export a copy.
The package also works on static HTTPS hosting with the existing isolation
service worker. `?wifi=off` disables networking.

Packaging builds a separate wasm32 module using pinned lwIP 2.2.1 and mbedTLS
3.6.7 sources, then downloads the font assets and license notices. Both QEMU
address modes use the same network module. Initial preparation needs internet
access; later packaging reuses the local assets and dependency archives. If
the fonts cannot be prepared, packaging warns and leaves them out;
`WIFI_ASSETS=none` packages none.
Emscripten, CMake, Ninja, OpenSSL, curl, and Python 3 are required to build it.

The default catalog includes Alef, Literata, Noto Sans Extended, and Pretendard.
Only these prepared families appear in the device's download list. To select a
different bundle, prepare it and package again:

```sh
python3 eink-qemu/wasm/prepare-wifi-assets.py build-wasm-deps/wifi-assets \
  --family Alef --family Literata --family SourceSerif4
eink-qemu/wasm/package-web.sh
```

`--catalog` selects a different font format/release manifest. The default is
`sd-fonts-m1-b4`. For families without a built-in license source, supply
`--license-dir /path/to/notices`, containing `<Family>.txt` notices. Each font
is checked against the catalog's size and CRC32; `index.json` records source
URLs and SHA256 hashes. Files stay in ignored build directories. To use another
prepared directory, set `WIFI_ASSETS=/absolute/path` when packaging.
Packaging verifies the prepared files and publishes only the selected catalog
and its licenses. Unselected fonts can remain in the download cache for reuse.

This adapter implements **HTTP downloads**, not general internet networking.
lwIP terminates Ethernet/TCP locally; DHCP and DNS assign the virtual network,
and NTP uses the browser's clock. mbedTLS terminates guest HTTPS in the page.
JavaScript maps prepared font URLs to static files and streams the response
back to the guest. Sending advances on writes and TCP acknowledgements; the
maintenance timer does not impose a per-block download delay.

Other GET/HEAD requests use browser fetch and remain subject
to CORS, mixed-content rules, and browser header restrictions. Uploads, incoming
servers, arbitrary TCP/UDP protocols, and forwarding guest credentials are not
supported. Missing or blocked downloads return an HTTP error, not an empty
successful file.

The local TLS endpoint uses an untrusted, self-signed certificate. Firmware
that does not verify certificates, such as CrossPoint 1.6.5's wolfSSL
downloader, works without patches. Firmware that verifies certificates or
pins keys will reject it. Real HTTPS fetches still use the browser's certificate
verification; no browser certificate or security setting needs changing.

Node can use the same adapter after `build.sh --web`:

```sh
WIFI_ASSETS="$PWD/build-wasm-deps/wifi-assets" \
  eink-qemu/wasm/run-node.sh flash.bin sd.img
```

Node defaults to networking off without `WIFI_ASSETS`. The browser and Node
release sessions and pending downloads when the runtime stops or aborts.

Network checks (no relay or host sockets):

```sh
eink-qemu/wasm/build-browser-net.sh
python3 eink-qemu/wasm/prepare-wifi-assets.py build-wasm-deps/wifi-assets
node --test eink-qemu/wasm/test-browser-network.mjs
python3 -m unittest discover -s eink-qemu/wasm -p test_wifi_assets.py
```

Set `BENCH_NETWORK=1` on the Node test command to also measure TCP/TLS throughput
with a 10 ms simulated guest poll interval and verify every downloaded byte.
