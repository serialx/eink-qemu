# X4 Pro PSRAM calibration

The X4 Pro now models PSRAM data-cache misses, refill occupancy and dirty
writeback. The original 512 KiB workloads improve from errors of −62% and −92%
to **+1.28% sequential** and **−1.23% random**. Cached instruction costs stay
unchanged. These are guest elapsed times on one physical reader, not host speed
benchmarks or a cycle-accurate ESP32-S3 model.

## Results

The second physical boot is the validation reference. Each row uses the median
of repeated executions of the same frozen binary on hardware and QEMU.

| Workload | Hardware | Before | Calibrated | Error |
| --- | ---: | ---: | ---: | ---: |
| Original sequential RMW, 512 KiB | 240,182 µs | 91,366 µs | 243,255 µs | +1.28% |
| Original random reads, 512 KiB | 245,114 µs | 19,248 µs | 242,096 µs | −1.23% |
| Held-out packed-pixel rendering, SRAM | 149,238 µs | 150,859 µs | 150,859 µs | +1.09% |
| Held-out packed-pixel rendering, PSRAM | 149,454 µs | 150,859 µs | 151,021 µs | +1.05% |

The focused kernels cover reads, stores and read-modify-writes, sequential words
and one word per cache line, offsets 0/1/3/7, 4 KiB/512 KiB working sets, and
240/80 MHz CPUs. Across **96 groups**, median absolute error is **1.37%**, with
a **9.05%** worst case. Every full-buffer hash matches, including stores.

The broader capacity/pattern sweep covers 4/16/32/64/128/512/1024 KiB, strides
of 1/8/16/1024 words, random reads, dependent reads and cold/warm execution.
Across **392 groups**, median absolute error improves from **40.68% to 11.90%**.
Its worst remaining error is **26.80%**, in a cached, branch-heavy kernel whose
CPU execution time was already underestimated without PSRAM misses. The model
does not disguise this instruction-timing error with a cache-hit penalty.

Native and Node/Wasm checksums match throughout. Median elapsed times differ by
at most 1 µs in the focused suite, 40 µs in the broader sweep, and 0 µs in the
30 original CPU groups. The broader difference reflects interrupt/scheduling
phase; neither run has a data mismatch.

## Model and calibration

