# ESP machines on QEMU 11.1.2

The integrated machine layer builds both targets without warnings, boots the
unchanged CrossPoint 1.6.5 X3 image to Home on a fresh SD card, and reproduces
the reference build's X4 Pro ROM-only output. X3 also returns to Home after
`system_reset`. No firmware, original flash image, or reference checkout was
modified.

Source: `x4prosim-pre-rebase` (`f9339150cd`). Integration start:
`e580cb7c1f`, with every device and target layer already merged. The machine
commit contains only the assigned machine/configuration/ROM paths and this
report. Two separately committed integration fixes are described below.
A diff from `v11.1.2` also includes all previously merged layers; use
`git diff e580cb7c1f..HEAD` for this task's complete changes.

## Preserved behavior and API adaptations

- Imported all eleven ESP32/S3/C3/C6 machine, clock, and interrupt-matrix C
  files and all ten matching headers. MMIO addresses, aliases, overlaps,
  IRQ source numbers, GPIO/SPI/I2C connections, timer routing, flash models,
  device properties and board options retain the reference implementation.
- Restored `esp32`, `esp32s3`, `esp32c3`, `esp32c6`, `x4pro`, and `x3` machine
  registration. C3/C6 now advertise `riscv32_machine_interfaces`; without
  that 11.1 interface their registered classes are hidden from machine help.
- Preserved direct allocation of the concrete Espressif RISC-V CPU, rather
  than instantiating it through a generic hart array. The stack hart-array
  wrapper used only for boot helpers now initializes `RISCVBootInfo` for the
  new `riscv_load_kernel` API.
- Moved infrastructure includes to `hw/core/`, `system/`, and `qobject/`;
  removed obsolete umbrella includes and duplicate includes. Used explicit
  physical-memory, watchpoint, serial, and block-backend declarations.
- Changed QOM class initializer data to `const void *`, made property arrays
  const and counted, and removed sentinel-only property arrays. Kept existing
  Resettable hold callbacks on clock/interrupt devices.
- Registered Xtensa SoC reset through `qemu_register_resettable` with
  `device_class_set_legacy_reset`. The initial port preserved the handler body
  but incorrectly allowed generic traversal to reset every child bus before
  the selective handler. The review fix disables that traversal, leaving
  peripheral/RTC domain selection to the existing handler. Paired CPU-only,
  digital and full-system reset checks now match the old implementation;
  see [fixes.md](fixes.md). Removed `CPUState.num_ases`: 11.1 allocates
  the CPU address spaces from the class maximum (zero means one space).
- Preserved aligned allocation of the S3 TIE extension before CPU execution.
- Replaced `cpu_get_phys_page_debug` with `cpu_translate_for_debug` for ELF
  loading and `cpu_physical_memory_*` with `physical_memory_*`. Passed the
  new error argument to raw ROM loaders while retaining board error messages.
- Moved the removed `MachineClass.fixup_ram_size` operation into the start of
  each Xtensa machine initializer. The old PSRAM rounding/capping and X4 Pro's
  8 MiB default remain intact; QMP checks verify that `-m 3M` becomes 4 MiB.
- Used `riscv_csr_write_i64` for interrupt enable reset writes, preserving
  the registered Espressif CSR behavior.
- Replaced the removed `tb_flush` with `queue_tb_flush` plus the existing
  `CPU_INTERRUPT_EXITTB`. MMIO ends the icount block and the CPU processes the
  queued flush before continuing at the new clock scale. Queuing also works
  when a running machine resets on the main thread; a synchronous flush
  would assert there. Both cold boot and a running-system reset were tested.
- Kept the X3 instruction costs at ROM/SRAM/flash = 6/8/10 ns and
  load/store/multiply/divide/branch extras = 0/3/0/160/2. Explicit `-global`
  overrides still win. The scripts use `-icount shift=0` for X3 and `shift=2`
  for X4 Pro; no script change was necessary.
- Retained the reference tag's existing `TYPE_SD_CARD_SPI` on the X3 SSI bus.
  Contrary to the preliminary integration warning, this tag already uses
  that type and has no `spi=true` assignment to remove. Native DWC cards on
  Xtensa still use `TYPE_SD_CARD`.
- Preserved the Wi-Fi and OpenCores NIC lookup/creation behavior. Validation
  uses the documented `-nic user,model=esp32_wifi`; no NIC model/property was
  renamed. X4 Pro uses UC8179; X3 retains the tag's UC8253 panel defaults.
