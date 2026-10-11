# X3 CPU costs and SD refit

The X3 now uses **`-icount shift=0,sleep=on`**: one tick is one nanosecond.
Both `eink-qemu/run.sh` and `eink-qemu/drive.py` select this for X3; X4 Pro keeps
shift=2 and ordinary instruction counting. Direct QEMU launches must use
shift=0 with the X3 defaults below. Shift=2 would multiply these costs by four.

## CPU defaults and interpretation

Properties belong to `espressif-riscv-cpu` and accept integers from 0 to 255.
For example, `-global espressif-riscv-cpu.cost-flash-ns=9` changes the flash
base without rebuilding. Qdev properties are set before realization; restart
to change a fit. Explicit overrides, including zero surcharges, take precedence
over the X3 board defaults. Other RISC-V machines retain region bases of one
and zero surcharges unless explicitly configured.

| Property | X3 default (ns) | Charge |
| --- | ---: | --- |
| `cost-rom-ns` | 6 | Instruction starts in 0x40000000–0x4005ffff |
| `cost-sram-ns` | 8 | IRAM, executable DRAM, and addresses outside the ROM/flash windows |
| `cost-flash-ns` | 10 | Instruction starts in 0x42000000–0x427fffff |
| `cost-load` | 0 | Integer load, including C.LW/C.LWSP |
| `cost-store` | 3 | Integer store, including C.SW/C.SWSP |
| `cost-mul` | 0 | M-extension multiply variants |
| `cost-div` | 160 | Divide and remainder variants |
| `cost-branch` | 2 | Conditional branch, jump, call, return, including compressed forms |

The charge is `(region base + instruction-class surcharge) × clock factor`.
A zero base is treated as one so time advances. Conditional branches pay the
same surcharge whether taken or not. Bases describe the **instruction PC**,
not the data address: the probe's load/store and random-load loops execute from
flash even though their buffers are in SRAM.

The regional bases replace the earlier `cost-base` and `cost-flash-fetch`
properties. Integer 4 ns ticks could not fit the measurements: base=2 alone
made memcpy and software multiply 13–17% slower than the device, while adding
one flash tick made flash code about 40% too slow. Nanosecond costs allow a
separate ROM fit and finer flash adjustments. A ROM base of 6 ns plus store
and branch costs fits the library routines better than a flat 7 ns charge.
The flash base and branch cost already fit integer multiply, so no extra
multiply penalty is needed. A 160 ns divide surcharge is a compromise between
integer division and the integer operations used by software floating divide.

These are effective workload costs, not literal cycle counts or a pipeline
model. Fetch alignment, cache contents, branch prediction, operand-dependent
divide latency, data caching, and instruction overlap are not modeled. The
10 ns flash base deliberately favors page rendering over tiny cached loops.

## CPU probe measurements

The supplied device table contains five measurements per workload at 160 MHz;
three device boots were reported identical. `Plain` is the supplied emulator
capture at shift=2 with one tick per instruction. `Calibrated` is a fresh boot
of the supplied CPU probe with the current machine defaults at shift=0.
All 85 operations completed with zero errors and matching checksums. The probe
prints `cpu_mhz=160 xtal=40 flash_mhz=80 cache_size=16384`.

| Workload | Device µs | Plain µs | Device/plain | Calibrated µs | Device/calibrated | Emulator error |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| sram_loop | 113,199 | 56,119 | 2.017 | 116,612 | 0.971 | +3.02% |
| flash_loop_small | 125,779 | 56,119 | 2.241 | 144,759 | 0.869 | +15.09% |
| flash_spread_64k | 856,267 | 20,844 | 41.080 | 53,478 | 16.012 | -93.75% |
| flash_spread_256k | 855,649 | 20,844 | 41.050 | 53,483 | 15.999 | -93.75% |
| load_store | 46,369 | 22,988 | 2.017 | 61,770 | 0.751 | +33.21% |
| load_random | 176,087 | 88,188 | 1.997 | 225,178 | 0.782 | +27.88% |
| memcpy_1k | 92,700 | 52,832 | 1.755 | 98,316 | 0.943 | +6.06% |
| memset_1k | 68,540 | 32,788 | 2.090 | 68,273 | 1.004 | -0.39% |
| mul | 62,890 | 24,051 | 2.615 | 64,335 | 0.978 | +2.30% |
| div | 121,945 | 12,025 | 10.141 | 111,584 | 1.093 | -8.50% |
| fmul | 408,774 | 230,490 | 1.773 | 383,507 | 1.066 | -6.18% |
| fdiv | 641,471 | 224,481 | 2.858 | 698,159 | 0.919 | +8.84% |
| dmul | 290,251 | 170,151 | 1.706 | 273,916 | 1.060 | -5.63% |
| call_ret | 176,088 | 72,154 | 2.440 | 193,010 | 0.912 | +9.61% |
| call_ret_iram | 176,088 | 72,154 | 2.440 | 176,927 | 0.995 | +0.48% |
| layout | 615,940 | 273,248 | 2.254 | 732,422 | 0.841 | +18.91% |
| glyph_render | 1,625,458 | 733,726 | 2.215 | 1,948,309 | 0.834 | +19.86% |

