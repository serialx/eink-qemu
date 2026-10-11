# QEMU 11.1 port review

Reviewed source: `49f40e8a82e56bb2824c3455ef256db524251a0f`, on 2026-10-09.
The comparison is the complete fork delta `v9.2.2..x4prosim-pre-rebase`
against `v11.1.2..49f40e8a82`. This is a completeness, behavior, API and build
review; it does not change implementation files.

## Findings ranked by severity

**One P1 regression is confirmed and reproduced.** No P0, P2 or P3 port
regressions were confirmed. Completeness and the fresh build pass, but the
port is not behaviorally faithful until the selective-reset regression is
fixed. No source fix was made during this review.

### P1 — Xtensa CPU-only resets now reset peripherals and RTC

**Locations:** `hw/xtensa/esp32.c:553` and `hw/xtensa/esp32s3.c:451`, with
legacy handlers installed at `hw/xtensa/esp32.c:661` and
`hw/xtensa/esp32s3.c:565`.

**Old behavior:** `qemu_register_reset` registered a leaf legacy-reset object
that called only `esp32_soc_reset` / `esp32s3_soc_reset`. Their
`requested_reset` checks selected the CPU, RTC and peripheral domains.
A PROCPU/APPCPU-only reset preserved UART registers and the unrelated buses.

**New behavior:** `qemu_register_resettable(OBJECT(dev))` registers the whole
SoC in the reset tree. The inherited `DeviceClass` Resettable implementation
walks every child bus (`hw/core/qdev.c:254`), including `periph_bus` and
`rtc_bus`. Resettable enters children and runs their hold callbacks before
the parent's hold callback (`hw/core/resettable.c:129`, `:155`). The
selective legacy handler therefore runs after devices outside the requested
domain have already reset. The old selective body is still present, but it
no longer controls those resets. This contradicts the preservation claim in
`eink-qemu/porting/machines.md:35`. ESP32, ESP32-S3 and the derived X4 Pro are
affected; the separate RISC-V CPU has no child buses and does not have this
specific traversal problem.

**Evidence:** diskless, paused qtest checks against the read-only reference
binary and this review's fresh build give the following results. No guest
instructions or firmware writes are needed.

| Machine | UART0 CLKDIV | RTC OPTIONS0 | Reset request | Before (both) | Old after | New after |
| --- | --- | --- | --- | --- | --- | --- |
| `esp32` | `0x3ff40014` | `0x3ff48000` | PROCPU `0x20` | `0x345` | `0x345` | `0x2b6` |
| `esp32s3` | `0x60000014` | `0x60008000` | PROCPU `0x20` | `0x345` | `0x345` | `0x2b6` |
| `esp32s3` | `0x60000014` | `0x60008000` | APPCPU `0x10` | `0x1234` | `0x1234` | `0x2b6` |

The last row was independently reproduced by the device audit. `0x2b6` is
the UART reset default, so this is an observed register-state regression,
not merely a reset API concern.

**Concrete fix:** preserve a leaf reset boundary for the SoC's selective
handler. For example, give each SoC a Resettable child traversal override
that deliberately does not traverse `periph_bus`/`rtc_bus`, leaving domain
resets to its existing selective handler, or register a dedicated leaf
Resettable wrapper that invokes that handler. Do not register the entire
ordinary DeviceState subtree while also relying on `requested_reset` gating.
Verify both CPU-only reset bits preserve a written UART register, and separately
verify full-system and peripheral-domain resets still clear the intended
registers. Update the machine note's preservation claim after that passes.

Reproduce the PROCPU rows from the worktree root:

```python
import pathlib, select, subprocess, time

old = "/Users/serialx/workspace/x4prosim/build/qemu-system-xtensa"
new = str(pathlib.Path("build/qemu-system-xtensa").resolve())
for machine, uart, rtc in [("esp32", 0x3ff40014, 0x3ff48000),
                           ("esp32s3", 0x60000014, 0x60008000)]:
    for label, binary in [("old", old), ("new", new)]:
        p = subprocess.Popen(
            [binary, "-M", machine, "-S", "-display", "none", "-serial", "none",
             "-monitor", "none", "-qtest", "stdio", "-qtest-log", "/dev/null"],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL, text=True, bufsize=1)
        def q(command):
            p.stdin.write(command + "\n")
            p.stdin.flush()
            assert select.select([p.stdout], [], [], 10)[0], command
            response = p.stdout.readline().strip()
            assert response.startswith("OK"), response
            return response
        try:
            q(f"writel {uart:#x} 0x345")
            before = q(f"readl {uart:#x}")
            q(f"writel {rtc:#x} 0x20")
            time.sleep(0.1)
            print(machine, label, before, "->", q(f"readl {uart:#x}"))
        finally:
            p.terminate()
            p.wait(timeout=10)
```

## Completeness

| Measure | Count |
| --- | ---: |
| Files in the original diff | 405 |
| Matched counterparts | 402 |
| Documented drops because upstream supplies the implementation | 3 |
| Undocumented omissions | 0 |

The original diff has 343 additions, 61 modifications and one deletion.
The 402 matches comprise 400 identical paths (including the preserved deletion
of `.github/workflows/lockdown.yml`) and two relocated interfaces. Of the 343
added files, 127 have identical Git blobs in both trees and 216 have edits.
A match establishes coverage, not semantic equivalence; behavior is reviewed
separately below. The full per-file accounting is in the appendix.

| Old path without a same-path new diff | Disposition and evidence |
| --- | --- |
| `include/exec/exec-all.h` | Matched: `tb_insns_for_ticks` moved to `accel/tcg/tb-internal.h`; documented in `core.md:25`. The obsolete umbrella header is absent upstream. |
| `target/riscv/translate.c` | Matched: implementation moved to `target/riscv/tcg/translate.c`; documented in `target-riscv.md:54`. |
| `block/file-win32.c` | Documented drop: upstream `0c9f429ec8` already relocates `qemu_ftruncate64`; `core.md`, Host and dependency changes. |
| `util/oslib-win32.c` | Documented drop: other half of the same upstream relocation; same evidence. |
| `python/scripts/mkvenv.py` | Documented drop: version matching and wheel-path changes are already upstream in `6ad034e712` and `587f4a1805`; `core.md`, Host and dependency changes. |

The new diff has 420 paths before this report: the 400 direct matches and 20
new paths. Those 20 are the two relocated interfaces, restored callback XTS
source/header, helper argument limit, CAN dependency, RISC-V TCG initialization,
SD initialization and denied-IOMMU integration fixes, and eleven porting notes.
None is an unexplained scratch artifact. Five ROM blobs are byte-identical to
the reference. Runner/calibration/data paths under `eink-qemu/` outside
`porting/`, and `test_qemu.sh`, have an empty old-to-new diff.

## Faithfulness: core and CPU targets

Every old hunk in the specified core and target paths was compared with the
new upstream-relative diff, direct old/new imported-file diffs, and the
applicable local upstream implementation. The coverage tables below identify
all of those hunks; absence of a finding does not certify silicon accuracy.