- Restored all five ROM binaries byte-for-byte and added them to
  `pc-bios/meson.build`. Their hashes were checked against the tag; they were
  not regenerated or patched. ROM lookup succeeds with `./build` binaries
  without an explicit `-L`.

## Build and Kconfig integration

Preserved all upstream sources and configuration entries while adding the
fork's Meson source lists, C3 clock trace event, ROM install list, and optional
machine-disable comments in the two default configuration files.

The Xtensa select lists now match the old tag exactly, without the duplicate
flash entries or unnecessary S3 `SSI_SD` introduced during parallel merges.
C3 retains every old select and explicitly selects `SSI_M25P80`; C6 retains
its old selects and explicitly selects `SSI` and `SSI_M25P80`. These extra
flash selections make the RISC-V machines' dependencies explicit. Existing
device-layer CAN and I2C dependencies are unchanged.

The guide's configure command was already active in this worktree; no extra
configure flag or host source workaround was needed. Full rebuild:

```sh
ninja -C build -t clean
/usr/bin/time -p ninja -C build
```

```text
[2767/2768] Linking target qemu-system-riscv32-unsigned
[2768/2768] Generating qemu-system-riscv32 with a custom command
real 19.33
user 204.35
sys 90.87
```

Exit 0, **zero compiler/linker warnings** across all 2,768 steps. Log:
`build/machines/build-clean.log`. After the clock-reset adaptation, the full
incremental `ninja -C build` also exited 0 without warnings, recompiling the
clock and relinking RISC-V (`build/machines/build-final.log`).

## Required integration fixes outside machine paths

### SPI SD initialization — `a6419eeff2`

`hw/sd/sd.c`: enter `sd_transfer_state` after SPI CMD1/ACMD41 instead of
`sd_ready_state`. SPI has no subsequent CMD2/CMD3/CMD7 selection phase.
Upstream moved SPI framing/state handling from `ssi-sd.c` into `sd.c`; the
fork's working host behavior must follow that boundary.

Before the fix the unchanged firmware sent CMD0, CMD8, CMD55, ACMD41, CMD58,
then CMD18. The last command was rejected as being in the `ready` state and
firmware reported `SD card not detected (err=0x19 data=0x00 ...)`. After the
fix the same sequence reads the MBR, mounts FAT32, and reaches Home. No
synthetic CMD9 is injected by the board or controller.

The storage layer's local qtest driver was copied into this worktree's build
directory and run unchanged, then rerun with its preparatory CMD9 removed:

```sh
python3 build/check-sd-port.py
python3 build/check-sd-no-cmd9.py
```

Both passed:

```text
PASS CMD8 R7 idle, CMD58 R3 ready, no CMD9 required, CMD13 R2
PASS immediate CMD24 data token; random/repeated/sequential busy deadlines
PASS random/repeated/sequential CMD17 read deadlines and payload
PASS CMD25 block busy and STOP_TRAN random-extra/decrement (1000 us)
PASS CMD18 first and next block read deadlines
```

Logs: `build/machines/sd-regression.log` and `sd-no-cmd9.log`.
This is the only device-layer source change required to boot.

### Denied CPU IOMMU access — `b2c940f221`

`system/physmem.c`: preserve the original CPU protection flags when an IOMMU
translation has no permitted accesses and selects the unassigned region.
This is a seven-line core fix, explicitly authorized by coordinator message
`msg_d959c1477968` after the machine worker supplied the crash evidence.

The cache model's `IOMMU_NONE` result is valid under 11.1's translation
contract. `address_space_translate_for_iotlb` already handles this result by
selecting the unassigned region. The failure was in how that result reached
TCG: clearing all protection bits caused `tlb_set_compare` to clear the
MMIO slow flags. The initial load then used a null host pointer. Upstream
commit `24b5e0fdb5` moved `TLB_MMIO` from inline flags to `slow_flags`, exposing
this case; in 9.2 the disabled all-ones inline entry still selected MMIO.

Preserving the CPU permissions for the unassigned region makes the access
raise the normal guest transaction fault. It does not grant access to the
IOMMU's denied backing memory. No cache-model workaround or firmware change
is needed. The X4 Pro ROM-only test below verifies the exact old guest fault
and that the host remains alive. LLDB evidence before the fix:
`build/machines/x4pro-lldb.log`, `build/machines/x4pro-tlb.log` (guest address
`0x3c800000`, `haddr=NULL`, `flags=0`, crash in `load_atomic4`).

