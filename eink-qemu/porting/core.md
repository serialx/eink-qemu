# Core layer on QEMU 11.1.2

The core port builds both `qemu-system-xtensa` and `qemu-system-riscv32` on the
macOS host described in [host.md](host.md). Full `ninja` passed after each of
items 1–6 below, in order, and after final verification, without compiler or
linker warnings. No fork target, device, or machine source was added.

## 1. Instruction tick costs

The port preserves the behavior from `2aaea17e71` in `x4prosim-pre-rebase`.
`tb->icount`, `db->num_insns`, `max_insns`, and `CF_COUNT_MASK` always count
instructions. Only `icount_cost` is charged to the virtual-time decrementer.
An absent `TranslatorOps.insn_cost` callback costs one tick per instruction,
so existing targets retain instruction-based icount accounting.

| Hook | QEMU 11.1.2 location | Behavior |
| --- | --- | --- |
| Target callback and accumulated cost | `include/exec/translator.h:TranslatorOps`, `DisasContextBase` | Optional `insn_cost(db, cpu)` runs after `translate_insn`; `icount_cost` accumulates its positive result. |
| TB cost | `include/exec/translation-block.h:TranslationBlock` | Separate 16-bit `icount_cost`; `icount` remains an instruction count. |
| Per-instruction costs | `include/tcg/tcg.h:TCGContext` | `gen_insn_cost[TCG_MAX_INSNS]` parallels `gen_insn_end_off`. |
| Cost collection | `accel/tcg/translator.c:translator_loop` | Initialize the sum, default each cost to one, record costs, and assert that total cost fits `UINT16_MAX`. |
| Debit and atomic borrow | `accel/tcg/translator.c:gen_tb_start`, `gen_tb_end` | Patch the generated subtract with tick cost; for `CF_USE_ICOUNT | CF_NOIRQ`, mask asynchronous exit bits and record a negative borrow of 65,536 in `icount_extra` when the single instruction exceeds its budget. |
| Unwind encoding | `accel/tcg/translate-all.c:encode_search` | Append each positive cost as a direct signed LEB128 value after the host-PC delta. |
| Budget conversion | `accel/tcg/translate-all.c:tb_insns_for_ticks` | Decode whole instruction costs until the next instruction exceeds the remaining ticks. |
| Internal declaration | `accel/tcg/tb-internal.h:tb_insns_for_ticks` | Replaces the old declaration in the removed `exec-all.h`; used internally by `cpu-exec.c`. |
| Unwind decoding | `accel/tcg/translate-all.c:cpu_unwind_data_from_tb` | Consume the same extra column and optionally return the unexecuted tick cost, including the faulting instruction. |
| Exception/MMIO refund | `accel/tcg/translate-all.c:cpu_restore_state_from_tb` | Refund unexecuted ticks and undo the atomic borrow when the low-word refund wraps. |
| Data-only unwind | `accel/tcg/translate-all.c:cpu_unwind_state_data` | Pass a null cost-result pointer while still consuming the cost column. |
| Deadline handling | `accel/tcg/cpu-exec.c:icount_exit_request`, `cpu_loop_exec_tb` | Exit at a signed remaining budget of zero or less; convert ticks to instructions before setting `CF_COUNT_MASK`; if no whole instruction fits, execute one with `CF_NOIRQ`, `CF_NO_GOTO_TB`, and `CF_NO_GOTO_PTR`. |

Two upstream adaptations matter for subsequent layers:

- The unwind table uses the common `INSN_START_WORDS` (three 64-bit columns),
  replacing `TARGET_INSN_START_WORDS`. Target metadata, including RISC-V fault
  information and `CF_PCREL` handling, is unchanged. The cost is an additional
  column, not a target metadata field. Its maximum encoded size is three bytes,
  comfortably within the existing 1,024-byte TCG high-water reserve per row.
- Generated accesses use `offsetof(CPUState, field) - sizeof(CPUState)`, matching
  the new target-independent translator. The old `ArchCPU.parent_obj` offsets
  must not be restored.

The RISC-V target layer must install the callback and bound TB length so the
sum cannot exceed 65,535 ticks. This core layer intentionally leaves every
upstream target callback null. The weighted paths were checked in a focused
harness; guest execution with a non-null target callback awaits that layer.

