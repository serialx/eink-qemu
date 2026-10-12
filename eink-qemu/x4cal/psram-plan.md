# X4 Pro PSRAM timing calibration

Approved scope: extend the existing X4 Pro calibration with a measured data-cache
timing model, retain cached CPU timing, validate against the connected reader,
and commit reproducible evidence and implementation separately. Base: bc198e18b5.

## Constraints

Use the same frozen diagnostic image on hardware and QEMU. Preserve the original
flash and SD contents; restore the overwritten app prefix in a finally block and
verify its readback. The diagnostic does not access SD. Defaults for X3, Picco,
and generic ESP32-S3 machines must remain unchanged. Model timing, not incoherent
cache data: RAM remains the authoritative storage. Explicitly document limits.

## Task 1: Measure cache behavior

Add an IRAM PSRAM diagnostic with repeated sequential, random, dependent,
read/write, warm/cold and capacity/conflict workloads. Record cache registers,
line size, CPU frequency and PSRAM configuration. Freeze the image, run the
uncalibrated emulator, then collect hardware repetitions and restore the reader.
Interface: immutable image and structured PSRAM records for Task 2 fitting.
Validation: build, checksum matching, completion markers, verified restoration.

## Task 2: Model and calibrate misses

Write real Xtensa execution tests that distinguish cache hits, capacity/conflict
misses, dirty evictions, invalidation, MMU aliases and CPU clock changes. Observe
failure before implementation. Add an opt-in S3 cache timing model and runtime
memory-access hooks, with nanosecond stalls integrated into icount. Calibrate
refill/writeback timing against Task 1 and validate held-out workloads.
Interface: X4 default properties, unchanged cached costs, shared native/Wasm code.
Validation: targeted tests, paired diagnostic checksums and residuals.

## Task 3: Verify and document

Run native tests, X3 cost verification, JavaScript tests and build/run Wasm.
Recheck prior cached CPU and rendering results. Document measurement method,
residuals, configuration and limitations. Obtain one independent final review,
address substantive findings, and commit all changes in meaningful groups.

## Review focus

Timer deadline overshoot and MMIO replay; faults and cross-line accesses;
physical tag aliasing, dirty eviction and reset; cache mode and invalidation;
default-off behavior outside X4 Pro; evidence provenance and held-out accuracy.