| Area | Compared semantics and result |
| --- | --- |
| Weighted icount | `accel/tcg/translator.c:63`, `:91`, `:206`, `:239`: asynchronous-bit mask, signed atomic borrow, positive default-one callback and cost-based debit retained. Instruction counts remain separate from tick costs. New CPUState-relative offsets follow the upstream translator. |
| Deadline/unwind | `accel/tcg/cpu-exec.c:779`, `:927`; `accel/tcg/translate-all.c:122`, `:139`, `:158`, `:203`: signed exhausted-budget check, whole-instruction budget conversion, atomic fallback flags, cost-column encode/decode, faulting-instruction refunds and borrow reversal retained. All decoder callers consume the extra column. |
| Helper arity | Four helper expansion headers retain eight-argument support; `include/tcg/helper-info.h:29` and `tcg/tcg.c:2595` correct old capacity gaps, and the wrapper now passes the actual function pointer. These are documented fixes, not weakened semantics. |
| SHA/HMAC/XTS | All 18 internal SHA/HMAC files preserve executable text after include/comment/whitespace normalization; constants, padding, state-word order and native-word HMAC output agree. Restored XTS source/header are byte-identical to the reference and still built unconditionally. |
| Static slirp | `meson.build:1282` changes forced static linkage to `prefer_static`; this changes the normal linkage default but is explicitly approved/documented in `core.md`. Windows CI retains `--static`, which sets `prefer_static=true`, and the conditional `LIBSLIRP_STATIC` define. Windows linking was not run here. |
| `dup_const` | Upstream's constant branch/invalid-width diagnostic replaces the old revert. The deliberate drop and host-limited evidence are recorded in `core.md`; no unrecorded change was found. |
| RISC-V property interface | `target/riscv/cpu.c:2848`: eight properties remain uint8, with identical spelling/defaults: `cost-rom-ns`, `cost-sram-ns`, `cost-flash-ns` = 1; `cost-load`, `cost-store`, `cost-mul`, `cost-div`, `cost-branch` = 0. Internal `cost_clock_scale` remains uint32, zero meaning 1x. |
| RISC-V regions/classification | `target/riscv/tcg/translate.c:1372`, `:1410`: identical compressed and full-width opcode predicates; classify the instruction start. Flash is `[0x42000000,0x42800000)`, ROM `[0x40000000,0x40060000)`, SRAM everywhere else. No endpoint widened or narrowed. |
| RISC-V cost arithmetic | `target/riscv/tcg/translate.c:1368`, `:1417`: `min(65535,(max(1,base)+extra)*max(1,scale))`, with multiplication widened to uint64 to fix old overflow. TB bound remains `65535/maximum_cost`. Python source-formula checks passed 393,216 base/extra/scale cases and eight address-boundary cases; these are arithmetic checks, not compiled guest tests. |
| RISC-V alias sharing | `target/riscv/tcg/tcg-cpu.c:1324` retains upstream `CF_PCREL` sharing only when effective region bases agree, preventing reuse with the wrong region cost. The clock path queues a TB flush after scale changes. |
| Espressif CPU/reset | `target/riscv/esp_cpu.c:314`, `:368`, `:375`: parent reset is chained before clearing Espressif IRQ/MIE state; reset registration is paired with unrealize unregistration. Direct CPU reset now also clears that extra state, an explicit documented conversion. RV32IMC, disabled Zawrs/Zfa, CSR/interrupt/divider behavior and false PMA/MIE-bitmap defaults survive. Debug CSR reads intentionally now return defined zero. |
| Xtensa generated cores | All six generated ISA/GDB/module files match after whitespace normalization. Names, 40,000 kHz clocks, opcode registration and register mappings survive. Both GDB environment variables retain present-and-not-`"0"` semantics, including empty values. |
| Xtensa TIE/helpers | Whole-file comparison of `translate_tie_esp32s3.c` matches after only obsolete includes, duplicate declaration and 248 no-op temporary frees are removed. All use TEMP_TB/TEMP_CONST, whose frees upstream ignores. Helper declarations are byte-identical; state layout and 16-byte alignment survive. The six DFP stubs remain no-ops. |
| Xtensa shared translator | Current upstream context was moved to the shared header; exported utilities keep their bodies, and floating-point wrappers now have reliable external definitions. Three-word instruction markers, upstream address handling and semihosting remain intact. Xtensa still costs one tick per instruction. |

Core coverage: **53 old hunks across 32 files**, matched by **57 new hunks
across 34 files**. Target coverage: **31 old hunks across 23 files**, matched
by **32 new hunks across 24 files**. Tables use default three-line Git hunks;
an added file counts as one hunk.

### Core hunk accounting

| File | Old hunks | New hunks | Old hunk ranges | Disposition |
| --- | ---: | ---: | --- | --- |
| `accel/tcg/cpu-exec.c` | 2 | 2 | `-781,7 +781,7`; `-934,11 +934,18` | Preserved / equivalent QEMU 11.1 API adaptation |
| `accel/tcg/tb-internal.h` | 0 | 1 | — | Replacement declaration |
| `accel/tcg/translate-all.c` | 7 | 7 | `-113,17 +113,18`; `-148,6 +149,7`; `-161,13 +163,34`; `-188,9 +211,14`; `-203,7 +231,8`; `-212,10 +241,15`; `-248,7 +282,7` | Preserved / equivalent QEMU 11.1 API adaptation |
| `accel/tcg/translator.c` | 8 | 9 | `-1,6 +1,13`; `-51,6 +58,12`; `-64,9 +77,8`; `-75,6 +87,23`; `-128,6 +157,7`; `-167,6 +197,10`; `-196,7 +230,7`; `-216,6 +250,7` | Preserved / equivalent QEMU 11.1 API adaptation |
| `crypto/hmac256-internal.c` | 1 | 1 | `-0,0 +1,90` | Executable text preserved |
| `crypto/hmac256_i.h` | 1 | 1 | `-0,0 +1,52` | Executable text preserved |
| `crypto/meson.build` | 2 | 1 | `-58,6 +58,15`; `-73,3 +82,4` | Same internal sources and unconditional XTS |
| `crypto/sha1-internal.c` | 1 | 1 | `-0,0 +1,227` | Executable text preserved |
| `crypto/sha1_i.h` | 1 | 1 | `-0,0 +1,21` | Executable text preserved |
| `crypto/sha224-internal.c` | 1 | 1 | `-0,0 +1,30` | Executable text preserved |
| `crypto/sha224_i.h` | 1 | 1 | `-0,0 +1,24` | Executable text preserved |
| `crypto/sha256-internal.c` | 1 | 1 | `-0,0 +1,107` | Executable text preserved |
| `crypto/sha256_i.h` | 1 | 1 | `-0,0 +1,20` | Executable text preserved |
| `crypto/sha384-internal.c` | 1 | 1 | `-0,0 +1,37` | Executable text preserved |
| `crypto/sha384_i.h` | 1 | 1 | `-0,0 +1,24` | Executable text preserved |
| `crypto/sha512-internal.c` | 1 | 1 | `-0,0 +1,148` | Executable text preserved |
| `crypto/sha512_224-internal.c` | 1 | 1 | `-0,0 +1,24` | Executable text preserved |
| `crypto/sha512_224_i.h` | 1 | 1 | `-0,0 +1,12` | Executable text preserved |
| `crypto/sha512_256-internal.c` | 1 | 1 | `-0,0 +1,27` | Executable text preserved |
| `crypto/sha512_256_i.h` | 1 | 1 | `-0,0 +1,12` | Executable text preserved |
| `crypto/sha512_i.h` | 1 | 1 | `-0,0 +1,20` | Executable text preserved |
| `crypto/sha512_t-internal.c` | 1 | 1 | `-0,0 +1,41` | Executable text preserved |
| `crypto/sha512_t_i.h` | 1 | 1 | `-0,0 +1,20` | Executable text preserved |
| `crypto/xts.c` | 0 | 1 | — | Restore upstream-removed callback implementation |
| `include/exec/exec-all.h` | 1 | 0 | `-29,6 +29,8` | Declaration moved to tb-internal.h |
| `include/exec/helper-gen.h.inc` | 2 | 2 | `-98,6 +98,21`; `-108,3 +123,4` | Retained; correct helper function pointer |
| `include/exec/helper-head.h.inc` | 1 | 1 | `-129,6 +129,9` | Preserved / equivalent QEMU 11.1 API adaptation |
| `include/exec/helper-info.c.inc` | 2 | 2 | `-83,6 +83,19`; `-94,3 +107,4` | Preserved / equivalent QEMU 11.1 API adaptation |
| `include/exec/helper-proto.h.inc` | 2 | 2 | `-51,6 +51,11`; `-65,4 +70,5` | Preserved / equivalent QEMU 11.1 API adaptation |
| `include/exec/translation-block.h` | 1 | 1 | `-87,7 +87,8` | Preserved / equivalent QEMU 11.1 API adaptation |
| `include/exec/translator.h` | 2 | 4 | `-85,6 +85,7`; `-132,6 +133,8` | Preserved / equivalent QEMU 11.1 API adaptation |
| `include/tcg/helper-info.h` | 0 | 1 | — | Correct old eight-argument capacity gap |
| `include/tcg/tcg.h` | 3 | 2 | `-545,6 +545,7`; `-851,6 +852,9`; `-1039,8 +1043,7` | Cost array/call8 retained; dup_const decision documented |
| `meson.build` | 1 | 1 | `-1217,8 +1217,20` | Approved static-link default change; Windows define retained |
| `tcg/tcg.c` | 1 | 2 | `-2385,6 +2385,15` | Retained call8; corrected extension bound |