## 2. Eight-argument helpers

Upstream still stopped at seven arguments. Added `DEF_HELPER_8` and
`DEF_HELPER_FLAGS_8` to the four `include/exec/helper-*.inc` expansion files,
plus `include/tcg/tcg.h:tcg_gen_call8` and its implementation in `tcg/tcg.c`.

The port corrects two gaps in the old fork's addition:

- `include/tcg/helper-info.h:MAX_CALL_IARGS` is now eight. The old fork left it
  at seven despite its ESP32-S3 eight-argument helper declarations. This sizes
  both argument-location and temporary-extension arrays correctly, and also
  updates the interpreter's argument limit through the shared constant.
- The generated wrapper passes `helper_info_NAME.func` as the function pointer,
  matching the existing wrappers. The old added wrapper passed the address of
  the metadata structure as both arguments.

`tcg_gen_callN` now accepts `n_extend == ARRAY_SIZE(extend_free)` in its debug
assertion, so extending every argument is valid. Each type occupies three bits;
return plus eight arguments fits in the 32-bit typemask. QEMU 11.1 uses 64-bit
TCG host registers, so eight 128-bit inputs occupy sixteen physical input slots;
the enlarged array accommodates them. With a 128-bit return, eighteen operand
liveness bits plus four reserved bits still fit `TCGLifeData`. Calls allocate
argument storage dynamically and bypass the fixed non-call constraint arrays,
so `TCG_MAX_OP_ARGS` does not need increasing.

The coordinator approved the companion `include/tcg/helper-info.h` change.

## 3. `dup_const` revert dropped

Dropped Espressif commit `f08bf1a417` (revert of `666cc794abe7`). The upstream
`MO_64` inline branch and invalid-constant diagnostic remain unchanged. The
original revert supplies no failure explanation, and the macro itself has not
been replaced upstream; there is no basis to claim a particular upstream fix.
It is unnecessary on this host: Apple clang 21 builds the full tree, and a
focused `-Werror` probe compiles all four constant widths and a dynamic-width
call at both `-O0` and `-O2`. Other host compilers were not tested.

## 4. Internal SHA, HMAC, and callback-based XTS

Restored the old fork's compression APIs and headers for SHA-1, SHA-224,
SHA-256, SHA-384, SHA-512, SHA-512/224, SHA-512/256, SHA-512/t, and HMAC-256,
and added their sources to `crypto/meson.build`'s `util_ss`. These APIs expose
compression state for the ESP SHA/HMAC/DS models; upstream's high-level crypto
provider APIs do not replace them. Original license and copyright headers are
preserved, with `qemu/osdep.h` first in C files and minor formatting cleanup.
Padding, state-word order, and the existing HMAC output convention are retained.

Upstream removed `crypto/xts.c` and `include/crypto/xts.h` in `167194d087`
when its supported nettle version made the in-tree implementation unnecessary.
The ESP32-C3/S3 XTS-AES models still need the callback-based API, so both files
are restored unchanged from v9.2.2 (also unchanged in the old fork). XTS is
built unconditionally in `util_ss`, as the fork required. The coordinator
approved restoring the companion header outside the initial explicit file list.

## 5. Host and dependency changes

| Old fork item | Port decision |
| --- | --- |
| `meson.build`: force static slirp everywhere | Changed, with coordinator approval, to `static: get_option('prefer_static')`. Normal macOS/Linux builds keep shared slirp; Windows CI must retain `prefer_static=true` for its static build. Forcing static slirp on this host also pulled static GLib/gettext into an otherwise dynamic build and produced duplicate `-liconv` linker warnings. The final build has no such warnings. |
| `meson.build`: `LIBSLIRP_STATIC` | Retained only when slirp is found, the host is Windows, and `prefer_static` is enabled. The declaration now agrees with the selected linkage. Existing slirp version/CFI checks remain intact. |
| `python/scripts/mkvenv.py`: pip 25.2 version-matcher fallback | Dropped as already upstream in `6ad034e712`, with later upstream refinements retained. |
| `python/scripts/mkvenv.py`: pass the wheel directory directly instead of a `file://` URL | Dropped as already upstream in `587f4a1805`. The baseline already works with Python 3.14 and bundled Meson 1.11.1. |
| `util/oslib-win32.c` / `block/file-win32.c`: relocate `qemu_ftruncate64` | Dropped as already upstream in `0c9f429ec8`; no Windows source changes are needed. |