## Acceptance: available boards

```text
$ ./build/qemu-system-riscv32 -machine help
Supported machines are:
amd-microblaze-v-generic AMD Microblaze-V generic platform
esp32c3              Espressif ESP32-C3 machine
esp32c6              Espressif ESP32-C6 machine
none                 empty machine
opentitan            RISC-V Board compatible with OpenTitan
sifive_e             RISC-V Board compatible with SiFive E SDK
sifive_u             RISC-V Board compatible with SiFive U SDK
spike                RISC-V Spike board
virt                 RISC-V VirtIO board
x3                   Xteink X3 (ESP32-C3, UC8279d e-paper)

$ ./build/qemu-system-xtensa -machine help
Supported machines are:
esp32                Espressif ESP32 machine
esp32s3              Espressif ESP32S3 machine
kc705                kc705 EVB (dc232b)
kc705-nommu          kc705 noMMU EVB (de212)
lx200                lx200 EVB (dc232b)
lx200-nommu          lx200 noMMU EVB (de212)
lx60                 lx60 EVB (dc232b)
lx60-nommu           lx60 noMMU EVB (de212)
ml605                ml605 EVB (dc232b)
ml605-nommu          ml605 noMMU EVB (de212)
none                 empty machine
sim                  sim machine (dc232b) (default)
virt                 virt machine (dc232b)
x4pro                Xteink X4 Pro (ESP32-S3R8, SSD1677 e-paper)
```

## Acceptance: CrossPoint 1.6.5 on X3

The supplied 16 MiB image contains `CrossPoint version: 1.6.5`. Original and
copied input both retain SHA-256
`80b8d1de62ff9ade1bf5770177e23283677ce13427b724a3e7091409fab8b12a`.
Only `drive.py`'s disposable `.run` copy receives guest writes.

This recipe pins the recorded 1 GiB SDSC image and the old helper's 4 KiB
clusters. The current default produces a 2050 MiB SDHC image, which changes
the acceptance setup. See [SD image geometry](../../README.md#sd-image-geometry)
for new timing comparisons.

```sh
mkdir -p build/machines
cp /Users/serialx/workspace/x4prosim/x3-flash.bin build/machines/x3-flash.bin
python3 x4prosim/mksd.py build/machines/sd.img 1024 --cluster-size 4096
QEMU_EXTRA='-nic user,model=esp32_wifi' \
  python3 x4prosim/drive.py build/machines/x3-flash.bin \
  build/machines/sd.img build/machines/x3.log \
  wait:15 shot:build/machines/home.png \
  hmp:system_reset wait:10 shot:build/machines/home-after-reset.png
```

Exit 0. The run uses `./build/qemu-system-riscv32`, headless display, X3
icount defaults, USB-CDC logging, and a newly created 1 GiB MBR/FAT32 SD image.
The monitor's local Unix socket needs execution outside the Codex sandbox;
no source or host configuration change was required.

Firmware log from cold boot through the Home screenshot:

```text
ESP-ROM:esp32c3-api1-20210207
Build:Feb  7 2021
rst:0x1 (POWERON),boot:0x8 (SPI_FAST_FLASH_BOOT)
SPIWP:0xee
mode:DIO, clock div:1
load:0x3fcd5820,len:0x1010
load:0x403cbf10,len:0x9f4
load:0x403ce710,len:0x2ea4
entry 0x403cbf10
[530] [INF] [HW] Using cached device type: X3
[530] [XTDET] NVS hw_calib/screenType: not set
[642] [XTDET] X3 stock probe VER=FF FF FF BUSY-timeout=0 -> UC8253
[   642][E][Wire.cpp:424] beginTransmission(): could not acquire lock
[   642][E][Wire.cpp:556] write(): NULL TX buffer pointer
[   643][E][Wire.cpp:453] endTransmission(): NULL TX buffer pointer
[   644][E][Wire.cpp:424] beginTransmission(): could not acquire lock
[   644][E][Wire.cpp:556] write(): NULL TX buffer pointer
[   644][E][Wire.cpp:453] endTransmission(): NULL TX buffer pointer
[658] [INF] [GYR] SDK IMU initialized
[658] [INF] [CLK] SDK RTC found[658] [INF] [MAIN] Hardware detect: X3
[670] [SD] SD card detected
[1067]   Wait complete:  X3_PON (128 ms)
[2002]   Wait complete:  X3_DRF (935 ms)
[2677]   Wait complete:  X3_DRF (384 ms)
[3164]   Wait complete:  X3_DRF (382 ms)
[3651]   Wait complete:  X3_DRF (383 ms)
[10021] [INF] [MEM] Free: 155504 bytes, Total: 266300 bytes, Min Free: 155376 bytes, MaxAlloc: 114676 bytes
```

This image's INFO-level log does not print a literal Home transition; the
screenshots establish that result. The pre-existing Wire lock/buffer error
lines also occur with the reference executable and do not prevent boot.

Screenshot: [Home](../../build/machines/home.png).
Reset verification: [Home after system_reset](../../build/machines/home-after-reset.png).
Both were visually inspected. They show the Home menu with Browse Files,
Library, File Transfer, Settings and 80% battery. They also match the
reference executable's Home screenshot byte-for-byte (PNG SHA-256
`490aa22ae0643baa89afea79abd903a8c10b741612d5e485e92b0b5c040bb684`).
The old binary was run using its read-only `drive.py`, the copied image, and
a separate fresh `build/machines/sd-old.img`. Its older panel/timing behavior
differs from the tag's current UC8253/cost defaults; this port follows the tag.