### CPU target hunk accounting

| Old path (relative to target/) | Old hunks | New path / hunks | Disposition |
| --- | ---: | --- | --- |
| riscv/cpu.c | 1 | same / 1 | All property names, widths, defaults retained |
| riscv/cpu.h | 1 | same / 1 | Cost fields retained; explanatory comment added |
| riscv/esp_cpu.c | 1 | same / 1 | Entire new file audited through old/new diff; reset, CPU class, callbacks and CSR API adapted |
| riscv/esp_cpu.h | 1 | same / 1 | Public interface/state retained; new callback tables/reset phases and moved includes |
| riscv/meson.build | 1 | same / 1 | Both machine selectors retained; system/TCG scoping |
| riscv/translate.c | 2 | riscv/tcg/translate.c / 2 | Callback, TB bound and registration retained; 64-bit multiplication fix |
| — | 0 | riscv/tcg/tcg-cpu.c / 1 | New alias-sharing safeguard |
| xtensa/core-esp32.c | 1 | same / 1 | Include migration; behavior retained |
| xtensa/core-esp32/core-isa.h | 1 | same / 1 | Whitespace only |
| xtensa/core-esp32/gdb-config.inc.c | 1 | same / 1 | Whitespace-only comparison passes |
| xtensa/core-esp32/xtensa-modules.inc.c | 1 | same / 1 | Whitespace only |
| xtensa/core-esp32s3.c | 1 | same / 1 | Include migration; behavior retained |
| xtensa/core-esp32s3/core-isa.h | 1 | same / 1 | Whitespace only |
| xtensa/core-esp32s3/gdb-config.inc.c | 1 | same / 1 | Whitespace only |
| xtensa/core-esp32s3/xtensa-modules.inc.c | 1 | same / 1 | Whitespace only |
| xtensa/cores.list | 1 | same / 1 | All three source registrations retained |
| xtensa/cpu.h | 1 | same / 1 | Extension pointer retained |
| xtensa/cpu_esp32s3.h | 1 | same / 1 | State layout retained; guard/comments/style changes |
| xtensa/gdbstub.c | 1 | same / 1 | Window-register environment switch retained |
| xtensa/helper.c | 1 | same / 1 | Core-register environment switch retained |
| xtensa/helper.h | 1 | same / 1 | Byte-identical entire file |
| xtensa/translate.c | 8 | same / 8 | Context extraction, cpu_SR export, exception export, alignment export, DFP stub, six opcode registrations, output FP wrappers, input FP wrappers all retained |
| xtensa/translate.h | 1 | same / 1 | Current upstream context plus shared declarations; unused old field dropped |
| xtensa/translate_tie_esp32s3.c | 1 | same / 1 | Entire file equivalent after verified no-op/unused deletions |

### Integration and build-control checks

All original Kconfig, Meson, trace and default-machine additions were compared
as normalized added-line multisets, then each difference inspected. The Wi-Fi
source list is factored into one list with the same C3/S3 gates; GT911 now
selects its required I2C bus (documented); Espressif CPU compilation is scoped
to system/TCG with both original selectors. C3/C6 explicitly select SPI flash
dependencies; Xtensa select lists preserve the old devices. ROM installation
retains all five fork blobs. No missing source gate or trace declaration was
found in those comparisons or the fresh two-target link.

The new-only `hw/sd/sd.c:2301` transition to transfer state after SPI
CMD1/ACMD41 compensates for upstream's moved SPI protocol handling; it is
recorded in `machines.md`. `system/physmem.c:740` restores original CPU
permissions only when returning the unassigned region for an entirely denied
IOMMU translation, preserving the new TLB MMIO slow path without granting
access to denied backing memory. It is also documented with prior crash/guest
fault evidence. This review inspected both changes; it did not rerun that
firmware or denied-access regression test.

## Device faithfulness and API audit

Reviewed the old-to-new deltas for all **215 fork-added hw/include/hw files**
(114 C files and 101 headers); 35 are byte-identical. All **73 property macro
invocations** in the comparison, including SSI SD, retain their names, types,
defaults and macro contents after whitespace normalization and sentinel
removal. No `DEFINE_PROP_END_OF_LIST`, `DEFINE_PROP_PTR` or `CharBackend`
remains in these imported files.

All **28 old upstream-device hunks** are accounted for below: 17 SSI SD,
five m25p80 and six OpenCores. No undocumented timing, width, reset-value or
property change was found in these three patches.

### `hw/sd/ssi-sd.c`: all 17 original hunks

| Old hunk start | Current location | Disposition |
| --- | --- | --- |
| 1 | 1 | Complete latency documentation retained. |
| 24 | 33 | Timer/trace includes retained with current qdev-properties header. |
| 64 | 83 | All timing properties, deadlines, counters, histories, capacity and response flags retained. |
| 100 | 129 | Deadline helper and busy polling retained; upstream card-inserted guard remains ahead of busy polling. |
| 128 | 174 | Same-byte `dispatch` retained. |
| 158 | 206 | STOP_TRAN delay preserves signed decrement, random extra, saturation, and virtual-clock semantics. |
| 170 | 235 | Per-command deadline reset and CMD0 timing/history reset retained. |
| 180 | 247 | Old manual R3/R7 idle patch and fake CMD13/CSD conversion correctly superseded by native SPI framing in `hw/sd/sd.c:791`, `:810`, `:1909`, `:2558`. CMD58 CCS now reads bytes 1–4, after native R1. |
| 236 | 251 | Idle cache and accepted-command sector/write-pattern accounting retained. |
| 251 | 285 | Write data-response busy scheduling/accounting and repeat-before-sequential first-read latency retained. |
| 261 | 323 | Immediate data-token dispatch and read deadline polling retained. |
| 290 | 362 | Completed-read history and CMD18 next-block delay retained. |
| 317 | 397 | Write-response flag retained after completed write CRC. |
| 345 | 426 | Timing subsection remains v2/minimum v1 with the same fields and version gates. |
| 363 | 482 | Timing subsection registration retained; base VMState remains v7. |
| 387 | 509 | Reset values and all 11 latency property names/types/defaults retained, including signed decrement. |
| 398 | 557 | Property registration retained with const/count-based array and upstream reset helper. |

No missing fork hunk found. The old raw-response translation was appropriately removed because the upstream SPI card now returns framed SPI responses. The machine integration fix at `hw/sd/sd.c:2301` enters transfer state following SPI CMD1/ACMD41, addressing the initialization regression described in `machines.md`. The earlier storage-layer note requiring preparatory CMD9 is superseded by that integration change; it is not a remaining HEAD defect. This audit did not rerun the layer's full latency/migration tests.

