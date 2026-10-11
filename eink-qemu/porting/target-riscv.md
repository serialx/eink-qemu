# RISC-V target on QEMU 11.1.2

The Espressif CPU and eink-qemu instruction-cost model are ported from
`x4prosim-pre-rebase`. Full `ninja -C build` succeeds without compiler or
linker warnings. CPU listing, direct C3/C6 startup, and the default `virt`
OpenSBI smoke checks pass. ESP firmware boot and peripheral integration await
the machine layer.

## CPU interface and machine integration

The reference tree registers **one** CPU type, `espressif-riscv-cpu`, exposed
by `-cpu help` as **`espressif`**. Both ESP32-C3 and ESP32-C6 use it; there are
no separate `esp32c3` or `esp32c6` CPU names to restore. The machine must set
`has-pma=true` and `mie-as-bitmap=true` for C6. These properties default to
false, preserving C3 behavior. The `hartid-base`, cycle-counter divider,
interrupt GPIO name, interrupt callback, and public `esp_cpu.h` interface
remain available to the machine and interrupt-matrix layers.

The requested `-M virt -cpu espressif` combination aborts before realization:
`RISCVHartArrayState` allocates an array of `sizeof(RISCVCPU)`, which cannot
contain the larger `EspRISCVCPU` subclass. The old fork has the same limitation.
The coordinator approved substituting **`-M none -cpu espressif`**, which calls
`cpu_create()` and allocates the concrete type's full size. ESP machine code
must continue initializing the Espressif CPU directly, as in the reference,
rather than using the generic hart array. No machine source was changed.

## Upstream adaptations

- Added `esp_cpu.c` and `esp_cpu.h`; compile the source once through the system
  source set when either ESP machine symbol and TCG are enabled.
- Declare the RV32IMC ISA through QEMU 11.1's `RISCVCPUDef` class data while
  retaining the `rv32` parent and the old Zawrs/Zfa exclusions. Restore RV32IMC
  after the dynamic parent's accelerator initialization overwrites MISA with
  generic defaults, as the old instance initializer did. Keep upstream profile
  and named-feature registration intact.
- Register the eight `cost-*` properties in the existing `static const
  Property` array, without an end marker, using `device_class_set_props()`.
- Use the moved `hw/core` and `accel/tcg` headers; CSR write callbacks now
  accept the upstream return-address argument.
- Copy `TCGCPUOps` and `SysemuCPUOps` into the Espressif class, preserving
  parent callbacks and overriding external-interrupt dispatch and wakeup.
  Upstream now supplies TCG operations at class initialization, so the old
  instance-time mutation of a shared static operation table is unnecessary.
- Replace the legacy global reset registration with `ResettableClass` hold
  chaining and `qemu_register_resettable()` registration, paired with
  unregistration at unrealize because CPUs have no parent bus. Reset clears the IRQ bitmap, C6 MIE bitmap, and parent IRQ after
  the parent CPU reset. Parent realization errors propagate through `errp`.
- Debug CSR stub reads explicitly return zero; the old empty read branch
  could leave its output uninitialized. Other CSR and interrupt behavior is
  retained, including the reference's process-global CSR registration model.

## Instruction timing

The callback is now in `target/riscv/tcg/translate.c`, installed as
`TranslatorOps.insn_cost`. Classification uses the instruction's starting
virtual address, with the exact old half-open ranges:

| Region | Address range | Default base cost |
| --- | --- | --- |
| Flash | `0x42000000 <= pc < 0x42800000` | 1 |
| ROM | `0x40000000 <= pc < 0x40060000` | 1 |
| SRAM/default | Every other address | 1 |

The old load/store/multiply/divide/branch opcode classification, including
compressed instructions, is unchanged. Each extra defaults to zero. Cost is
`min(65535, (max(1, base) + extra) * max(1, cost_clock_scale))`.
`cost_clock_scale` remains an internal 32-bit field, with zero meaning 1x;
the clock device must flush translated blocks when changing it, as before.
With `-icount shift=0`, ticks are nanoseconds. All-default properties and
scale charge exactly one tick per instruction on every RISC-V model.

Two small corrections keep the model safe under upstream translation rules:

- Multiply in 64 bits before saturation, avoiding wraparound for large clock
  scales. Bound TB instruction count by `65535 / maximum_instruction_cost`.
