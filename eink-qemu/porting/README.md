# Porting eink-qemu to QEMU 11.1.2

Branch `serialx/qemu-rebase` starts at upstream **v11.1.2** and re-applies the
fork in layers. The fork used to sit on Espressif's `esp-develop` (QEMU 9.2.2 +
126 Espressif commits) plus 53 eink-qemu commits; Espressif never moved past
9.2.2, so the whole stack is ported by hand. The old history stays on branch
`x4prosim` and tag `x4prosim-pre-rebase` (f9339150cd).

## Reference material

- Original fork tree: `git show x4prosim-pre-rebase:<path>`; original change
  to an upstream file: `git diff v9.2.2 x4prosim-pre-rebase -- <path>`; new
  file: `git checkout x4prosim-pre-rebase -- <path>` and then adapt it.
- Upstream churn on a file you touch: `git diff v9.2.2 v11.1.2 -- <path>`,
  and `git log --oneline v9.2.2..v11.1.2 -- <path>` for the why.
- `docs/devel/` in this tree documents the 11.1 APIs (reset, qdev, memory,
  translator). Look for how an upstream device of the same kind does it today.
- Read-only reference checkout with the old build and firmware images:
  `/Users/serialx/workspace/x4prosim` (branch `x4prosim`): `build/qemu-system-riscv32`,
  `build/qemu-system-xtensa`, `x3-flash.bin` (CrossPoint 1.6.5 for the X3,
  ESP32-C3), `sd.img`. Never modify that checkout.
- Project docs: `EINK-QEMU.md` and `README.md` in this tree (ported in the
  scripts/docs layer), and `eink-qemu/` for run scripts.

## Build (every worktree builds in its own `build/`)

```sh
mkdir -p build && cd build
../configure --target-list=xtensa-softmmu,riscv32-softmmu --enable-gcrypt --enable-sdl \
  --enable-slirp --disable-gnutls --disable-strip --disable-user --disable-capstone \
  --disable-vnc --disable-gtk --disable-docs
ninja qemu-system-xtensa qemu-system-riscv32
```

Host: macOS, Apple clang 21, Homebrew Python 3.14 (`/opt/homebrew/bin/python3`),
ninja 1.13, no system meson (configure uses the bundled wheel). Record any extra
configure flag or host workaround you needed in `eink-qemu/porting/host.md`.

## Layers and ownership

| layer | owns | notes |
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

Each layer also owns the `meson.build`, `Kconfig` and `trace-events` of the
directories it lists. A device layer may add `select FOO` lines for its own
devices under the machine entries in `hw/xtensa/Kconfig` and `hw/riscv/Kconfig`.
Do not edit files outside your layer; if you must, say so in your report.

The machine config symbols `XTENSA_ESP32`, `XTENSA_ESP32S3`, `RISCV_ESP32C3`,
`RISCV_ESP32C6` exist from the core layer on (default y), so device sources
gated on them compile as soon as their meson lines are in. A device gated on
its own symbol (`CONFIG_BQ27220`, ...) is compiled once a machine selects it;
to compile-check before that, add the `select` to the machine Kconfig entry.

## Rules

- Port behaviour faithfully: same registers, same timings, same properties and
  defaults, same command-line interface (`-machine x3`, `-machine x4pro`,
  `-global` property names). Firmware must boot unchanged (rule zero).
- Use the 11.1 idioms (`ResettableClass` or `device_class_set_legacy_reset`,
  `DEFINE_PROP_*` arrays without end markers, the split `exec/` and
  `accel/tcg/` headers, `qemu/target-info.h`, ...). Do not re-introduce
  removed compatibility shims.
- If an Espressif change is now obsolete upstream (fixed or restructured), drop
  it and record that in your layer's notes file `eink-qemu/porting/<layer>.md`,
  one line per decision. Record anything the next layer or reviewer must know.
- QEMU C style, 4-space indent. Keep the original file header comments.
- Build must stay warning-free for your files. Commit per layer with the
  message `eink-qemu: port <layer> to QEMU 11.1` and a body listing what was
  adapted, with `Ported-from: x4prosim-pre-rebase` and the original authors
  credited (`git log --format='%an' v9.2.2..x4prosim-pre-rebase -- <paths>`).
- Before `worker_done`: `git status` clean, `git diff --stat v11.1.2..HEAD`
  limited to your paths, and the acceptance command from your task output
  pasted in the report.
