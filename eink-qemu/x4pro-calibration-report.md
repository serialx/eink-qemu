# X4 Pro UC8279-0x68 panel implementation

> Historical first-stage report. The completed timing calibration, temporary
> diagnostic uploads, verified restoration and remaining limits are in
> [X4 Pro fidelity calibration](x4cal/README.md). Its measurements supersede
> the provisional timings and open implementation stages below.

The explicit `uc8279-68` profile runs the connected X4 Pro's unmodified
CrossPoint 1.6.5 firmware through boot, touch navigation, text and image
grayscale, page turns, and return to Home. Native and WebAssembly builds
support it. This completes the first panel implementation; it does **not**
establish complete device timing or optical fidelity.

Work was based on `04e34edecf`, with evidence collected October 12–13, 2026
(KST). The design and implementation sequence are in
[the design](x4pro-calibration-design.md) and [the plan](x4pro-calibration-plan.md).

## Selecting the profile

```sh
X4PANEL=uc8279-68 eink-qemu/run.sh flash.bin sd.img
X4PANEL=uc8279-68 eink-qemu/wasm/run-node.sh flash.bin sd.img
```

`drive.py` accepts the same variable. Direct QEMU uses
`-machine x4pro,panel=uc8279-68`. In the browser, choose **X4 Pro panel →
UC8279 (0x68)**, or add `panel=uc8279-68&turbo=0` to the query string.
Turn Turbo off for measured panel delays. Auto detects the SoC, not the panel.
UC8179 remains the default; other boards ignore the separate panel selection.

## Implemented behavior

- VER `00 0F 68 01 00`, idle FLG `13`, and the captured 48-byte MTP header.
  RMTP includes its leading dummy byte; the firmware now records exactly
  the same header as the physical reader. Unknown waveform bytes remain zero.
- Seven-byte LUT groups, including group/pair repetition, per-row lengths,
  incomplete-group handling, and shorter replacement uploads. The longest
  row determines BUSY duration; animation storage is bounded independently.
- 800×600 addressed RAM, visible gates 120–599, TRES, partial-window uploads
  and refreshes, OLD/NEW planes, and PSR source/gate orientation.
- Virtual BUSY deadlines, explicit global timing overrides, reset cancellation,
  and finite turbo animation. Traces report profile, PSR, PLL, partial mode,
  decoded frame count, duration and virtual start time.
- Native/Node/browser profile selection, browser Ready status for the new
  firmware log tag, and smoke-test support.

## Timing evidence

These are firmware-reported waits, including polling/scheduling overhead,
not oscilloscope measurements of BUSY. The hardware column uses the cumulative
device-session log, without adding the overlapping initial baseline. The
native column uses the final firmware replay after the RMTP correction.

| Operation | Hardware: n; median [range], ms | Native: n; median [range], ms | Model BUSY, ms |
| --- | --- | --- | --- |
| PON | 5; 39 [39–40] | 1; 41 [41] | 40 |
| Full OTP | 7; 1327 [1327–1329] | 2; 1328.5 [1328–1329] | 1328 |
| Fast OTP | 19; 483 [482–484] | 8; 484 [483–485] | 483 |
| Text grayscale, 7 frames | 2; 229 [229] | 1; 229 [229] | 228.998 |
| Previous-gray settling, 25 frames | 1; 559 [559] | 1; 560 [560] | 558.992 |

Observed medians differ by at most the larger of 2 ms or 2%. This is a
consistency check against available observations, **not** the planned
controlled, repeated, same-state hardware/emulator acceptance workload.
The physical session includes ordinary UI activity and resets during backup;
settings and card contents were not paired with the emulator fixture.

The register timing fit is provisional: `18333 µs × frames + 100667 µs`, at
the observed PLL `0x0e`. Only the 7- and 25-frame modes have hardware samples.
An earlier native image-page replay exercised a 47-frame quality waveform,
reporting 964 ms against a 962.318 ms model deadline. That is an extrapolation,
not a hardware-validated image-mode timing result. Other PLL values are traced
but do not scale timing. POF remains a 2 ms approximation.

## Verification

- Fresh native Xtensa/RISC-V and Emscripten 6.0.12 `MEMORY64=2` builds pass.
- Emulator Python suite: 67 tests, 66 passed and one skipped. The existing
  separate drift regression passes for both panel controllers. Browser
  JavaScript suite: 35 passed.
- Actual QEMU GPIO reads verify identity, MTP dummy/header and the 40 ms BUSY
  edge. Sanitized controller tests cover LUT ordering/replacement, timing,
  bounded long uploads, partial windows, both PSR axes, ghosting boundaries,
  reset during BUSY, and turbo. Existing X3/UC8179 tests remain passing.
- Exact backed-up firmware selects UC8279 and reaches Home in native, Node
  26.11.0 and Chrome 155.0.8059.39. Native and Node measured-mode settled Home
  PPMs match byte for byte. Both turbo smoke runs also pass, with Node's
  settled image matching the native turbo reference exactly.
- Native screenshots were inspected for Home, Library, the four-level image
  fixture, text/page turn, and return to Home. Chrome checks profile arguments,
  Ready status, a pointer tap into Library, Stop re-enabling the panel control,
  and hiding that control for X3. These are emulator functional checks, not
  comparisons with physical-screen photographs.
- Independent review found an existing drift-test extraction regression;
  it was fixed without removing its assertions. Additional orientation and
  ghosting tests pass. Follow-up review found no material issue in the RMTP
  and browser milestone corrections.

The fixture is a generated two-chapter EPUB with text and four gray bands on
a private 64 MiB FAT32 card with 512-byte clusters. That geometry is useful
for browser functional tests; it is not the connected reader's card and must
not be used to claim SD or total page-render performance fidelity.

## Preserved device and evidence

The device remains on its original firmware. No persistent flash writes or
diagnostic SD writes were made to it. The full 16 MiB ROM-read backup still has
SHA-256 `c6ae669eca525b4c8888da5d2ae6812bd4616b458654ba15b59c178965d5d1d0`.
Emulator runs used disposable copies.

Private evidence lives under ignored `build/x4pro-calibration.local/`:

- `device-session.log`, `device-baseline.log`, their JSONL events, backup
  manifest/partition data, and failed RAM-stub/screenshot attempts.
- `native-final-command.json`, `native-final.log`, `.trace`, and screenshots;
  `reading-book-*` includes the image-quality and page-turn checkpoints.
- `node-command.json`, `node-replay.*`, `node-smoke/`, `native-smoke/`,
  `native-turbo/`, browser logs/screenshots and the local replay scripts.
- `wait-comparison.json`, fresh-build logs, red/green regression evidence,
  `final-native-tests.log`, `final-drift-tests.log`, `final-js-tests.log`.

Fresh native binaries were copied to `build/qemu-system-{xtensa,riscv32}`
for the usual launchers. The previous binaries were retained in the evidence
directory with `.before-calibration` suffixes. Flash dumps, device logs and
generated build artifacts are not included in the source changes.

## Remaining fidelity work

Controlled hardware repetitions and matching physical-screen checkpoints
remain open. The installed release lacks the new serial-control handshake,
and its screenshot framing is incompatible with the current monitor; those
limitations must be resolved before automated paired workloads.

The UC8279 optical model still uses approximate inherited parameters and
synthetic OTP animation. Unknown MTP waveform bodies, unequal-row hardware
timing, temperature response, other PLL settings and image-quality timing
need measurements. CPU/PSRAM costs, SDMMC command/DMA/card timing and deep-sleep
wake remain separate implementation stages. Total page-render timing is not
yet calibrated to this X4 Pro.