No edits were made to the Python or Windows files. `otool -L` confirms the
final macOS RISC-V binary uses Homebrew's `libslirp.0.dylib`. Windows compilation
and the static Windows link were not tested on this macOS host.

## 6. Machine Kconfig symbols

Added default-enabled symbols without adding machine sources:

| Symbol | Dependency | Existing upstream selects retained from the old tree |
| --- | --- | --- |
| `XTENSA_ESP32` | `XTENSA` | `SSI`, `SSI_M25P80`, `UNIMP`, `OPENCORES_ETH`, `TMP105`, `LED` |
| `XTENSA_ESP32S3` | `XTENSA` | `SSI`, `SSI_M25P80`, `UNIMP`, `OPENCORES_ETH`, `TMP105` |
| `RISCV_ESP32C3` | `RISCV32` | `OPENCORES_ETH`, `UNIMP`, `SSI`, `SSI_SD`, `I2C` |
| `RISCV_ESP32C6` | `RISCV32` | `OPENCORES_ETH`, `UNIMP` |

Deferred absent fork-device selects to their owners: `DWC_SDMMC`, `ESP_RGB`,
`BM8563`, `CW2017`, `GT911`, `BQ27220`, `DS3231`, and `QMI8658`. Existing upstream
machine entries, including their PFLASH selections, remain unchanged. All four
new symbols appear as `y` in the generated target device configurations.

## Verification

The configure command remains the one in [README.md](README.md). Full builds
were run after tick costs, eight-argument helpers, the `dup_const` decision,
crypto restoration, host/dependency decisions, and Kconfig additions. Logs are
in ignored `build/core-step1.log` through `core-step6.log`, with the final
`ninja -C build` result in `core-final-build.log`; all final step logs are free
of compiler and linker warnings.

An optional checkpatch scan is not clean: it treats the restored files as new
and rejects their inherited license headers (including LibTomCrypt's
`Unlicense`), and misidentifies the helper-metadata macro initializer as a
function body. The copied license text is intentionally preserved rather than
relicensed. This scan also requests submission metadata (`Signed-off-by` and
MAINTAINERS review); those are separate from the local build checks.

Additional focused checks, with scripts and logs retained under ignored
`build/`, passed:

- `check-core-icount.py`: extracts the real encoder, decoder, restore function,
  exit-budget check, and TB execution-budget logic; checks PC-relative and
  negative metadata deltas, weighted and unit costs, whole-instruction limits,
  one-atomic-instruction flags, signed exhausted budgets, partial refunds,
  borrow reversal, and a 65,535-tick instruction. Built with address/undefined
  sanitizers and QEMU's `-fwrapv` semantics.
- `check-core-helper8.py`: compiles the actual helper expansion headers,
  argument-layout functions, and `tcg_gen_call8`; checks function-pointer and
  argument forwarding plus eight i32 and eight i128 inputs with debug
  assertions and address/undefined sanitizers. The final call emitter is
  stubbed for inspection; this is not an ESP32-S3 guest-execution test.
- `core-dup-const.c`: constant and dynamic `dup_const` compilation at `-O0`
  and `-O2` with Apple clang 21 and `-Werror`.
- `check-core-crypto.py`: all seven SHA variants match Python hashlib for
  empty, `abc`, and multiblock messages; SHA-512/t produces the standard
  SHA-512/224 and SHA-512/256 initial states; HMAC-256 matches short and
  multiblock reference vectors after converting its native-word output.
- `check-core-xts.py`: builds the unmodified v9.2.2 upstream XTS unit test
  against the restored implementation; all 18 vector, split, and unaligned
  cases pass.

The two required guest commands below were each allowed to run for three
seconds, then terminated with SIGTERM by `check-core-smoke.py`. `virt` prints
OpenSBI and both processes remain alive until the timeout, then exit cleanly.
`sifive_e` does not load OpenSBI for `-bios default`: its unchanged
`hw/riscv/sifive_e.c:sifive_e_machine_init` installs the reset vector and loads
an image only when `-kernel` is supplied, so no firmware banner is expected.
The full QEMU test suite and other host/target combinations were not run.