### `hw/block/m25p80.c`: all five original hunks

| Old hunk start | Current location | Disposition |
| --- | --- | --- |
| 173 | 176 | One-byte Gigadevice continuous-read-mode constant retained. |
| 465 | 472 | Manufacturer enum retained. |
| 548 | 556 | JEDEC manufacturer `0xc8` mapping retained. |
| 1048 | 1130 | DIO consumes the additional mode byte. |
| 1095 | 1166 | QIO consumes one mode byte plus four dummy bytes, as in the fork. |

No finding. The new separate Gigadevice QIO case is necessary: upstream Winbond now consumes two dummy bytes, so restoring a shared Winbond/Gigadevice case would have introduced a regression. The current port correctly keeps the fork's Gigadevice behavior and upstream Winbond behavior.

### `hw/net/opencores_eth.c`: all six original hunks

| Old hunk start | Current location | Disposition |
| --- | --- | --- |
| 57 | 62 | DP83848C PHYSTS enum/bits and register count retained. |
| 73 | 82 | Link-up/down PHYSTS values retained. |
| 114 | 131 | PHYSTS read-only write handler retained. |
| 128 | 145 | Out-of-range MII reads return zero; tracing stays within the bounds check. |
| 343 | 350, 365 | Reset restores all six configured MAC bytes; declaration moved without behavior change. |
| 731 | 763, 769 | Default MAC generation and NIC info string retained. |

No finding. Upstream access-size restrictions, physical-memory helper renames, const property array, and legacy-reset helper remain intact. No fork NIC property or MAC default initialization was lost.

### API conclusions

| API area | Result against local QEMU 11.1 implementation |
| --- | --- |
| Reset ordering | The Xtensa SoC traversal is the confirmed P1 above. Leaf `device_class_set_legacy_reset` use is supported; retained hold-phase IRQ operations and TWAI parent chaining are valid. Wi-Fi's removed saved-parent structure was unused and its SysBus parent has no reset phases. DWC's helper installs the intended hold trampoline; the old direct legacy field assignment did not. |
| Properties | Const, counted arrays replace sentinels; empty sentinel-only arrays are removed. The 73 compared property calls have no interface/default differences. |
| IRQ ownership | qdev-created input/output handles and borrowed GPIO inputs remain valid. GDMA preserves its allocated IRQ handles across state reset. A privately allocated TWAI IRQ still lacks teardown, an inherited concern below. No newly introduced IRQ ownership regression was found. |
| Memory ownership | Device MMIO/RAM/ROM/alias regions retain owners. NULL owners in machine code intentionally mean machine lifetime, per `docs/devel/memory.rst`. No new owner substitution or premature destruction was found. Private AddressSpace teardown remains an inherited concern below. |
| Chardev | `CharFrontend` correctly replaces `CharBackend`; callback signatures, watches and registration remain compatible. `DEFINE_PROP_CHR` release already calls `qemu_chr_fe_deinit`; a second deinit is not required merely because no explicit device deinit exists. The inherited UART timeout reset gap is separate. |
| Display/input | Synchronous display callbacks correctly return true under the new API; surface helper changes preserve use. Embedded QemuInputEvent union and Linux-keycode conversion preserve mappings. Key-handler unregister additions improve teardown. |
| DMA/clock | DWC command-buffer bounds and `size_t` API use agree with the new SD interface. The C3 clock's queued TB flush preserves the existing exit-TB request and avoids main-thread synchronous-flush assertions. |

These checks used local `docs/devel/reset.rst`, `docs/devel/memory.rst`,
`hw/core/qdev.c`, `hw/core/resettable.c`, property/IRQ implementations,
`include/chardev/char-fe.h`, `chardev/char-fe.c`, and UI input/display APIs.
Compilation alone was not treated as proof of reset or lifetime semantics.

### Inherited concerns, separate from port regressions

The following relevant logic is unchanged from the old fork. These are
source-level follow-up findings, not reasons to claim the port omitted or
weakened that logic. The reset-timer and ownership concerns were not exercised
dynamically, and the Wi-Fi issue was not exploited.

| Severity | Location | Old and new behavior | Concrete fix |
| --- | --- | --- | --- |
| P1, inherited | `hw/misc/esp32s3_wifi.c:132` | Both copy guest-controlled 12-bit TX descriptor length into a fixed stack `mac80211_frame`; 4095 bytes exceeds its headers plus 2316-byte payload, allowing host stack overwrite. | Reject lengths above wire-frame capacity or below parser minimum, initialize the frame, and check DMA success before parsing. Test rejection without performing an oversized copy. |
| P2, inherited | `hw/char/esp32_uart.c:330` | Both cancel throttle_timer but leave rx_timeout_timer and rxfifo_tout intact; a pre-reset callback can restore stale RX timeout state after reset. | Cancel rx_timeout_timer and clear its latch during reset; check both armed and already-latched cases. |
| P3, inherited | `hw/net/can/esp32_twai.c:129` and `:136` | Both allocate a private IRQ and connect a CAN client without matching unrealize/finalize or error unwind; repeated realization/disposal can leak or retain stale device references. | Disconnect with can_sja_disconnect before qemu_free_irq, and match cleanup to successful/failed realization lifetime. |
| P3, inherited | `hw/dma/esp_gdma.c:967`, `hw/display/esp_rgb.c:268`, `:308`, `hw/misc/esp32s3_cache.c:311`, `:316` | Both initialize private AddressSpaces without corresponding destruction; owner-based MemoryRegion cleanup does not release AddressSpace root references/global-list membership. RGB allocates one even when S3 later unparents the unused RGB device. | Define matching AddressSpace teardown/error unwind, avoid initialization for unused unrealized RGB, and keep embedded storage valid through deferred destruction. Test repeated realization/teardown. |

## Build and hygiene evidence

A fresh local configuration and build passed. The initial sandboxed configure
failed resolving Python packages from PyPI; the retry used network access and
explicitly selected the Homebrew Python described in `host.md`, because the
terminal otherwise selected the PlatformIO Python. This is a review-local
configuration choice; no host or source file was edited.

```sh
mkdir -p build
cd build
../configure --python=/opt/homebrew/bin/python3 \
  --target-list=xtensa-softmmu,riscv32-softmmu --enable-gcrypt --enable-sdl \
  --enable-slirp --disable-gnutls --disable-strip --disable-user --disable-capstone \
  --disable-vnc --disable-gtk --disable-docs
cd ..
ninja -C build qemu-system-xtensa qemu-system-riscv32
```

```text
[1820/1825] Linking target qemu-system-xtensa-unsigned
[1821/1825] Generating qemu-system-xtensa with a custom command
[1824/1825] Linking target qemu-system-riscv32-unsigned
[1825/1825] Generating qemu-system-riscv32 with a custom command
exit status: 0
compiler/linker warnings: 0

$ ./build/qemu-system-riscv32 --version
QEMU emulator version 11.1.2 (v11.1.2-18-g49f40e8a82)
$ ./build/qemu-system-xtensa --version
QEMU emulator version 11.1.2 (v11.1.2-18-g49f40e8a82)
```

Local logs: `build/configure-review.log`, `build/configure-review-network.log`
and `build/review-ninja.log` (ignored build artifacts). Configure repeats the
baseline's unavailable static iconv warning and PIE-disable notice; neither
is a new compiler/linker warning. This review does not claim a Windows/Linux
CI run, full test-suite run or a fresh firmware timing calibration.

Across all 246 fork-added C/header paths, TODO/FIXME occurrences remain 11 in
both trees and printf/fprintf/puts/g_print call occurrences remain 91. A
whitespace-insensitive old-to-new diff finds no added TODO/FIXME/XXX or print
statement in those paths. Examples of retained diagnostics are
`hw/misc/esp32_wifi_ap.c:129` and `hw/misc/esp32s3_wifi.c:91`; retained UART
limitations are `hw/char/esp32_uart.c:99` and `:293`. These are inherited code,
not unfinished port changes. No new scratch file is tracked or left untracked.