All non-flash-sensitive rows meet the revised 10% target. The deliberate
flash/rendering exceptions are flash_loop_small (+15.09%), load_store
(+33.21%), load_random (+27.88%), layout (+18.91%), and glyph_render (+19.86%).
Their cached code runs faster on the device than the reading-oriented flash
base predicts. The 64/256 KB cache-thrash rows remain approximately **93.75%
too fast** (device/emulator about 16x); a static region base cannot reproduce
instruction-cache misses. They did not influence the fit.

## SD setup and stop-busy refit

Slower guest code now accounts for more SPI-driver setup and preparation of
write buffers. Reusing the previous peripheral overheads would count some of
that time twice. The following X3 defaults changed; all other SD timing
properties and panel waveform/power deadlines retain their previous values.

| Property | Previous | Current | Unit / meaning |
| --- | ---: | ---: | --- |
| GPSPI `transaction-overhead-ns` | 1450 | 700 | ns per transaction |
| GPSPI `buffer-overhead-ns` | 1875 | 300 | additional ns for transactions longer than one byte |
| SSI SD `read-next-us` | 12 | 9 | µs between CMD18 blocks |
| SSI SD `write-stop-decrement-us` | 26 | -40 | signed µs reduction per completed CMD25 block |
| SSI SD `write-stop-random-extra-us` | 205 | 100 | additional µs for a random-start CMD25 |

`write-stop-decrement-us` is now an int32 property. A negative value adds busy
time as the burst grows; positive values preserve the previous subtraction
behavior. STOP busy is clamped to 0…UINT32_MAX microseconds:

```
STOP busy = 480 + (random ? 100 : 0) - (-40 × completed blocks)
```

The old decreasing STOP deadline absorbed the faster emulator's driver timing.
The probe prepares its next write before waiting for STOP busy, and the longer
CPU preparation now hides more of that deadline. The positive burst-length
term restores the observed command residuals. This remains a deterministic
fit of operation medians, not a model of the card's long-tail write stalls.
The arithmetic also bounds negative-decrement growth before addition so
extreme property values cannot overflow the deadline calculation.

The unchanged SD probe completed all 2880 operations with zero errors on a
private 16 GiB card. All 30 medians pass: worst read is sequential 8 sectors,
**1458 vs 1404 µs (+3.85%)**; worst write is random 8 sectors,
**1550.5 vs 1746 µs (-11.20%)**. The supplied device table pools 288 samples per
row; this emulator run has 96 per row.