### Acceptance output

```text
$ ./build/qemu-system-riscv32 -M virt -icount shift=2 -nographic -bios default -display none

OpenSBI v1.8.1
   ____                    _____ ____ _____
  / __ \                  / ____|  _ \_   _|
 | |  | |_ __   ___ _ __ | (___ | |_) || |
 | |  | | '_ \ / _ \ '_ \ \___ \|  _ < | |
 | |__| | |_) |  __/ | | |____) | |_) || |_
  \____/| .__/ \___|_| |_|_____/|____/_____|
        | |
        |_|

Platform Name               : riscv-virtio,qemu
Platform Features           : medeleg
Platform HART Count         : 1
Platform HART Protection    : pmp
Platform IPI Device         : aclint-mswi
Platform Timer Device       : aclint-mtimer @ 10000000Hz
Platform Console Device     : uart8250
Platform HSM Device         : ---
Platform PMU Device         : ---
Platform Reboot Device      : syscon-reboot
Platform Shutdown Device    : syscon-poweroff
Platform Suspend Device     : ---
Platform CPPC Device        : ---
Firmware Base               : 0x80000000
Firmware Size               : 317 KB
Firmware RW Offset          : 0x40000
Firmware RW Size            : 61 KB
Firmware Heap Offset        : 0x46000
Firmware Heap Size          : 37 KB (total), 0 KB (reserved), 11 KB (used), 25 KB (free)
Firmware Scratch Size       : 4096 B (total), 1404 B (used), 2692 B (free)
Runtime SBI Version         : 3.0
Standard SBI Extensions     : time,rfnc,ipi,base,hsm,srst,pmu,dbcn,fwft,legacy,dbtr,sse
Experimental SBI Extensions : none

Domain0 Name                : root
Domain0 Boot HART           : 0
Domain0 HARTs               : 0*
Domain0 Region00            : 0x80040000-0x8004ffff M: (F,R,W) S/U: ()
Domain0 Region01            : 0x80000000-0x8003ffff M: (F,R,X) S/U: ()
Domain0 Region02            : 0x00100000-0x00100fff M: (I,R,W) S/U: (R,W)
Domain0 Region03            : 0x10000000-0x10000fff M: (I,R,W) S/U: (R,W)
Domain0 Region04            : 0x02000000-0x0200ffff M: (I,R,W) S/U: ()
Domain0 Region05            : 0x0c400000-0x0c5fffff M: (I,R,W) S/U: (R,W)
Domain0 Region06            : 0x0c000000-0x0c3fffff M: (I,R,W) S/U: (R,W)
Domain0 Region07            : 0x00000000-0xffffffff M: () S/U: (R,W,X)
Domain0 Next Address        : 0x00000000
Domain0 Next Arg1           : 0x87e00000
Domain0 Next Mode           : S-mode
Domain0 SysReset            : yes
Domain0 SysSuspend          : yes

Boot HART ID                : 0
Boot HART Domain            : root
Boot HART Priv Version      : v1.12
Boot HART Base ISA          : rv32imafdch
Boot HART ISA Extensions    : sstc,zicntr,zihpm,zicboz,zicbom,sdtrig,svadu
Boot HART PMP Count         : 16
Boot HART PMP Granularity   : 2 bits
Boot HART PMP Address Bits  : 32
Boot HART MHPM Info         : 16 (0x0007fff8)
Boot HART Debug Triggers    : 2 triggers
Boot HART MIDELEG           : 0x00001666
Boot HART MEDELEG           : 0x00f4b509
qemu-system-riscv32: terminating on signal 15 from pid 99259 (<unknown process>)
[3-second timeout: SIGTERM sent; QEMU exited 0]

$ ./build/qemu-system-riscv32 -M sifive_e -icount shift=2 -nographic -bios default -display none
qemu-system-riscv32: terminating on signal 15 from pid 99259 (<unknown process>)
[3-second timeout: SIGTERM sent; QEMU exited 0]
```
