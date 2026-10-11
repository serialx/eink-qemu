# ESP32 and ESP32-S3 Xtensa targets on QEMU 11.1.2

The ESP32 and ESP32-S3 CPU configurations and S3 TIE translator are ported from
`x4prosim-pre-rebase`. Full `ninja -C build` passes on the macOS host in
[host.md](host.md), without warnings from `target/xtensa` or the final link.
Both CPU models are listed, and an empty `sim` machine using `esp32s3` runs
for three seconds without an assertion. No machine or device sources were
changed.

## Registration and generated overlays

Compared `cpu.h`, `helper.c`, `translate.c`, `cores.list`, the upstream
`core-de233_fpu.c` and `core-dc233c.c`, and `import_core.sh` between v9.2.2
and v11.1.2 before applying the fork changes.

- `cores.list` still feeds `target/xtensa/meson.build` directly. Added
  `core-esp32.c`, `core-esp32s3.c`, and `translate_tie_esp32s3.c` there,
  matching the old fork's source registration. No Meson edit was needed.
- Both core files retain mutable `XtensaConfig` objects, `DEFAULT_SECTIONS`,
  their 40,000 kHz clocks, and `REGISTER_CORE`. The upstream registration
  path now finalizes configuration in `xtensa_register_core` and uses
  `type_register_static`; those changes remain intact. The class callback
  continues using the upstream `const void *data` signature.
- Replaced the removed `exec/gdbstub.h` include with `gdbstub/helpers.h`,
  as used by current upstream cores and the import script.
- Kept the original generated `gdb-config.inc.c` and `xtensa-modules.inc.c`
  names. Upstream's importer emits `.c.inc`, but these files are textual
  includes and their suffix does not affect the build. The overlays and
  ISA tables retain their data and original copyright/license headers;
  only trailing whitespace was cleaned up.
- ESP32 uses the core and FPU opcode translators; ESP32-S3 additionally
  registers `xtensa_tie_opcodes`. No generated opcode or register entry
  was removed.

## Translator and CPU state

| Item | Adaptation |
| --- | --- |
| `cpu.h:CPUArchState.ext` | Restored the extension pointer without changing the upstream CPU layout or APIs elsewhere. |
| `cpu_esp32s3.h` | Restored Q registers, accumulators, UA state, SAR byte, FFT/GPIO state, and temporary storage with the original layout and 16-byte Q-register alignment. Normalized the header guard and comments. |
| `translate.h:DisasContext` | Moved the current upstream context definition into the shared header. Dropped the old fork header's unused `sar_m32_allocated` member, which neither the upstream translator nor S3 code uses. |
| Shared translation utilities | Exported `cpu_SR`, `gen_exception_cause`, `gen_load_store_alignment`, and the four `get_f32_*`/`put_f32_*` wrappers used by TIE. Their declarations live in `translate.h`; the duplicate `cpu_SR` declaration in the TIE source was removed. |
| Floating-point wrappers | Defined the four exported wrappers as ordinary external functions rather than the old fork's external `inline` definitions, so cross-file calls have definitions independent of optimization and C inline semantics. |
| Includes | Removed the obsolete `exec/exec-all.h` and unused CPU-load/store, semihosting, and internal-temporary includes from the imported TIE/header code. The shared header includes only CPU definitions, the generic translator API, and public TCG operations. |
| Temporary lifetime | Removed 248 explicit `tcg_temp_free_i32/i64` calls from the imported TIE translator. Its temporaries are created with `tcg_temp_new_*` or `tcg_constant_*`; upstream `tcg_temp_free_internal` ignores frees for `TEMP_TB` and `TEMP_CONST`. Their lifetime is managed by TCG, so removing these no-ops avoids importing the private EBB-temporary interface and preserves behavior. |
| TIE helpers | Restored all helper declarations and implementations, including eight-argument FFT helpers. These use the corrected helper generation supplied by the [core layer](core.md). |
| DFP accelerator stubs | Retained the six no-op `wur`/`rur` handlers for `f64r_lo`, `f64r_hi`, and `f64s`; upstream still does not provide these fork-specific handlers. No floating-point accelerator behavior was added. |

The upstream `xtensa_translate_code` entry point, three-word
`tcg_gen_insn_start(pc, 0, 0)`, `translator_loop(..., TCG_TYPE_VA)`, address
handling, and semihosting changes are preserved. The Xtensa translator does
not install an `insn_cost` callback, so the core layer continues charging one
tick per instruction.

## GDB behavior retained

- `helper.c:xtensa_core_class_init` honors `QEMU_XTENSA_CORE_REGS_ONLY`:
  when present and different from `"0"`, advertise only `num_core_regs`;
  otherwise use `num_regs`.
- `gdbstub.c:xtensa_count_regs` excludes window registers unless
  `QEMU_XTENSA_COUNT_WINDOW_REGS` is present and different from `"0"`.
  The existing exclusions for TIE state, mapped, and unmapped registers
  remain unchanged.

These preserve the old fork's environment-variable semantics, including an
empty value being different from `"0"`. GDB client interoperability was not
part of this build-level acceptance.

## Machine-layer dependency and validation limits

`env.ext` allocation remains with the ESP32-S3 machine, matching the old
fork's `hw/xtensa/esp32s3.c` allocation of
`qemu_memalign(16, sizeof(CPUXtensaEsp32s3State))`. That machine source has
not been ported in this task. The empty `sim` smoke test does not exercise
S3 TIE helpers that dereference this extension; their runtime/firmware
validation must follow machine integration. No target-side allocation or
reset behavior was invented to make the empty-guest test pass.

No functional fork item was dropped. The removed items are unused includes,
the unused context member, ignored temporary frees, and redundant declarations;
all changes are described above. Original fork authors for this layer are
Ivan Grokhotkov, Dmitry Yakovlev, and harshal.patil.

## Acceptance output

The unchanged configure command from [README.md](README.md) was used.
The final full build exited 0. Build logs are retained under ignored `build/`
as `xtensa-build-final.log`; earlier compile attempts are also retained there.
The CPU list and startup outputs are copied below from
`xtensa-cpu-help.log` and `xtensa-smoke.log`.

```text
$ ninja -C build
ninja: Entering directory `build'
[1/21] Generating subprojects/dtc/version_gen.h with a custom command
[2/21] Generating qemu-version.h with a custom command (wrapped by meson to capture output)
[3/8] Compiling C object libqemu-xtensa-softmmu.a.p/target_xtensa_core-esp32.c.o
[4/8] Compiling C object libqemu-xtensa-softmmu.a.p/target_xtensa_translate.c.o
[5/8] Compiling C object libqemu-xtensa-softmmu.a.p/target_xtensa_translate_tie_esp32s3.c.o
[6/8] Compiling C object libqemu-xtensa-softmmu.a.p/target_xtensa_core-esp32s3.c.o
[7/8] Linking target qemu-system-xtensa-unsigned
[8/8] Generating qemu-system-xtensa with a custom command

$ ./build/qemu-system-xtensa -cpu help
Available CPUs:
  dc232b
  dc233c
  de212
  de233_fpu
  dsp3400
  esp32
  esp32s3
  lx106
  sample_controller
  test_mmuhifi_c3

$ ./build/qemu-system-xtensa -M sim -cpu esp32s3 -nographic -display none
QEMU 11.1.2 monitor - type 'help' for more information
(qemu) qemu-system-xtensa: terminating on signal 15 from pid 51956 (<unknown process>)
[3-second timeout: SIGTERM sent; QEMU exited 0]
```