| Op | Pattern | Sectors | Device median (us) | Emulator median (us) | Error |
| --- | --- | ---: | ---: | ---: | ---: |
| r | rep | 1 | 301 | 303 | +0.66% |
| r | rep | 2 | 457 | 464 | +1.53% |
| r | rep | 4 | 770 | 785 | +1.95% |
| r | rep | 8 | 1398 | 1427 | +2.07% |
| r | rep | 32 | 5374 | 5287 | -1.62% |
| r | rnd | 1 | 398 | 400 | +0.50% |
| r | rnd | 2 | 555 | 561 | +1.08% |
| r | rnd | 4 | 876 | 885 | +1.03% |
| r | rnd | 8 | 1523.5 | 1527 | +0.23% |
| r | rnd | 32 | 5329.5 | 5380 | +0.95% |
| r | seq | 1 | 331 | 329 | -0.60% |
| r | seq | 2 | 482 | 490 | +1.66% |
| r | seq | 4 | 798 | 812.5 | +1.82% |
| r | seq | 8 | 1404 | 1458 | +3.85% |
| r | seq | 32 | 5307 | 5309 | +0.04% |
| w | rep | 1 | 671 | 677 | +0.89% |
| w | rep | 2 | 705 | 737 | +4.54% |
| w | rep | 4 | 913 | 975 | +6.79% |
| w | rep | 8 | 1352 | 1451 | +7.32% |
| w | rep | 32 | 5280 | 5317.5 | +0.71% |
| w | rnd | 1 | 813 | 816 | +0.37% |
| w | rnd | 2 | 810 | 837 | +3.33% |
| w | rnd | 4 | 1066 | 1075 | +0.84% |
| w | rnd | 8 | 1746 | 1550.5 | -11.20% |
| w | rnd | 32 | 5279 | 5318 | +0.74% |
| w | seq | 1 | 747 | 751 | +0.54% |
| w | seq | 2 | 748.5 | 737 | -1.54% |
| w | seq | 4 | 909 | 975 | +7.26% |
| w | seq | 8 | 1352 | 1452 | +7.40% |
| w | seq | 32 | 5280 | 5319 | +0.74% |

PASS: 30/30 rows; worst read 3.85%, write 11.20%.

## Reading and panel acceptance

Booted a fresh copy of the supplied CrossPoint develop image with the same
16384 MB FAT32 card content and 16-sector clusters used in the previous
calibration. Each session had its own flash and APFS card clone. Both Aesop and
Demian were opened through the emulator's private serial-control monitor;
six warm-up page turns preceded six measured turns, matching the earlier
reading workload. All measured turns advanced, both sessions returned Home,
and neither log reported a firmware error or reset.

Stage medians are milliseconds; each total is the median of complete page
renders, so it need not equal the sum of independently rounded stage medians.

| Stage | Aesop device | Aesop emulator | Demian device | Demian emulator |
| --- | ---: | ---: | ---: | ---: |
| prewarm | 32 | 31 | 114 | 114 |
| bw_render | 23 | 21.5 | 35 | 34.5 |
| display | 588 | 582 | 587 | 581.5 |
| gray_lsb | 88 | 83.5 | 108 | 101 |
| gray_msb | 86 | 83 | 108 | 102.5 |
| gray_display | 228 | 229 | 228 | 229 |
| cleanup | 102 | 95.5 | 102 | 96 |
| total | 1150 | 1125.5 | 1282 | 1257.5 |

Aesop totals are 2.13% faster than the device; Demian totals are 1.91% faster.
Demian BW rendering rises from the supplied plain-icount 13 ms to 34.5 ms,
**2.65x**, which is the requested 2.6–2.7x rendering target. Gray stages also
include SPI transfers, so their total stage time does not scale by the CPU
multiplier alone.

A new `uc8279_plane_write` trace measures command arrival through the final
byte of a complete DTM1/DTM2 window. At 160 MHz, full 52272-byte planes take
47.90 ms median in Aesop (5 samples, 47.46–48.12 ms) and 47.97 ms in Demian
(2 samples, 47.87–48.08 ms), against the device's 50.7 ms: about **5.5% fast**.
This observer adds no modeled delay and excludes the command byte's own wire
time. The residual remains; increasing the shared SPI setup enough to remove
it would worsen the SD fit.

PON remains a 127 ms deadline and the firmware observes 128 ms at boot.
DRF deadlines remain 227.950/382.150/484.950/934.700 ms for gray/fast/pre-BW/full;
reading logs observe roughly 228–230/382–384/485–487/935–936 ms because polling
and scheduling add a small delay. No panel timing property was changed.

## Clock scaling and accounting

The register model retains its original PLL 80 MHz reset state. Firmware
writes to SYSTEM_CPU_PER_CONF (0x600c0008) and SYSTEM_SYSCLK_CONF (0x600c0058)
select 160 MHz after boot and 10 MHz after the approximately three-second idle
period. Both reading logs contain `[PWR] Going to low-power mode`, and their
traces show 160 MHz/factor 1 → 10 MHz/factor 16; input restores 160 MHz through
a brief 40 MHz/factor 4 transition. The CPU probe independently reports
`[CPU] INFO cpu_mhz=160`.

