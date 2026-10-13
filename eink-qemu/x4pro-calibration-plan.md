# X4 Pro UC8279-0x68 Implementation Plan

> Historical implementation plan: UC8279 revision `0x68` is now the default.
> See [current calibration and profile selection](x4cal/README.md).

> **For agentic workers:** Use superpowers:executing-plans for this plan. Track evidence and decisions in `build/x4pro-calibration.local/progress.md`.

**Goal:** Run the backed-up X4 Pro firmware using the connected reader's panel protocol and measured refresh timing.

**Architecture:** Add an explicit `panel=uc8279-68` X4 Pro machine property. Extend the existing 800x480 panel transport and ink model with the seven-byte LUT encoding, register addressing and profile-specific timing. Preserve the default UC8179 profile.

**Tech Stack:** QEMU C, Python unittest/qtest, native and WebAssembly launchers.

**Spec:** [x4pro-calibration-design.md](x4pro-calibration-design.md).

## Global constraints

- Unmodified firmware; no PC-specific workarounds.
- Existing X3 calibration and UC8179 defaults remain compatible.
- Keep physical-device data and flash backups in the ignored evidence directory.
- Timing uses virtual clocks. Initial optical settings are approximations, not a new photographic fit.
- CPU/PSRAM, SDMMC timing, and S3 deep-sleep wake are separate follow-up changes.

## Review focus

- A short LUT upload after a longer upload must not play stale groups.
- A partial window must preserve pixels outside it and respect the visible gate offset.
- Reset during BUSY must cancel completion and animation timers safely.
- Explicit global timing overrides, including turbo, must take precedence over profile defaults.
- Invalid panel selections must fail clearly; profile selection must reach both native and browser QEMU.

## Task 1: Panel profile and protocol

Files: `hw/display/uc8179.c`, `hw/xtensa/esp32s3.c`, new `eink-qemu/tests/test_x4pro_panel.py`.

Interface: `-machine x4pro,panel=uc8279-68` creates the shared panel with the new profile; default and `panel=uc8179` retain existing behavior.

- [x] Add failing qtests for profile validation, measured VER/MTP header, PON and reset, SPI/GPIO reads, TRES/partial addressing, seven-byte phase repetition, short LUT replacement, and full/fast/grayscale/settling BUSY durations.
- [x] Add machine property validation and profile selection; add measured identity and MTP header.
- [x] Decode complete seven-byte groups in phase order, with independent row lengths and bounded animation storage; keep the six-byte decoder behavior.
- [x] Implement the profile's 800x600 addressed RAM, 800x480 visible gate window, partial-window updates, and PSR orientation.
- [x] Keep BUSY duration independent of the bounded synthetic OTP animation. Add trace output for timing and register settings.
- [x] Build both targets and pass new panel tests plus existing panel and X4 Pro regressions.

## Task 2: Native and browser selection

Files: `eink-qemu/run.sh`, `eink-qemu/drive.py`, `eink-qemu/turbo-args.sh`, `eink-qemu/wasm/web/{app.js,runtime.js,index.html}`, Node launcher if applicable, launcher tests.

Interface: native `X4PANEL=uc8279-68`; browser panel choice passed separately from SoC/machine selection. Omission uses UC8179.

- [x] Add selection/argument regression coverage before implementation.
- [x] Forward the selected profile without changing firmware chip detection or input routing.
- [x] Keep turbo finite and responsive with the new timing properties.
- [x] Run launcher, JavaScript and panel regressions.

## Task 3: Firmware replay and evidence

Files: calibration report under `eink-qemu/`, local replay artifacts under `build/x4pro-calibration.local/`.

- [x] Replay the original flash on the new profile; verify UC8279 detection and completed Home refreshes.
- [x] Exercise grayscale and settling using the installed firmware's LUTs; compare virtual deadlines with the recorded hardware waits.
- [x] Inspect screen checkpoints for orientation, addressing, grayscale and input response.
- [x] Build WebAssembly and replay the same profile in Node and the browser where available.
- [x] Run the repository's emulator regression suite; document exact measured coverage and remaining hardware/optical limitations.
- [x] Obtain an independent code review, address material findings, and leave the changes locally reviewable.

## Execution decisions

The user approved the design and instructed execution. Work proceeds inline on a dedicated local feature branch in the existing workspace so the established build and private device evidence remain available. No additional design approval is needed for routine implementation choices. Local commits and publication are deferred until the implementation is reviewable.

## Outcome and acceptance limits

Tasks 1–3 are implemented and verified as described in
[x4pro-calibration-report.md](x4pro-calibration-report.md). The planned
controlled hardware repetitions and paired physical screenshots remain open;
the opt-in timing profile is provisional. Existing release-control and
screenshot-protocol limitations prevented that broader acceptance workload.
CPU/PSRAM, SDMMC and optical calibration remain follow-up work.