- Preserve upstream `CF_PCREL` translation sharing only when all effective
  region base costs match. With different region costs, a block translated at
  one virtual alias must not be reused at another alias with a different cost.
  Default properties keep upstream PC-relative sharing enabled.

## Build and verification

Configure used the guide's options plus
`--python=/opt/homebrew/bin/python3` to select the documented host Python;
the initial PATH selected a PlatformIO environment instead. As in `host.md`,
the sandboxed configure could not fetch Python dependencies, so configure was
rerun with network access. No host source or build-system workaround was added.

The complete RISC-V build and then the full default Ninja build passed.
The final incremental full build output was:

```text
$ ninja -C build
ninja: Entering directory `build'
[1/18] Generating subprojects/dtc/version_gen.h with a custom command
[2/18] Generating qemu-version.h with a custom command (wrapped by meson to capture output)
[3/5] Compiling C object libqemu-riscv32-softmmu.a.p/target_riscv_esp_cpu.c.o
[4/5] Linking target qemu-system-riscv32-unsigned
[5/5] Generating qemu-system-riscv32 with a custom command
[exit 0; no compiler or linker warnings]
```

Additional checks in ignored `build/`:

- `check-target-riscv-cost.py` compiles the extracted old and new callbacks
  with address/undefined sanitizers. It compares all 65,536 compressed opcode
  bit patterns plus representative 32-bit opcodes at ten region-boundary
  addresses and six scales. It also checks unit-cost defaults, zero bases,
  `UINT32_MAX` scale saturation, and the TB cost bound.
- `check-target-riscv-smoke.py` runs the acceptance commands below with a
  three-second timeout. It also boots OpenSBI using all eight nondefault cost
  properties (`6,7,10,2,3,4,5,1`) and again with all eight set to 255; both
  remain alive through the timeout and exit zero after SIGTERM.
- `check-target-riscv-reset.py` runs an RV32IMC increment loop in RAM for
  both C3 and C6 configurations, checks the exact MISA value `0x40001104`,
  and verifies that QMP `system_reset` restores PC `0x1000` and clears
  `mstatus.MIE`. General-purpose registers retain upstream reset behavior.
- `git diff --check` passes. The QEMU test executables were built; the full
  test suite, other hosts, and ESP firmware were not run.

```text
PASS: 3932760 old/new cost comparisons, default and zero costs, saturation, TB bound
PASS: espressif: RV32IMC guest increment loop ran with MIE set; system_reset restored PC=0x1000 and cleared MIE
PASS: espressif,has-pma=true,mie-as-bitmap=true: RV32IMC guest increment loop ran with MIE set; system_reset restored PC=0x1000 and cleared MIE

```

## Acceptance output

The failed originally requested combination is retained here for clarity:

```text
$ ./build/qemu-system-riscv32 -M virt -cpu espressif -icount shift=0 -nographic -display none -bios none
ERROR:../qom/object.c:496:object_initialize_with_type: assertion failed: (size >= type->instance_size)
[QEMU aborted with SIGABRT; coordinator approved the direct-CPU substitution]
```

The CPU listing, approved direct C3/C6 CPU checks, and unchanged default-CPU
`virt` boot produced:

```text
$ ./build/qemu-system-riscv32 -cpu help
Available CPUs:
  espressif
  lowrisc-ibex
  max
  rv32
  rv32e
  rv32i
  sifive-e31
  sifive-e34
  sifive-u34
[QEMU exited 0]

$ ./build/qemu-system-riscv32 -M none -cpu espressif -icount shift=0 -nographic -display none -bios none
QEMU 11.1.2 monitor - type 'help' for more information
(qemu) qemu-system-riscv32: terminating on signal 15 from pid 77739 (<unknown process>)
[3-second timeout: SIGTERM sent; QEMU exited 0]

$ ./build/qemu-system-riscv32 -M virt -icount shift=2 -nographic -display none -bios default

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
qemu-system-riscv32: terminating on signal 15 from pid 77739 (<unknown process>)
[3-second timeout: SIGTERM sent; QEMU exited 0]

$ ./build/qemu-system-riscv32 -M none -cpu espressif,has-pma=true,mie-as-bitmap=true -icount shift=0 -nographic -display none -bios none
QEMU 11.1.2 monitor - type 'help' for more information
(qemu) qemu-system-riscv32: terminating on signal 15 from pid 77739 (<unknown process>)
[3-second timeout: SIGTERM sent; QEMU exited 0]
```