## Acceptance: X4 Pro ROM without flash

Run this command for each binary below, capture combined output, wait five
seconds, then send SIGTERM:

```sh
./build/qemu-system-xtensa \
  -machine x4pro -icount shift=2,sleep=on \
  -display none -serial null -monitor none \
  -chardev stdio,id=cdc,mux=off \
  -global driver=misc.esp32s3.usb_serial_jtag,property=chardev,value=cdc \
  -nic none
```

Reference binary: `/Users/serialx/workspace/x4prosim/build/qemu-system-xtensa`.
No flash drive, SD image or firmware override is passed to either invocation.
The USB-CDC output is identical through the guest backtrace:

```text
Not initializing SPI Flash
ESP-ROM:esp32s3-20210327
Build:Mar 27 2021
rst:0x1 (POWERON),boot:0x4 (SPI_FLASH_BOOT)
Guru Meditation Error: Core 0 panic'ed (LoadStoreError)
Core 0 register dump:
PC      : 0x40056f75  PS      : 0x00060330  A0      : 0x80045804  A1      : 0x3fceb560
A2      : 0x3fceb6ac  A3      : 0x3c800000  A4      : 0x00000008  A5      : 0x3fceb6ac
A6      : 0xffff0000  A7      : 0x00000000  A8      : 0x00000000  A9      : 0x3fceb530
A10     : 0x00000000  A11     : 0x3e000000  A12     : 0x01800000  A13     : 0x01ffffff
A14     : 0x00000000  A15     : 0x00000000  SAR     : 0x00000000  EXCCAUSE: 0x00000003
EXCVADDR: 0x3c800000  LBEG    : 0x40056f5c  LEND    : 0x40056f72  LCOUNT  : 0xffffffff

Backtrace: 0x40056f75:0x3fceb560 0x40045801:0x3fceb570 0x40043ab6:0x3fceb6f0 0x40034c45:0x3fceb710

```

Both processes remain alive for the timeout and exit 0 on SIGTERM. The guest
LoadStoreError is the reference's expected no-flash behavior, not a host
crash. `python3 build/machines/rom-compare.py` asserts byte-identical output
before the termination diagnostic. Logs: `build/machines/x4pro-rom-new.log`,
`x4pro-rom-old.log`, and `rom-compare.log`.

## Additional verification and limits

`python3 build/machines/check-properties.py` passes QMP checks for the eight
X3 CPU cost defaults, a user `-global` cost override, X4 Pro's 8 MiB PSRAM
default, the two generic Xtensa zero-PSRAM defaults, and `-m 3M` rounding on
all three Xtensa machines. All six machines start and accept `system_reset`
while paused; the firmware run additionally verifies a running X3 reset.
Results: `build/machines/properties.log`.

`git diff --check` passes. Five ROM hashes match the tag, and the original
firmware hash still matches the copied input (`build/machines/hashes.txt`).
All logs, temporary qtest drivers, SD/flash copies and screenshots remain in
ignored `build/`; the commands and acceptance output are recorded here.

The full default build includes tools and test executables. The full QEMU
test suite was not run. This task does not claim ESP32/C6 application boots,
X4 Pro application-firmware validation, physical-device comparisons, Wi-Fi
connectivity, or validation on other hosts.
