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