Hardware reports `DCACHE_CTRL=0x9`, 32 KiB capacity, 32-byte lines, 80 MHz octal
PSRAM and 8 MiB physical RAM. The data cache is **four-way associative** according
to the [Espressif datasheet](https://documentation.espressif.com/esp32_s3_datasheet_en.pdf)
and the installed ROM cache API, despite the SDK defining `DATA_CACHE_8WAYS`.
The emulator follows the capacity and line-size control bits, and uses physical
PSRAM tags so MMU aliases share cache entries. Replacement uses LRU. Dirty state
is tracked separately; the underlying RAM remains coherent and authoritative.

Successful scalar, floating-point, atomic and S3 vector data accesses update
the timing cache. Failed conditional stores do not dirty a line. Cache mode
changes and reset clear timing tags. MMIO invalidation/writeback/clean commands
update them over the specified range; the range register counts cache lines.
Firmware on this S3 uses the external cache controller: its Xtensa configuration
has no core D-cache instructions such as `dhi`.

The measurements support a pipelined bus model. A requested word becomes
available before the refill has finished occupying the bus. The CPU can execute
hits during the refill tail and a queued dirty writeback. A subsequent miss
waits for bus availability. This accounts for both the similar throughput of
simple and branch-heavy miss loops and the lower incremental cost of sequential
writes with more computation between cache-line boundaries.

X4 Pro defaults, fitted using the first physical boot:

| Property on `esp32s3.icache` | Value | Meaning |
| --- | ---: | --- |
| `psram-read-ns` | 305 | Fixed portion of requested-word delay |
| `psram-read-cycles` | 8 | Additional CPU-clock-dependent delay |
| `psram-read-tail-ns` | 170 | Bus occupancy after the word is available |
| `psram-write-ns` | 390 | Fixed dirty writeback occupancy |
| `psram-write-cycles` | 4 | Additional CPU-clock-dependent occupancy |

For example, at 240 MHz the requested-word delay is 339 ns and the refill bus
tail is 170 ns. A dirty eviction adds 407 ns of bus occupancy. These are effective
fitted delays, not claims about individual PSRAM command waveforms. They were
chosen from sequential/strided timing differences; dependent reads and the
original CPU/render functions were excluded from fitting.

Runtime stalls advance icount's virtual clock in nanoseconds without changing
the prepaid translation-block instruction budget. A completed RAM access keeps
its charge when a later MMIO instruction is replayed. The CPU leaves translated
execution at the next block boundary to service deadlines. This avoids changing
the already calibrated cache-hit instruction costs and works with native and
WebAssembly TCG. Stall time does not scale with `-icount shift`.

The generic S3, Picco and X3 retain their previous defaults. To disable PSRAM
timing explicitly, use the structured global syntax (the device name has a dot):

```sh
-global driver=esp32s3.icache,property=psram-read-ns,value=0
```

## Evidence and reproduction

`3108cf2829` contains frozen probe v5; the current source adds the v6 `detail`
command. The v5 and v6 image/source hashes, configuration and flash restoration
hashes are in [psram-manifest.json](evidence/psram-manifest.json). Two physical
boots of each binary produced **2,928 PSRAM/detail samples** plus **300 original
CPU samples**. Only the rounded app0 prefix was temporarily overwritten. The
original application was restored after each session and verified by ROM
readback. No diagnostic SD or panel operations were performed.

```sh
python3 eink-qemu/x4cal/analyze_psram.py
pio run -d eink-qemu/x4cal/probe -e x4pro
python3 eink-qemu/mkflash.py build/x4pro-calibration.local/probe-build/x4pro probe-flash.bin
python3 eink-qemu/x4cal/run_probe.py --qemu build/qemu-system-xtensa \
  --flash probe-flash.bin --sd disposable-sd.img --log psram.log psram detail cpu
```

The analyzer validates paired coverage and checksums before summarizing residuals
against the second hardware boot. The public CSVs contain synthetic measurements,
not user data or device identifiers. Version 5's generic store/RMW kernel returns
an address checksum; v6 additionally hashes the entire buffer outside the timed
interval to verify stored data. For exact historical reproduction use each
recorded source revision and frozen image rather than mixing probe versions.

## Limits

- Fitted bus timing applies to the measured 80 MHz octal PSRAM and 32-byte cache
  lines. Other line sizes and capacity settings have functional tag behavior;
  their bus timing, PSRAM clock changes and quad PSRAM are not calibrated.
- LRU approximates replacement. Flash data does not compete for these timing
  tags; instruction-cache misses, cache locking/preload/freeze, DMA coherence and
  multicore contention have not been calibrated. Explicit cache-maintenance
  commands update tag/dirty state but do not model their complete bus latency.
- The clock snapshot includes the prepaid current translation block. Multiple
  accesses in a block therefore approximate overlap at block granularity, and
  a timer deadline can be serviced after that block finishes. This is not a
  cycle-accurate pipeline or interrupt-latency model.
- Word position within a line has a measurable effect, especially the last word
  at 80 MHz. The model averages this behavior; that case accounts for the focused
  suite's 9.05% maximum error. Branch-heavy cache-hit CPU timing remains a separate
  limitation, quantified above.

Regression evidence includes real Xtensa execution tests for capacity, dirty
eviction, overlap, 64 KiB mode, MMU aliases, invalidation, scalar/vector/atomic
accesses and single-instruction blocks. The full native suite, X3 cost and panel
drift checks, JavaScript tests, native/Wasm builds and original-firmware Home
smokes pass. Native and Node Home screenshots are byte-identical. See
[psram-results.json](evidence/psram-results.json) for the numerical results.