`git diff --check v11.1.2..49f40e8a82` reports only the inherited trailing
space in `eink-qemu/testdata/device/boot-reader-1002p.txt:43`; that raw evidence
file is byte-identical to the reference. It is not a behavior finding.

## Appendix: complete old-diff inventory

This inventory and its commands record the reviewed revisions before the
project rename; paths here retain their historical names.

`M` means modified, `A` added, and `D` deleted in the original diff.
“Same path” means that exact path also occurs in the new upstream-relative
diff; the table explicitly identifies every exception.

| Old status | Old path | New counterpart/disposition |
| --- | --- | --- |
| A | `.github/workflows/build.yml` | Same path |
| D | `.github/workflows/lockdown.yml` | Deletion preserved |
| A | `.github/workflows/scripts/bundle-win-dlls.sh` | Same path |
| A | `.github/workflows/scripts/configure-cross-linux-arm64.sh` | Same path |
| A | `.github/workflows/scripts/configure-macos.sh` | Same path |
| A | `.github/workflows/scripts/configure-native.sh` | Same path |
| A | `.github/workflows/scripts/configure-win.sh` | Same path |
| A | `.github/workflows/scripts/install-native.sh` | Same path |
| A | `.github/workflows/scripts/prerequisites-cross-linux-arm64.sh` | Same path |
| A | `.github/workflows/scripts/prerequisites-macos.sh` | Same path |
| A | `.github/workflows/scripts/prerequisites-native.sh` | Same path |
| A | `.github/workflows/scripts/prerequisites-old.sh` | Same path |
| A | `.github/workflows/sync-jira.yml` | Same path |
| M | `.gitlab-ci.yml` | Same path |
| A | `README.md` | Same path |
| A | `X4PROSIM.md` | Same path |
| M | `accel/tcg/cpu-exec.c` | Same path |
| M | `accel/tcg/translate-all.c` | Same path |
| M | `accel/tcg/translator.c` | Same path |
| M | `block/file-win32.c` | Documented upstream replacement; see completeness table |
| M | `configs/devices/riscv32-softmmu/default.mak` | Same path |
| M | `configs/devices/xtensa-softmmu/default.mak` | Same path |
| A | `crypto/hmac256-internal.c` | Same path |
| A | `crypto/hmac256_i.h` | Same path |
| M | `crypto/meson.build` | Same path |
| A | `crypto/sha1-internal.c` | Same path |
| A | `crypto/sha1_i.h` | Same path |
| A | `crypto/sha224-internal.c` | Same path |
| A | `crypto/sha224_i.h` | Same path |
| A | `crypto/sha256-internal.c` | Same path |
| A | `crypto/sha256_i.h` | Same path |
| A | `crypto/sha384-internal.c` | Same path |
| A | `crypto/sha384_i.h` | Same path |
| A | `crypto/sha512-internal.c` | Same path |
| A | `crypto/sha512_224-internal.c` | Same path |
| A | `crypto/sha512_224_i.h` | Same path |
| A | `crypto/sha512_256-internal.c` | Same path |
| A | `crypto/sha512_256_i.h` | Same path |
| A | `crypto/sha512_i.h` | Same path |
| A | `crypto/sha512_t-internal.c` | Same path |
| A | `crypto/sha512_t_i.h` | Same path |
| M | `hw/block/m25p80.c` | Same path |
| A | `hw/char/esp32_uart.c` | Same path |
| A | `hw/char/esp32c3_uart.c` | Same path |
| A | `hw/char/esp32c6_uart.c` | Same path |
| A | `hw/char/esp32s3_uart.c` | Same path |
| M | `hw/char/meson.build` | Same path |
| M | `hw/display/Kconfig` | Same path |
| A | `hw/display/esp_rgb.c` | Same path |
| M | `hw/display/meson.build` | Same path |
| A | `hw/display/ssd1677.c` | Same path |
| M | `hw/display/trace-events` | Same path |
| A | `hw/display/uc8179.c` | Same path |
| A | `hw/display/uc8279.c` | Same path |
| A | `hw/dma/esp32c3_gdma.c` | Same path |
| A | `hw/dma/esp32c6_gdma.c` | Same path |
| A | `hw/dma/esp32s3_gdma.c` | Same path |
| A | `hw/dma/esp_gdma.c` | Same path |
| M | `hw/dma/meson.build` | Same path |
| A | `hw/gpio/esp32_gpio.c` | Same path |
| A | `hw/gpio/esp32c3_gpio.c` | Same path |
| A | `hw/gpio/esp32c6_gpio.c` | Same path |
| A | `hw/gpio/esp32s3_gpio.c` | Same path |
| M | `hw/gpio/meson.build` | Same path |
| A | `hw/i2c/esp32_i2c.c` | Same path |
| A | `hw/i2c/esp32s3_i2c.c` | Same path |
| M | `hw/i2c/meson.build` | Same path |
| M | `hw/input/Kconfig` | Same path |
| A | `hw/input/gt911.c` | Same path |
| M | `hw/input/meson.build` | Same path |
| A | `hw/input/x3_keys.c` | Same path |
| A | `hw/input/x4pro_keys.c` | Same path |
| M | `hw/misc/Kconfig` | Same path |
| A | `hw/misc/bq27220.c` | Same path |
| A | `hw/misc/cw2017.c` | Same path |
| A | `hw/misc/esp32_aes.c` | Same path |
| A | `hw/misc/esp32_crosscore_int.c` | Same path |
| A | `hw/misc/esp32_dport.c` | Same path |
| A | `hw/misc/esp32_fe.c` | Same path |
| A | `hw/misc/esp32_flash_enc.c` | Same path |
| A | `hw/misc/esp32_ledc.c` | Same path |
| A | `hw/misc/esp32_phya.c` | Same path |
| A | `hw/misc/esp32_rng.c` | Same path |
| A | `hw/misc/esp32_rsa.c` | Same path |
| A | `hw/misc/esp32_rtc_cntl.c` | Same path |
| A | `hw/misc/esp32_sha.c` | Same path |
| A | `hw/misc/esp32_wifi_ap.c` | Same path |
| A | `hw/misc/esp32_wlan.h` | Same path |
| A | `hw/misc/esp32_wlan_packet.c` | Same path |
| A | `hw/misc/esp32_wlan_packet.h` | Same path |
| A | `hw/misc/esp32c3_aes.c` | Same path |
| A | `hw/misc/esp32c3_cache.c` | Same path |
| A | `hw/misc/esp32c3_ds.c` | Same path |
| A | `hw/misc/esp32c3_hmac.c` | Same path |
| A | `hw/misc/esp32c3_jtag.c` | Same path |
| A | `hw/misc/esp32c3_rsa.c` | Same path |
| A | `hw/misc/esp32c3_rtc_cntl.c` | Same path |
| A | `hw/misc/esp32c3_saradc.c` | Same path |
| A | `hw/misc/esp32c3_sha.c` | Same path |
| A | `hw/misc/esp32c3_xts_aes.c` | Same path |
| A | `hw/misc/esp32c6_cache.c` | Same path |
| A | `hw/misc/esp32c6_i2c_ana_mst.c` | Same path |
| A | `hw/misc/esp32c6_intpri.c` | Same path |
| A | `hw/misc/esp32c6_jtag.c` | Same path |
| A | `hw/misc/esp32c6_lp.c` | Same path |
| A | `hw/misc/esp32c6_modem.c` | Same path |
| A | `hw/misc/esp32c6_pcr.c` | Same path |
| A | `hw/misc/esp32c6_sha.c` | Same path |
| A | `hw/misc/esp32c6_spi_mem.c` | Same path |
| A | `hw/misc/esp32s3_aes.c` | Same path |
| A | `hw/misc/esp32s3_ana.c` | Same path |
| A | `hw/misc/esp32s3_cache.c` | Same path |
| A | `hw/misc/esp32s3_ds.c` | Same path |
| A | `hw/misc/esp32s3_hmac.c` | Same path |
| A | `hw/misc/esp32s3_pms.c` | Same path |
| A | `hw/misc/esp32s3_regstub.c` | Same path |
| A | `hw/misc/esp32s3_rng.c` | Same path |
| A | `hw/misc/esp32s3_rsa.c` | Same path |
| A | `hw/misc/esp32s3_rtc_cntl.c` | Same path |
| A | `hw/misc/esp32s3_sens.c` | Same path |
| A | `hw/misc/esp32s3_sha.c` | Same path |
| A | `hw/misc/esp32s3_usb_jtag.c` | Same path |
| A | `hw/misc/esp32s3_wifi.c` | Same path |
| A | `hw/misc/esp32s3_xts_aes.c` | Same path |
| A | `hw/misc/esp_aes.c` | Same path |
| A | `hw/misc/esp_ds.c` | Same path |
| A | `hw/misc/esp_hmac.c` | Same path |
| A | `hw/misc/esp_rsa.c` | Same path |
| A | `hw/misc/esp_sha.c` | Same path |
| M | `hw/misc/meson.build` | Same path |
| A | `hw/misc/qmi8658.c` | Same path |
| A | `hw/misc/ssi_psram.c` | Same path |
| A | `hw/net/can/esp32_twai.c` | Same path |
| A | `hw/net/can/esp32_twai.h` | Same path |
| A | `hw/net/can/esp32c3_twai.c` | Same path |
| A | `hw/net/can/esp32c3_twai.h` | Same path |
| A | `hw/net/can/esp32s3_twai.c` | Same path |
| A | `hw/net/can/esp32s3_twai.h` | Same path |
| M | `hw/net/can/meson.build` | Same path |
| M | `hw/net/opencores_eth.c` | Same path |
| A | `hw/nvram/esp32_efuse.c` | Same path |
| A | `hw/nvram/esp32c3_efuse.c` | Same path |
| A | `hw/nvram/esp32c6_efuse.c` | Same path |
| A | `hw/nvram/esp32s3_efuse.c` | Same path |
| A | `hw/nvram/esp_efuse.c` | Same path |
| M | `hw/nvram/meson.build` | Same path |
| M | `hw/riscv/Kconfig` | Same path |
| A | `hw/riscv/esp32c3.c` | Same path |
| A | `hw/riscv/esp32c3_clk.c` | Same path |
| A | `hw/riscv/esp32c3_intmatrix.c` | Same path |
| A | `hw/riscv/esp32c6.c` | Same path |
| A | `hw/riscv/esp32c6_clk.c` | Same path |
| A | `hw/riscv/esp32c6_intmatrix.c` | Same path |
| M | `hw/riscv/meson.build` | Same path |
| M | `hw/riscv/trace-events` | Same path |
| M | `hw/rtc/Kconfig` | Same path |
| A | `hw/rtc/bm8563.c` | Same path |
| A | `hw/rtc/ds3231.c` | Same path |
| M | `hw/rtc/meson.build` | Same path |
| M | `hw/sd/Kconfig` | Same path |
| A | `hw/sd/dwc_sdmmc.c` | Same path |
| M | `hw/sd/meson.build` | Same path |
| M | `hw/sd/ssi-sd.c` | Same path |
| M | `hw/sd/trace-events` | Same path |
| A | `hw/ssi/esp32_spi.c` | Same path |
| A | `hw/ssi/esp32c3_spi.c` | Same path |
| A | `hw/ssi/esp32c6_spi.c` | Same path |
| A | `hw/ssi/esp32s3_gpspi.c` | Same path |
| A | `hw/ssi/esp32s3_spi.c` | Same path |
| M | `hw/ssi/meson.build` | Same path |
| M | `hw/ssi/trace-events` | Same path |
| A | `hw/timer/esp32_frc_timer.c` | Same path |
| A | `hw/timer/esp32_timg.c` | Same path |
| A | `hw/timer/esp32c3_systimer.c` | Same path |
| A | `hw/timer/esp32c3_timg.c` | Same path |
| A | `hw/timer/esp32c6_systimer.c` | Same path |
| A | `hw/timer/esp32c6_timg.c` | Same path |
| A | `hw/timer/esp32s3_systimer.c` | Same path |
| A | `hw/timer/esp32s3_timg.c` | Same path |
| A | `hw/timer/esp_systimer.c` | Same path |
| A | `hw/timer/esp_timg.c` | Same path |
| M | `hw/timer/meson.build` | Same path |
| M | `hw/timer/trace-events` | Same path |
| M | `hw/xtensa/Kconfig` | Same path |
| A | `hw/xtensa/esp32.c` | Same path |
| A | `hw/xtensa/esp32_intc.c` | Same path |
| A | `hw/xtensa/esp32s3.c` | Same path |
| A | `hw/xtensa/esp32s3_clk.c` | Same path |
| A | `hw/xtensa/esp32s3_intc.c` | Same path |
| M | `hw/xtensa/meson.build` | Same path |
| M | `include/exec/exec-all.h` | `accel/tcg/tb-internal.h` |
| M | `include/exec/helper-gen.h.inc` | Same path |
| M | `include/exec/helper-head.h.inc` | Same path |
| M | `include/exec/helper-info.c.inc` | Same path |
| M | `include/exec/helper-proto.h.inc` | Same path |
| M | `include/exec/translation-block.h` | Same path |
| M | `include/exec/translator.h` | Same path |
| A | `include/hw/char/esp32_uart.h` | Same path |
| A | `include/hw/char/esp32c3_uart.h` | Same path |
| A | `include/hw/char/esp32c6_uart.h` | Same path |
| A | `include/hw/char/esp32s3_uart.h` | Same path |
| A | `include/hw/display/esp_rgb.h` | Same path |
| A | `include/hw/dma/esp32c3_gdma.h` | Same path |
| A | `include/hw/dma/esp32c6_gdma.h` | Same path |
| A | `include/hw/dma/esp32s3_gdma.h` | Same path |
| A | `include/hw/dma/esp_gdma.h` | Same path |
| A | `include/hw/gpio/esp32_gpio.h` | Same path |
| A | `include/hw/gpio/esp32c3_gpio.h` | Same path |
| A | `include/hw/gpio/esp32c6_gpio.h` | Same path |
| A | `include/hw/gpio/esp32s3_gpio.h` | Same path |
| A | `include/hw/i2c/esp32_i2c.h` | Same path |
| A | `include/hw/misc/esp32_aes.h` | Same path |
| A | `include/hw/misc/esp32_crosscore_int.h` | Same path |
| A | `include/hw/misc/esp32_dport.h` | Same path |
| A | `include/hw/misc/esp32_fe.h` | Same path |
| A | `include/hw/misc/esp32_flash_enc.h` | Same path |
| A | `include/hw/misc/esp32_ledc.h` | Same path |
| A | `include/hw/misc/esp32_phya.h` | Same path |
| A | `include/hw/misc/esp32_reg.h` | Same path |
| A | `include/hw/misc/esp32_rng.h` | Same path |
| A | `include/hw/misc/esp32_rsa.h` | Same path |
| A | `include/hw/misc/esp32_rtc_cntl.h` | Same path |
| A | `include/hw/misc/esp32_sha.h` | Same path |
| A | `include/hw/misc/esp32_wifi.h` | Same path |
| A | `include/hw/misc/esp32c3_aes.h` | Same path |
| A | `include/hw/misc/esp32c3_cache.h` | Same path |
| A | `include/hw/misc/esp32c3_ds.h` | Same path |
| A | `include/hw/misc/esp32c3_hmac.h` | Same path |
| A | `include/hw/misc/esp32c3_jtag.h` | Same path |
| A | `include/hw/misc/esp32c3_reg.h` | Same path |
| A | `include/hw/misc/esp32c3_rsa.h` | Same path |
| A | `include/hw/misc/esp32c3_rtc_cntl.h` | Same path |
| A | `include/hw/misc/esp32c3_sha.h` | Same path |
| A | `include/hw/misc/esp32c3_xts_aes.h` | Same path |
| A | `include/hw/misc/esp32c6_cache.h` | Same path |
| A | `include/hw/misc/esp32c6_i2c_ana_mst.h` | Same path |
| A | `include/hw/misc/esp32c6_intpri.h` | Same path |
| A | `include/hw/misc/esp32c6_jtag.h` | Same path |
| A | `include/hw/misc/esp32c6_lp.h` | Same path |
| A | `include/hw/misc/esp32c6_modem.h` | Same path |
| A | `include/hw/misc/esp32c6_pcr.h` | Same path |
| A | `include/hw/misc/esp32c6_reg.h` | Same path |
| A | `include/hw/misc/esp32c6_sha.h` | Same path |
| A | `include/hw/misc/esp32c6_spi_mem.h` | Same path |
| A | `include/hw/misc/esp32s3_aes.h` | Same path |
| A | `include/hw/misc/esp32s3_ana.h` | Same path |
| A | `include/hw/misc/esp32s3_cache.h` | Same path |
| A | `include/hw/misc/esp32s3_ds.h` | Same path |
| A | `include/hw/misc/esp32s3_hmac.h` | Same path |
| A | `include/hw/misc/esp32s3_pms.h` | Same path |
| A | `include/hw/misc/esp32s3_reg.h` | Same path |
| A | `include/hw/misc/esp32s3_rng.h` | Same path |
| A | `include/hw/misc/esp32s3_rsa.h` | Same path |
| A | `include/hw/misc/esp32s3_rtc_cntl.h` | Same path |
| A | `include/hw/misc/esp32s3_sha.h` | Same path |
| A | `include/hw/misc/esp32s3_wifi.h` | Same path |
| A | `include/hw/misc/esp32s3_xts_aes.h` | Same path |
| A | `include/hw/misc/esp_aes.h` | Same path |
| A | `include/hw/misc/esp_ds.h` | Same path |
| A | `include/hw/misc/esp_hmac.h` | Same path |
| A | `include/hw/misc/esp_rsa.h` | Same path |
| A | `include/hw/misc/esp_sha.h` | Same path |
| A | `include/hw/misc/ssi_psram.h` | Same path |
| A | `include/hw/nvram/esp32_efuse.h` | Same path |
| A | `include/hw/nvram/esp32c3_efuse.h` | Same path |
| A | `include/hw/nvram/esp32c6_efuse.h` | Same path |
| A | `include/hw/nvram/esp32s3_efuse.h` | Same path |
| A | `include/hw/nvram/esp_efuse.h` | Same path |
| A | `include/hw/riscv/esp32c3_clk.h` | Same path |
| A | `include/hw/riscv/esp32c3_clk_defs.h` | Same path |
| A | `include/hw/riscv/esp32c3_intmatrix.h` | Same path |
| A | `include/hw/riscv/esp32c6_clk.h` | Same path |
| A | `include/hw/riscv/esp32c6_intmatrix.h` | Same path |
| A | `include/hw/sd/dwc_sdmmc.h` | Same path |
| A | `include/hw/ssi/esp32_spi.h` | Same path |
| A | `include/hw/ssi/esp32c3_spi.h` | Same path |
| A | `include/hw/ssi/esp32c6_spi.h` | Same path |
| A | `include/hw/ssi/esp32s3_spi.h` | Same path |
| A | `include/hw/timer/esp32_frc_timer.h` | Same path |
| A | `include/hw/timer/esp32_timg.h` | Same path |
| A | `include/hw/timer/esp32c3_systimer.h` | Same path |
| A | `include/hw/timer/esp32c3_timg.h` | Same path |
| A | `include/hw/timer/esp32c6_systimer.h` | Same path |
| A | `include/hw/timer/esp32c6_timg.h` | Same path |
| A | `include/hw/timer/esp32s3_systimer.h` | Same path |
| A | `include/hw/timer/esp32s3_timg.h` | Same path |
| A | `include/hw/timer/esp_systimer.h` | Same path |
| A | `include/hw/timer/esp_timg.h` | Same path |
| A | `include/hw/xtensa/esp32.h` | Same path |
| A | `include/hw/xtensa/esp32_intc.h` | Same path |
| A | `include/hw/xtensa/esp32s3_clk.h` | Same path |
| A | `include/hw/xtensa/esp32s3_clk_defs.h` | Same path |
| A | `include/hw/xtensa/esp32s3_intc.h` | Same path |
| M | `include/tcg/tcg.h` | Same path |
| M | `meson.build` | Same path |
| A | `pc-bios/esp32-v3-rom-app.bin` | Same path |
| A | `pc-bios/esp32-v3-rom.bin` | Same path |
| A | `pc-bios/esp32c3-rom.bin` | Same path |
| A | `pc-bios/esp32c6-rom.bin` | Same path |
| A | `pc-bios/esp32s3_rev0_rom.bin` | Same path |
| M | `pc-bios/meson.build` | Same path |
| M | `python/scripts/mkvenv.py` | Documented upstream replacement; see completeness table |
| M | `target/riscv/cpu.c` | Same path |
| M | `target/riscv/cpu.h` | Same path |
| A | `target/riscv/esp_cpu.c` | Same path |
| A | `target/riscv/esp_cpu.h` | Same path |
| M | `target/riscv/meson.build` | Same path |
| M | `target/riscv/translate.c` | `target/riscv/tcg/translate.c` |
| A | `target/xtensa/core-esp32.c` | Same path |
| A | `target/xtensa/core-esp32/core-isa.h` | Same path |
| A | `target/xtensa/core-esp32/gdb-config.inc.c` | Same path |
| A | `target/xtensa/core-esp32/xtensa-modules.inc.c` | Same path |
| A | `target/xtensa/core-esp32s3.c` | Same path |
| A | `target/xtensa/core-esp32s3/core-isa.h` | Same path |
| A | `target/xtensa/core-esp32s3/gdb-config.inc.c` | Same path |
| A | `target/xtensa/core-esp32s3/xtensa-modules.inc.c` | Same path |
| M | `target/xtensa/cores.list` | Same path |
| M | `target/xtensa/cpu.h` | Same path |
| A | `target/xtensa/cpu_esp32s3.h` | Same path |
| M | `target/xtensa/gdbstub.c` | Same path |
| M | `target/xtensa/helper.c` | Same path |
| M | `target/xtensa/helper.h` | Same path |
| M | `target/xtensa/translate.c` | Same path |
| A | `target/xtensa/translate.h` | Same path |
| A | `target/xtensa/translate_tie_esp32s3.c` | Same path |
| M | `tcg/tcg.c` | Same path |
| A | `test_qemu.sh` | Same path |
| M | `util/oslib-win32.c` | Documented upstream replacement; see completeness table |
| A | `x4prosim/drive.py` | Same path |
| A | `x4prosim/ghosting/README.md` | Same path |
| A | `x4prosim/ghosting/device-photos/keyboard-type-delete-pep.jpg` | Same path |
| A | `x4prosim/ghosting/device-photos/series-1.jpg` | Same path |
| A | `x4prosim/ghosting/device-photos/series-10.jpg` | Same path |
| A | `x4prosim/ghosting/device-photos/series-2.jpg` | Same path |
| A | `x4prosim/ghosting/device-photos/series-3.jpg` | Same path |
| A | `x4prosim/ghosting/device-photos/series-4.jpg` | Same path |
| A | `x4prosim/ghosting/device-photos/series-5.jpg` | Same path |
| A | `x4prosim/ghosting/device-photos/series-6.jpg` | Same path |
| A | `x4prosim/ghosting/device-photos/series-7.jpg` | Same path |
| A | `x4prosim/ghosting/device-photos/series-8.jpg` | Same path |
| A | `x4prosim/ghosting/device-photos/series-9.jpg` | Same path |
| A | `x4prosim/ghosting/device-photos/series-steps.txt` | Same path |
| A | `x4prosim/ghosting/refresh-animation.md` | Same path |
| A | `x4prosim/ghosting/sim-screenshots/01-old-model-keyboard-exit-ghost.png` | Same path |
| A | `x4prosim/ghosting/sim-screenshots/02-lut-model-keyboard-exit-clean.png` | Same path |
| A | `x4prosim/ghosting/sim-screenshots/03-photo-vs-sim-type-delete-pep.png` | Same path |
| A | `x4prosim/ghosting/sim-screenshots/04-series-step4-vs-step10.png` | Same path |
| A | `x4prosim/ghosting/sim-screenshots/05-menus-final-clean.png` | Same path |
| A | `x4prosim/ghosting/sim-screenshots/06-epub-direct-gray-page2.png` | Same path |
| A | `x4prosim/ghosting/sim-screenshots/07-battery-stats-after-tsens-fix.png` | Same path |
| A | `x4prosim/ghosting/sim-screenshots/photo-series-replay/step-1.png` | Same path |
| A | `x4prosim/ghosting/sim-screenshots/photo-series-replay/step-10.png` | Same path |
| A | `x4prosim/ghosting/sim-screenshots/photo-series-replay/step-2.png` | Same path |
| A | `x4prosim/ghosting/sim-screenshots/photo-series-replay/step-3.png` | Same path |
| A | `x4prosim/ghosting/sim-screenshots/photo-series-replay/step-4.png` | Same path |
| A | `x4prosim/ghosting/sim-screenshots/photo-series-replay/step-5.png` | Same path |
| A | `x4prosim/ghosting/sim-screenshots/photo-series-replay/step-6.png` | Same path |
| A | `x4prosim/ghosting/sim-screenshots/photo-series-replay/step-7.png` | Same path |
| A | `x4prosim/ghosting/sim-screenshots/photo-series-replay/step-8.png` | Same path |
| A | `x4prosim/ghosting/sim-screenshots/photo-series-replay/step-9.png` | Same path |
| A | `x4prosim/ghosting/sim-screenshots/refresh-animation/fast-refresh-selection-move-frames.png` | Same path |
| A | `x4prosim/ghosting/sim-screenshots/refresh-animation/home-after-keypress-rebased-build.png` | Same path |
| A | `x4prosim/ghosting/sim-screenshots/refresh-animation/page-turn-direct-gray-flash-frames.png` | Same path |
| A | `x4prosim/ghosting/sim-screenshots/refresh-animation/page-turn-final-clean.png` | Same path |
| A | `x4prosim/ghosting/sim-screenshots/rejected/bloom-on-undriven-pixels-halos.png` | Same path |
| A | `x4prosim/ghosting/sim-screenshots/rejected/remnant-250-background-gray-inverse-ghosts.png` | Same path |
| A | `x4prosim/ghosting/test/keyboard-test-exit-steps.txt` | Same path |
| A | `x4prosim/ghosting/test/photo-series-replay-steps.txt` | Same path |
| A | `x4prosim/ghosting/test/type-delete-pep-steps.txt` | Same path |
| A | `x4prosim/ghosting/tools/hist.py` | Same path |
| A | `x4prosim/ghosting/tools/pepmeasure.py` | Same path |
| A | `x4prosim/ghosting/tools/photomeasure.py` | Same path |
| A | `x4prosim/ghosting/tools/seqmeasure.py` | Same path |
| A | `x4prosim/mkflash.sh` | Same path |
| A | `x4prosim/mknvs.py` | Same path |
| A | `x4prosim/mksd.py` | Same path |
| A | `x4prosim/run.sh` | Same path |
| A | `x4prosim/sdcal/README.md` | Same path |
| A | `x4prosim/sdcal/compare.py` | Same path |
| A | `x4prosim/sdcal/cpu-evidence/baseline.log` | Same path |
| A | `x4prosim/sdcal/cpu-evidence/baseline2.log` | Same path |
| A | `x4prosim/sdcal/cpu-evidence/clock-register-test.log` | Same path |
| A | `x4prosim/sdcal/cpu-evidence/clock-register-test.trace` | Same path |
| A | `x4prosim/sdcal/cpu-evidence/clock-scale.S` | Same path |
| A | `x4prosim/sdcal/cpu-evidence/instruction-costs.S` | Same path |
| A | `x4prosim/sdcal/cpu-evidence/instruction-results.json` | Same path |
| A | `x4prosim/sdcal/cpu-evidence/mechanism.log` | Same path |
| A | `x4prosim/sdcal/cpu-evidence/mechanism2.log` | Same path |
| A | `x4prosim/sdcal/cpu-evidence/verified-default.log` | Same path |
| A | `x4prosim/sdcal/cpu-evidence/verified-default.registers` | Same path |
| A | `x4prosim/sdcal/cpu-evidence/verified-default.trace` | Same path |
| A | `x4prosim/sdcal/cpu-evidence/verified-fetch2.log` | Same path |
| A | `x4prosim/sdcal/cpu-evidence/verified-fetch2.registers` | Same path |
| A | `x4prosim/sdcal/cpu-evidence/verified-fetch2.trace` | Same path |
| A | `x4prosim/sdcal/cpu-evidence/verify.py` | Same path |
| A | `x4prosim/sdcal/cpu.md` | Same path |
| A | `x4prosim/sdcal/device-table.txt` | Same path |
| A | `x4prosim/sdcal/panel.md` | Same path |
| A | `x4prosim/sdcal/parse.py` | Same path |
| A | `x4prosim/sdcal/run_qemu.py` | Same path |
| A | `x4prosim/testdata/README.md` | Same path |
| A | `x4prosim/testdata/device/boot-reader-1002p.txt` | Same path |
| A | `x4prosim/testdata/device/panel-otp.txt` | Same path |
| A | `x4prosim/testdata/sdcard/debug/logs/battery.csv` | Same path |
| A | `x4prosim/testdata/sdcard/debug/logs/battery.sum` | Same path |

Inventory acceptance commands:

```sh
git diff --name-status v9.2.2..x4prosim-pre-rebase
git diff --name-status v11.1.2..49f40e8a82
git diff x4prosim-pre-rebase..49f40e8a82 -- x4prosim ":!x4prosim/porting" test_qemu.sh
```

Results: 405 old paths, 420 new paths, and no runner/data diff.

## Report acceptance and scope

The fresh Ninja command, paired old/new reset reproduction, per-hunk accounting
and all 405 inventory entries above are the acceptance evidence. The only
review-owned tracked change is `eink-qemu/porting/review.md`; the inherited
`v11.1.2..49f40e8a82` implementation remains untouched. Review-commit scope is
therefore measured from `49f40e8a82`, rather than attributing the preceding
420-file port to this report commit.

```sh
git diff --name-only 49f40e8a82..HEAD
git diff --stat 49f40e8a82..HEAD
git status --short
```

The required commit subject is `x4prosim: review the QEMU 11.1 port`.
Outstanding implementation work is the P1 selective-reset fix and its paired
regression checks; inherited concerns are listed separately above. Review
completion does not imply those fixes have been made.
