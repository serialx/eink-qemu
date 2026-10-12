# X4 Pro complete fidelity calibration plan

> Execution: use superpowers:executing-plans inline; independent final review.

**Goal:** Perform every calibration supported by the connected reader and local
tooling, implement measured models, and commit reproducible results in focused
changes. The user explicitly authorized all calibration and local commits.

**Architecture:** Extend the existing X4 Pro machine and shared instruction-cost
accounting. A standalone, checksummed diagnostic runs unchanged on hardware and
QEMU. Fit CPU costs first, then SDMMC residuals and panel transfers; retain
generic defaults on other boards. Finish with paired firmware workloads and
restoration. Unknown optical or environmental behavior remains explicitly
unmeasured rather than assigned invented parameters.

**Stack:** QEMU C, Xtensa/Arduino/ESP-IDF diagnostic, Python analysis and qtests,
native and WebAssembly regression checks.

**Spec:** The following-calibration sections of
[x4pro-calibration-design.md](x4pro-calibration-design.md), extended by the user's
instruction to perform all possible calibration and commit the work.

## Constraints and review focus

- Preserve the verified original flash; temporary uploads touch only app0's
  verified application range. Restore the original application and verify it.
- Keep unrelated SD files intact. Writes use one uniquely named allocated test
  file; never benchmark writes against arbitrary raw sectors.
- Record probe source/toolchain/image hashes, clocks, card identity/geometry,
  raw samples and checksums. Keep device/private content outside git.
- Compare identical workloads, include held-out checks, and publish residuals.
- Test MMIO/fault timing refunds, clock changes with cached TBs, DMA/IRQ ordering,
  reset during pending work and explicit overrides. Keep X3 and generic defaults.
- Separate electrical BUSY timing, firmware wait overhead, CPU rendering time,
  host runtime speed and optical behavior.

## Commit-sized tasks

1. [x] Commit the verified UC8279-0x68 model, launcher integration and initial
   report separately. Preserve all earlier evidence and review fixes.
2. [x] Add a reproducible standalone diagnostic under `eink-qemu/x4cal/`:
   checksummed IRAM/flash arithmetic, branch/call/divide/FPU, SRAM/PSRAM access,
   memory-library and rendering-like workloads; SDMMC reads and dedicated-file
   writes; panel identity/LUT/refresh probes and lifecycle observations.
   Build and emulator-test before temporary hardware upload. Collect repeated
   samples and retain a restoration procedure with verified offsets/hashes.
3. [x] Add Xtensa instruction costs and S3 clock scaling, only where measurements
   justify them. Fit on training rows and report held-out/working-set residuals.
   Add executable regression tests for class/region costs and clock transitions;
   preserve existing tick accounting and other Xtensa machines.
4. [x] Model SDMMC wire/command/card timing with virtual deadlines and coherent
   completion interrupts. Measure several request sizes/access patterns/clocks.
   Add qtests for clock/bus width, timer completion, reset and error paths;
   compare unchanged probe checksums and timings against hardware.
5. [x] Replace provisional panel constants with repeated observations, probe
   longer LUTs and different PLL settings where safe, verify upload timing and
   sleep/reset/power behavior. Fit only observable behavior; record any need for
   external photographic/electrical equipment.
6. [x] Run paired functional/reading checkpoints where serial instrumentation
   permits, restore the device, run native/Wasm/X3 regressions, obtain independent
   review, fix material findings, and commit the final evidence/limits report.

## Execution decisions

Continue on `codex/x4pro-calibration` in the current checkout, preserving the
private evidence and fresh builds. The latest user instruction authorizes the
remaining phases and commits; no repeated design-approval gate is needed.
External firmware repositories are references only; diagnostic source and
builds live in this repository. Do not push or publish.

## Outcome

Implemented and measured CPU/clock, SDMMC, SPI, panel PLL/OTP state, and RTC
sleep/reset behavior. Published checksummed samples, reproduction commands,
residuals and restoration evidence in [x4cal/README.md](x4cal/README.md).
Large PSRAM cache misses, optical response, RF and complete deep-wake boot
latency remain explicit fidelity limits; no synthetic fit conceals them.
