# Port of eink-qemu to QEMU 11.1.2

The port to upstream **v11.1.2** is complete. The fork previously used
Espressif's `esp-develop` (QEMU 9.2.2 plus 126 Espressif commits), followed by
53 eink-qemu commits. The port reapplied that stack in layers and adapted it
to QEMU 11.1 APIs. The original tree is preserved at
`x4prosim-pre-rebase` (`f9339150cd`). These notes record the port's decisions
and review evidence; they are not an active work assignment.

The [port review](review.md) compares the original delta with the completed
port at `49f40e8a82`. Its selective-reset finding was fixed in `d01ff66f48`.

## Reference material

- Original file: `git show x4prosim-pre-rebase:<path>`; original fork delta:
  `git diff v9.2.2 x4prosim-pre-rebase -- <path>`.
- Upstream changes: `git diff v9.2.2 v11.1.2 -- <path>` and
  `git log --oneline v9.2.2..v11.1.2 -- <path>`.
- `docs/devel/` documents the current reset, qdev, memory and translator APIs.
- The paired review used an old QEMU 9.2.2 build from the reference tag.
  Reproducing those comparisons requires a separate checkout and build of
  that tag; no particular local checkout path is required.
- Current project instructions: `EINK-QEMU.md`, `README.md` and the run
  scripts under `eink-qemu/`.

## Native build used for the port

Each checkout uses its own build directory:

```sh
mkdir -p build && cd build
../configure --target-list=xtensa-softmmu,riscv32-softmmu --enable-gcrypt --enable-sdl \
  --enable-slirp --disable-gnutls --disable-strip --disable-user --disable-capstone \
  --disable-vnc --disable-gtk --disable-docs
ninja qemu-system-xtensa qemu-system-riscv32
```

The port was built on macOS with Apple clang 21, Homebrew Python 3.14,
ninja 1.13 and configure's bundled Meson wheel. Host adaptations are recorded
in [host.md](host.md).

## Port layers

| Layer | Files | Notes |
| --- | --- | --- |
| core | `accel/tcg`, `include/exec`, `include/tcg`, `tcg`, `meson.build`, `crypto`, `python`, `util/oslib-win32.c`, `block/file-win32.c`, machine Kconfig symbols | icount tick-cost hook, `DEF_HELPER_8`, dup_const revert, static slirp |
| target-xtensa | `target/xtensa` | ESP32 and ESP32-S3 cores, TIE translate, gdb env knobs |
| target-riscv | `target/riscv` | `esp_cpu.c`, per-instruction cost properties and `insn_cost` |
| dev-misc | `hw/misc` (except the wifi files), `include/hw/misc` | base classes `esp_*.c` and chip variants |
| dev-io | `hw/char`, `hw/gpio`, `hw/i2c`, `hw/rtc`, `hw/nvram`, `hw/dma`, `hw/timer` and their `include/hw/...` | |
| dev-storage-display | `hw/ssi`, `hw/sd`, `hw/display`, `hw/input`, `hw/block/m25p80.c` and their `include/hw/...` | panel models, SD timing model |
| dev-net | `hw/net`, the wifi files in `hw/misc` (`esp32_wifi_ap.c`, `esp32_wlan*`, `esp32s3_wifi.c`, `esp32_phya.c`, `esp32_fe.c`), `include/hw/net`, matching `include/hw/misc` wifi headers | |
| scripts-docs-ci | `eink-qemu/` (except `porting/`), `EINK-QEMU.md`, `README.md`, `test_qemu.sh`, `.github`, `.gitlab-ci.yml` | |
| machines | `hw/xtensa`, `hw/riscv`, `include/hw/xtensa`, `include/hw/riscv`, `configs/devices`, `pc-bios` | wires everything; boots firmware |

The layers included their directories' `meson.build`, `Kconfig` and
`trace-events` entries. The machine symbols `XTENSA_ESP32`, `XTENSA_ESP32S3`,
`RISCV_ESP32C3` and `RISCV_ESP32C6` were introduced in the core layer;
individual device symbols are selected by the machines that use them.

## Porting decisions

The port aimed to preserve registers, timings, properties, defaults and the
command-line interface so firmware could boot unchanged. The layer notes
record API adaptations and deliberate drops where upstream already supplied
an implementation.

QEMU 11.1 adaptations include `ResettableClass` and
`device_class_set_legacy_reset`, `DEFINE_PROP_*` arrays without end markers,
the split `exec/` and `accel/tcg/` headers, and `qemu/target-info.h`. Port
commits record `Ported-from: x4prosim-pre-rebase` and credit the original
authors. The review contains the per-file accounting, build evidence and
behavioral checks, including their limits.