The factor is `ceil(160 MHz / configured Hz)`. PLL supports 80/160 MHz;
XTAL uses 40 MHz divided by PRE_DIV_CNT+1; RC_FAST uses an approximate 17.5 MHz
source and rounds non-integral factors upward. The field interpretation follows
[Espressif's C3 clock implementation](https://github.com/espressif/esp-idf/blob/v5.5/components/hal/esp32c3/include/hal/clk_tree_ll.h).
A factor change flushes TBs and exits to the dispatcher. A regression test
executes the **same cached SRAM loop** at 160 and 10 MHz; its cycle-CSR/icount
tick deltas are 1,800,008 and 28,800,128, exactly **16x**, confirming the slower
charge and cache invalidation. These are icount tick observations, not a
claim that architectural cycle counters model the physical clock accurately.

`TranslatorOps.insn_cost` in `include/exec/translator.h` supplies the optional
cost hook. `accel/tcg/translator.c` accumulates `tb->icount_cost`, while
`tb->icount`, CF_COUNT_MASK, plugin counts, and unwind row counts still count
instructions. Per-instruction unwind costs refund unexecuted ticks on faults
and MMIO recompilation. Remaining tick budgets are converted back to instruction
limits; a final indivisible instruction may overrun a timer deadline, with a
signed borrow preserving the full charge. TB length is bounded so its cost
fits in 16 bits. RISC-V instruction-start parameters remain fault metadata.

## Reproduction and remaining limits

Both targets build with:

```
ninja -C build qemu-system-riscv32 qemu-system-xtensa
python3 eink-qemu/sdcal/cpu-evidence/verify.py \
  build/qemu-system-riscv32 /path/to/riscv32-esp-elf-gcc
```

The regression covers instruction classes, compressed loads/stores, one-insn
TBs, maximum costs, MMIO refunds, forced timer overruns, and the 16x clock
transition. Its current numeric results are in
[cpu-evidence/instruction-results.json](cpu-evidence/instruction-results.json).

Create each probe image by copying its supplied firmware to `firmware.bin`
alongside copies of `bootloader.bin` and `partitions.bin`, then running
`eink-qemu/mkflash.sh BUILD_DIR flash.bin`. Run
`python3 eink-qemu/sdcal/run_qemu.py "$PWD" OUTPUT_DIR` with `flash.bin` and
`sd.img` in the output directory inside this worktree; `run.sh` supplies the
correct X3 tick. CPU comparisons use the
supplied `cpu_parse.py --compare cpu-device.json`; SD comparisons use
`eink-qemu/sdcal/compare.py DEVICE_JSON console.log`.

A direct X3 launch uses:

```
build/qemu-system-riscv32 -machine x3 -icount shift=0,sleep=on \
  -drive file=flash-copy.bin,if=mtd,format=raw \
  -drive file=sd-copy.img,if=sd,format=raw \
  -chardev stdio,id=cdc -serial null \
  -global driver=misc.esp32s3.usb_serial_jtag,property=chardev,value=cdc \
  -display none -monitor none \
  -trace enable=esp32c3_cpu_clock -trace enable=uc8279_plane_write \
  -trace enable=uc8279_refresh -D timing.trace
```

New raw evidence is kept outside git in `cpu-calibration.local/`: CPU and SD
logs/parses in `cpu-defaults/` and `sd-defaults/`, six-page summaries and traces
in `read-defaults-aesop/` and `read-defaults-demian/`, and clock-loop artifacts
in `clock-scale.*`. Earlier candidate runs remain there for audit. The supplied
root-level CPU device files were read without modification. No physical device
was accessed and no other checkout was changed.

Remaining limits: the stated cached-flash and cache-thrash residuals, the 5.5%
plane-transfer residual, hardware cycle-counter accuracy, peripheral clock-tree
scaling, migration/record-replay, and non-C3 RISC-V extension timing. The optional
four-second Demian heap poll did not catch a MEM sample; memory retention was
not evaluated by this timing acceptance.
