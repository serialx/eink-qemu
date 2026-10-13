# X3 phase profiling

These tools measure the CrossPoint X3 boot without changing firmware. See
[current measurements](../README.md#measured-performance) for performance and
caveats, and [turbo-profile.md](turbo-profile.md) for historical profiles.
They use only Python's standard library and Node's built-in profiler. Native
sampling additionally requires macOS `sample` and permission to inspect the
emulator process. Run from any directory; default binaries and output paths
are relative to this repository, found from the script location.

Build the normal native and default Wasm emulators using the
[WebAssembly guide](../../../EINK-QEMU.md#webassembly-build), then set paths to
original input images. Each run makes its own copies; originals are not written.
On APFS, `cp -c` avoids physically copying the whole sparse SD image. On
Linux, copies use `cp --reflink=auto --sparse=always` so the stock-sized card
does not require writing 2 GiB before every run.

```sh
export X3_FLASH=/absolute/path/to/x3-flash.bin
export X3_SD=/absolute/path/to/sd.img
python3 eink-qemu/wasm/perf/benchmark.py --help
python3 eink-qemu/wasm/perf/benchmark.py
```

The default is three interleaved accurate native/Wasm runs with `sleep=off`.
`--turbo` (or `TURBO=1`) applies the canonical `turbo-args.sh x3` options to
both runtimes and forces `sleep=off`; `--accurate` overrides `TURBO=1`.
Each `run.json` and `results.json` records the mode, and an output directory
containing another mode is rejected. Older records without a mode are treated
as accurate: those harness versions ignored `TURBO`.
Home is
the third completed `X3_DRF` refresh. The second phase ends at the first
complete subsequent `[MEM]` line, then the harness captures the named `panel`
console and quits normally. It checks all screenshots against the first run;
accurate mode also compares the first eight guest wait timestamps. Turbo
requires the eight wait milestones but does not compare their timestamps.
Output directories must be new,
so an accidental rerun cannot silently overwrite evidence.

`--native-bin`, `--wasm-build`, `--node`, `--flash`, `--sd`, `--runs`,
`--variants`, `--output` and `--timeout` override defaults. `--sleep on` is
available, but exact timestamp comparisons are disabled because idle-time
warping includes host scheduling variation. `home-probe.py` provides the same
CLI stopped at Home; it does not check the later screenshot/timestamps.

The default eight wait milestones belong to the original SD fixture. For a
different card, first inspect a native run and use `--wait-count N` for its
complete startup/thumbnail wait sequence. Each run must reach all N milestones;
accurate sleep-off runs still compare their full text, including guest times.
`--node-arg=--flag` passes an option to Node and can be repeated for controlled
V8 experiments. The exact Node options are recorded in each run's `argv`.

Keep timing runs separate from profiles, instrumented counters and builds.
On a shared host, preserve interleaving and inspect each run as well as medians.
The scripts cannot reserve CPU time or make other workers quiet.

```sh
# Node: one full CPU profile per pthread; analyzer crops at phase markers.
python3 eink-qemu/wasm/perf/benchmark.py --variants wasm --profile node \
  --output build-wasm/evidence/profile/node-cpu
python3 eink-qemu/wasm/perf/analyze-profile.py \
  build-wasm/evidence/profile/node-cpu/wasm-{1,2,3} \
  --output build-wasm/evidence/profile/node-analysis.json

# Native: separate lifetimes prevent slow sampler initialization from
# accidentally including thumbnail work in the startup profile.
python3 eink-qemu/wasm/perf/benchmark.py --variants native --profile native \
  --stop-at home --output build-wasm/evidence/profile/native-home
python3 eink-qemu/wasm/perf/benchmark.py --variants native --profile native \
  --output build-wasm/evidence/profile/native-settled
python3 eink-qemu/wasm/perf/analyze-profile.py \
  build-wasm/evidence/profile/native-home/native-{1,2,3} \
  --output build-wasm/evidence/profile/native-home-analysis.json
python3 eink-qemu/wasm/perf/analyze-profile.py \
  build-wasm/evidence/profile/native-settled/native-{1,2,3} \
  --output build-wasm/evidence/profile/native-settled-analysis.json
```

`sample` starts at launch for Home-only runs and after Home for settled runs;
SIGINT ends sampling at the selected milestone. Startup/attachment overhead
can leave a sampling gap. Inspect `.stderr` and sample counts; reject a capture
that extends beyond its phase. This is statistical sampling, not exact CPU
accounting. Native tables report self sample counts; Node reports weighted
elapsed samples, including waits. Profiles are excluded from timing medians.

The analyzer selects the vCPU thread by symbols, not a hard-coded thread ID.
`--thread mainloop` selects the QEMU main-loop thread; Node also supports
`--thread js-main`. Do not add their elapsed samples together as wall time.
Device functions map to source files. Generated Wasm TBs are grouped by their
separate module URLs. Visible Asyncify frames exclude instrumentation inlined
into other functions. Top wait stacks retain callers so mutex contention and
synchronous JavaScript proxy waits are distinguishable.

`profile-hooks.cjs` measures only hot-TB synchronous module construction and
instantiation, not V8 background optimizing compilation. `node-cpu-prof.sh`
is an optional `NODE` wrapper for `run-node.sh`; set `TURBO_PROFILE_DIR` to an
absolute output directory. Node must quit normally to flush worker profiles.
V8 CPU-profile timestamps are checked against Python monotonic markers;
constructor events use a recorded wall/monotonic offset because Node
`process.hrtime()` has a different origin on this macOS host.

Temporary counter sources/builds and their restoration script live in
`build-wasm/evidence/profile/`, as cited by the report; no QEMU instrumentation
is enabled by this directory. `summarize-counters.py` extracts their phase
snapshots, and `make-report.py` regenerates the report's measurement tables.
Raw profiles, images and logs stay in ignored build directories.

## amd64 measurements

Measured on 2026-10-13 with an Intel Core i3-N305 (8 cores), Linux x86-64,
GCC 15.2.0, Emscripten 6.0.12 and Node 26.11.1. The baseline is
`04e34edecf`; the optimized code ends at `722eadf74f`. Native builds use the
normal configure options; Wasm uses the default `MEMORY64=2` build. No extra
Node flags are required.

Each runtime has three interleaved before/after pairs, with fresh copies of
the same flash and SD images, accurate device timing and `sleep=off`. Builds
and profiles ran separately from these measurements. Home is the third
completed `X3_DRF`; settled is the first complete subsequent `[MEM]` line.
Both times start at process launch. These are medians in seconds:

| Runtime | Baseline Home | Optimized Home | Baseline settled | Optimized settled |
| --- | ---: | ---: | ---: | ---: |
| Native QEMU | 2.839 | 1.029 | 4.367 | 1.840 |
| Node | 4.888 | 4.479 | 9.075 | 8.063 |

The settled medians decrease by 58% for native and 11% for Node. Individual
settled pairs, in measurement order, were native `4.392/1.840`, `4.309/1.897`,
`4.367/1.798` and Node `9.075/8.288`, `9.121/8.001`, `8.961/8.063`.

The fixture is the unmodified CrossPoint 1.6.5 X3/X4 release app, composed
with `mkflash.py`, and a fresh 2050 MiB FAT32 card with 32 KiB clusters
containing `aesop-eng.epub` and `demian-kor.epub`. This fixture has five startup
waits, so the benchmark uses `--wait-count 5`. It differs from the older M5
fixture; timings across those two series are not directly comparable.
All twelve runs have identical panel pixels and all five guest wait lines,
including timestamps (1061, 1996, 2669, 3157 and 3647 ms). The panel SHA-256 is
`3de44548188a599f93db8c35771cea3468e0ab288704fe6a6f3971683459d927`.
Raw runs, profiles and input hashes are in the local ignored directory
`build-wasm/evidence/amd64/`.

Two changes account for the improvement:

- Native x86-64 learns translation-block boundaries at MMIO accesses, including
  RISC-V PC-relative blocks. Repeated polling can reuse a block ending at the
  access instead of repeatedly unwinding and replaying it. A 2000-read probe
  reduced I/O-recompile exits from 2005 to 5 with identical results.
- The Wasm interpreter calls `helper_lookup_tb_ptr` directly. Its fixed pointer
  signature avoids JavaScript libffi conversions on indirect branches. Separate
  Node CPU profiles confirm the lookup disappears from `ffi_call_js` stacks;
  other helpers retain the generic path.

Lower JIT thresholds and Node's `--wasm-tiering-budget=100000` did not produce
a consistent improvement. `--no-liftoff` substantially slowed startup. The
default JIT threshold and V8 options remain unchanged.

Validation includes both native and Wasm RISC-V/Xtensa builds, X3 and X4 Pro
firmware smoke tests against baseline panel captures, weighted icount/deadline
and clock-change tests, and the device suite (54 passed, one optional release
fixture skipped). The new guest probes cover repeated MMIO, self-modifying
code, cold/hot cross-page indirect branches, generic helpers, and an instruction
fetch fault during TB lookup. Run the suite with `RISCV32_CC` pointing to a
RISC-V bare-metal GCC and `WASM_BUILD_DIR` to the default Wasm build. This
container requires `ASAN_OPTIONS=detect_leaks=0` for the panel test; address and
undefined-behavior checks remain enabled, but leak detection was not tested.
Browser and Apple-host performance were not measured in this series.

### Further Node measurements: interrupt notifications

The next pass compares `5c659ab024` with `ea5a49ceda` on the same i3-N305,
Node version and two-book X3 fixture. Three interleaved pairs use fresh image
copies, accurate timing, `sleep=off` and no profiler during timing runs.

| Node phase | Before | After | Time reduction |
| --- | ---: | ---: | ---: |
| Home | 4.541 s | 4.361 s | 4.0% |
| Home to settled | 3.624 s | 3.158 s | 12.9% |
| Settled, from launch | 8.121 s | 7.479 s | 7.9% |

These are independent phase medians. The settled pairs are `8.121/7.725`,
`8.213/7.431`, and `8.092/7.479` seconds. All six runs match the preceding
native reference's panel hash and all five guest wait lines exactly.

The C3 interrupt matrix now tracks the last level sent to each CPU line.
Threshold changes and shared-source updates notify the CPU only on a level
transition. Source lookup scans asserted input bits, with masked priorities
rejected first. Reset still lowers every output explicitly. Separate Node
profiles confirm a substantial reduction in interrupt-dispatch samples.
This improves the X3 device model in both runtimes; no extra Node flags are
needed, and it does not change the X4 Pro interrupt model.

A follow-up that computed the entire output bitmap on threshold changes
improved settled time by only 0.4% and slightly worsened Home time in three
direct pairs. It was discarded. The retained variant and its measurements
are under `build-wasm/evidence/amd64-node2/matrix{,-wasm}/`; the additional
bitmap experiment is in `aggregate-probe/` and `final/` and is not shipped.

Validation: 55 tests passed and one optional release fixture was skipped,
including a new test of real matrix functions with counted CPU callbacks and
50,000 randomized routing, priority, masking and reset changes. Weighted
icount/deadline checks pass, as do X3 native accurate and native/Node turbo
smoke tests. The same container leak-detection limitation noted above applies.

### Bound main-module compilation work

On the same i3-N305 with Node 26.11.1 and Emscripten 6.0.12, the default
Wasm build at `3a838c7aff` spent much of its optimizing compilation on the
RISC-V decoder. Commit `d104d310ff` bounds inlining: Wasm-only `noinline`
translator declarations and a 16 KiB Binaryen combined-inlining budget for
the RISC-V executable reduce
`decode_insn32` from 367,734 to 56,765 bytes and `tb_gen_code` from 72,694 to
7,922 bytes. This changes compiler inlining decisions without changing the
generated decoder, ISA coverage, device timing, JIT threshold or Node flags.
Native and Xtensa build settings remain unchanged.

Three interleaved baseline/candidate pairs per mode, with exclusive host
locks, fresh images, `sleep=off` and five waits, produced these medians:

| Node phase | Accurate before/after | Turbo before/after |
| --- | ---: | ---: |
| Home | 4.727 / 4.437 s | 2.914 / 2.688 s |
| Home to settled | 3.481 / 3.586 s | 4.244 / 4.200 s |
| Settled, from launch | 8.117 / 8.079 s | 7.149 / 6.869 s |

Home improved in all six pairs. Accurate settled time was mixed (one pair
slower and only a 0.5% median reduction); turbo settled time improved in all
three pairs, with a 3.9% median reduction. Accurate settled pairs were
`8.117/8.392`, `8.652/8.079`, `8.063/7.695`; turbo pairs were
`7.334/6.962`, `7.130/6.833`, `7.149/6.869` seconds. All twelve captures match
the panel hash above and their baseline wait lines, including every accurate
guest timestamp.

A separate shared-lock turbo trace reduced summed main-module TurboFan
event durations from 7,557 to 3,128 ms, including decoder compilation from
4,264 to 265 ms. These overlapping compiler-event durations are not elapsed
startup time. Total Wasm file size increased 1.2%; memory initialization is
unchanged. LLVM `noinline` alone and a 64 KiB Binaryen budget left the decoder
over 340 KiB, so neither was retained. Evidence, per-run values, build hashes
and validation details are under `build-wasm/evidence/amd64-perf3/compile-cost/`.

Both default Wasm targets build, and the native RISC-V translation unit
compiles with unchanged native flags. The device suite passes 99 tests;
12 Xtensa compiler-dependent tests and one optional release-fixture test
are skipped. The indirect-branch probe passes in native and Node builds;
the native icount replay/SMC probe also passes.

### Further Node measurements: series 3

The cumulative comparison is `3a838c7aff` to `6a92b2c17f` on the same
2026-10-13 i3-N305 host (seven usable CPUs), Node 26.11.1, Emscripten 6.0.12
and default `MEMORY64=2` build. Each mode has six interleaved pairs,
alternating before/after and after/before, with fresh two-book X3 image
copies, `sleep=off`, `--wait-count 5` and no profiler or extra Node flags.
Timing holds exclusive `gate.lock` then `host.lock`, releases both after
at most three pairs, and cannot overlap cooperating builds/profiles;
those use the shared host lock after gate admission, with at most three
build jobs.

| Mode | Node phase | Before median | After median | Median paired reduction |
| --- | --- | ---: | ---: | ---: |
| Accurate | Home | 4.441 s | 3.761 s | 15.65% |
| Accurate | Home to settled | 3.271 s | 2.943 s | 9.66% |
| Accurate | Settled, from launch | 7.730 s | 6.703 s | 13.35% |
| Turbo | Home | 2.846 s | 2.488 s | 11.72% |
| Turbo | Home to settled | 3.906 s | 3.698 s | 4.99% |
| Turbo | Settled, from launch | 6.768 s | 6.187 s | 8.86% |

Phase medians are independent. Paired reductions use the median of each
pair's after/before ratio, rather than the ratio of the two medians.
All six settled pairs improve in each mode. All 24 runs retain the panel
hash above and identical complete wait lines within each mode: accurate
`1061, 1996, 2669, 3157, 3647` ms; turbo `760, 761, 968, 983, 1003` ms.
These measurements describe this Node fixture, not browser or native speed.

Retained changes and their separate measured effects follow. Percentages
here compare independent phase medians against each experiment's own base;
they are not additive:

- `119c06e3e7`: live MMIO word fast path; accurate Home/settled decreased
  7.0%/5.5%; turbo settled was 0.7% slower in three pairs, with no demonstrated gain.
- `d104d310ff`: decoder inlining bound; accurate/turbo Home decreased
  6.1%/7.8%; the [decoder comparison above](#bound-main-module-compilation-work)
  records the mixed accurate settled result and compilation evidence.
- `b79d2a98c2`: aligned TCI RAM fast path; accurate/turbo settled decreased
  2.4%/2.7%, with all six pairs per mode faster.
- `7a8d8b94f8`: defer lookup-state initialization past live MMIO reads;
  confirmation on b79 reduced accurate/turbo settled 1.1%/1.4% in three pairs.
- `6a92b2c17f`: refresh only interrupt lines crossing the C3 threshold;
  accurate/turbo settled decreased 1.2%/0.9% in six pairs, while turbo Home
  increased 1.9%.

Rejected in this series (positive time changes mean slower):

- JIT memory-emitter TLB comparison retention: negligible accurate settled
  change (−0.6%) and a turbo settled regression (+5.0%).
- TCI typed FFI trampolines: adding them to the RAM fast path regressed
  turbo Home by 4.3%, without a demonstrated settled gain.
- Indirect-lookup RV32 state fast path: accurate settled changed −2.4%
  with baseline-first order but +0.1% with candidate-first order.
- Turbo UC8279 tiling: settled time increased 0.7%; every turbo pair was slower.
- Narrower decoder variants: gains remained within noise; the closest split
  gave −0.24%/−0.73% paired accurate/turbo settled changes and an order-dependent
  turbo result.
- Halted-WFI inline warp: unsafe ordering despite an apparent ~9% Node gain;
  a late BH observed the clock already advanced from 0 to 100.
- Indefinite-poll wait transport: no gain on the final stack; paired settled
  changes were +0.47% accurate and +0.075% turbo.

All per-run Home, Home-to-settled and settled values, paired summaries,
binary hashes, exact-output checks and experiment report references are in
`build-wasm/evidence/amd64-perf3/final/REPORT.md`. Raw evidence remains local
and ignored. No rejected candidate is included in `6a92b2c17f`.
