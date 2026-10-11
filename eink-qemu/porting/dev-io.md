# Device I/O port to QEMU 11.1.2

All 31 assigned C sources and 28 matching headers from `x4prosim-pre-rebase`
are ported on branch `serialx/port-dev-io`. Both requested executables build
and link without warnings from these sources. Register operations, reset
bodies, IRQ transitions, named GPIO lines (including `pin-in`, `pin-out`, and
`wake`), timer scheduling, property names, and defaults retain the old fork's
implementation.

This layer starts at `8980d7be81`, which already contains the core and target
ports. Its own diff is `git diff 8980d7be81..HEAD`; a diff from `v11.1.2` also
contains those inherited layers. No machine code or cross-layer headers were
changed or copied, and no temporary Kconfig scaffolding was needed.

## API decisions

- Keep the existing Resettable hold callbacks; the reference already uses the current reset API.
- Change QOM class initialization data to `const void *` as required by `TypeInfo.class_init`.
- Use `static const Property` arrays without `DEFINE_PROP_END_OF_LIST`; remove the FRC timer's sentinel-only array and its registration, retaining the instance `apb_freq` property.
- Rename the UART member type from `CharBackend` to `CharFrontend`; existing frontend callbacks, watches, baud calculations, and receive timers remain valid.
- Move device infrastructure includes to `hw/core/`, and DMA, block backend, and RTC includes to `system/`; include timer and sysbus definitions explicitly where embedded fields require them.
- Remove obsolete unused `hw/hw.h`, `sysemu/sysemu.h`, and QDict includes instead of restoring compatibility headers.
- Remove unused RTC controller includes from the C3/S3 UARTs and the unused `esp32_reg.h` include from the FRC timer header; no cross-layer copies are necessary.
- Keep the I2C slave `event`, `recv`, and `send` callbacks unchanged: their signatures match 11.1.2.
- Keep `qemu_irq`, named GPIO registration, virtual-clock timer calls, and FRC trace formats unchanged: they compile with the 11.1.2 APIs and trace generator.
- Strip trailing whitespace and terminal blank lines in imported files; no behavioral fixes are bundled into this port.

## Per-file record

All paths below were imported from the reference tag unless described as build
integration. “Unchanged” means the reference implementation needed no API
adaptation; whitespace cleanup may still apply.

| File | Port decision |
| --- | --- |
| `hw/char/esp32_uart.c` | Const QOM class data; const, counted property array; hw/core includes; remove unused sysemu umbrella. |
| `hw/char/esp32c3_uart.c` | Const QOM class data; hw/core includes; remove unused sysemu umbrella; remove unused cross-layer include. |
| `hw/char/esp32c6_uart.c` | Const QOM class data. |
| `hw/char/esp32s3_uart.c` | Const QOM class data; hw/core includes; remove unused sysemu umbrella; remove unused cross-layer include. |
| `hw/dma/esp32c3_gdma.c` | Const QOM class data. |
| `hw/dma/esp32c6_gdma.c` | Unchanged reference implementation. |
| `hw/dma/esp32s3_gdma.c` | Const QOM class data. |
| `hw/dma/esp_gdma.c` | Const QOM class data; const, counted property array; hw/core includes; system includes. |
| `hw/gpio/esp32_gpio.c` | Const QOM class data; const, counted property array; hw/core includes; remove obsolete hw/hw.h. |
| `hw/gpio/esp32c3_gpio.c` | Const QOM class data; hw/core includes; remove obsolete hw/hw.h. |
| `hw/gpio/esp32c6_gpio.c` | Const QOM class data. |
| `hw/gpio/esp32s3_gpio.c` | Const QOM class data; const, counted property array; hw/core includes; remove obsolete hw/hw.h. |
| `hw/i2c/esp32_i2c.c` | Const QOM class data; hw/core includes. |
| `hw/i2c/esp32s3_i2c.c` | Const QOM class data; hw/core includes. |
| `hw/nvram/esp32_efuse.c` | Const QOM class data; const, counted property array; hw/core includes; remove unused sysemu umbrella. |
| `hw/nvram/esp32c3_efuse.c` | Const QOM class data. |
| `hw/nvram/esp32c6_efuse.c` | Unchanged reference implementation. |
| `hw/nvram/esp32s3_efuse.c` | Const QOM class data. |
| `hw/nvram/esp_efuse.c` | Const QOM class data; const, counted property array; hw/core includes; remove unused sysemu umbrella; remove unused QDict include. |
| `hw/rtc/bm8563.c` | Const QOM class data; system includes; I2C callbacks and battery-backed state unchanged. |
| `hw/rtc/ds3231.c` | Const QOM class data; system includes; I2C callbacks and battery-backed state unchanged. |
| `hw/timer/esp32_frc_timer.c` | Const QOM class data; remove empty property array; preserve instance apb_freq; hw/core includes; remove obsolete hw/hw.h. |
| `hw/timer/esp32_timg.c` | Const QOM class data; const, counted property array; hw/core includes; remove obsolete hw/hw.h. |
| `hw/timer/esp32c3_systimer.c` | Const QOM class data; hw/core includes. |
| `hw/timer/esp32c3_timg.c` | Unchanged reference implementation. |
| `hw/timer/esp32c6_systimer.c` | Const QOM class data; hw/core includes. |
| `hw/timer/esp32c6_timg.c` | Unchanged reference implementation. |
| `hw/timer/esp32s3_systimer.c` | Const QOM class data; hw/core includes; remove obsolete hw/hw.h. |
| `hw/timer/esp32s3_timg.c` | Const QOM class data. |
| `hw/timer/esp_systimer.c` | Const QOM class data; hw/core includes; remove obsolete hw/hw.h. |
| `hw/timer/esp_timg.c` | Const QOM class data; const, counted property array; hw/core includes; remove obsolete hw/hw.h. |
| `include/hw/char/esp32_uart.h` | CharFrontend member; chardev property unchanged; hw/core includes; explicit timer definitions; remove obsolete hw/hw.h. |
| `include/hw/char/esp32c3_uart.h` | Unchanged reference implementation. |
| `include/hw/char/esp32c6_uart.h` | Unchanged reference implementation. |
| `include/hw/char/esp32s3_uart.h` | Unchanged reference implementation. |
| `include/hw/dma/esp32c3_gdma.h` | hw/core includes; remove obsolete hw/hw.h. |
| `include/hw/dma/esp32c6_gdma.h` | Unchanged reference implementation. |
| `include/hw/dma/esp32s3_gdma.h` | Unchanged reference implementation. |
| `include/hw/dma/esp_gdma.h` | hw/core includes; remove obsolete hw/hw.h. |
| `include/hw/gpio/esp32_gpio.h` | hw/core includes; remove obsolete hw/hw.h. |
| `include/hw/gpio/esp32c3_gpio.h` | hw/core includes; remove obsolete hw/hw.h. |
| `include/hw/gpio/esp32c6_gpio.h` | Unchanged reference implementation. |
| `include/hw/gpio/esp32s3_gpio.h` | hw/core includes; remove obsolete hw/hw.h. |
| `include/hw/i2c/esp32_i2c.h` | hw/core includes. |
| `include/hw/nvram/esp32_efuse.h` | hw/core includes; system includes; explicit timer definitions; remove obsolete hw/hw.h. |
| `include/hw/nvram/esp32c3_efuse.h` | Unchanged reference implementation. |
| `include/hw/nvram/esp32c6_efuse.h` | Unchanged reference implementation. |
| `include/hw/nvram/esp32s3_efuse.h` | Unchanged reference implementation. |
| `include/hw/nvram/esp_efuse.h` | hw/core includes; system includes; explicit timer definitions; remove obsolete hw/hw.h. |
| `include/hw/timer/esp32_frc_timer.h` | hw/core includes; explicit timer definitions; remove obsolete hw/hw.h; remove unused cross-layer include. |
| `include/hw/timer/esp32_timg.h` | hw/core includes; explicit timer definitions; explicit sysbus definitions; remove obsolete hw/hw.h. |
| `include/hw/timer/esp32c3_systimer.h` | hw/core includes; remove obsolete hw/hw.h. |
| `include/hw/timer/esp32c3_timg.h` | remove obsolete hw/hw.h. |
| `include/hw/timer/esp32c6_systimer.h` | Unchanged reference implementation. |
| `include/hw/timer/esp32c6_timg.h` | Unchanged reference implementation. |
| `include/hw/timer/esp32s3_systimer.h` | hw/core includes; remove obsolete hw/hw.h. |
| `include/hw/timer/esp32s3_timg.h` | remove obsolete hw/hw.h. |
| `include/hw/timer/esp_systimer.h` | hw/core includes; explicit timer definitions; explicit sysbus definitions; remove obsolete hw/hw.h. |
| `include/hw/timer/esp_timg.h` | hw/core includes; remove obsolete hw/hw.h. |
| `hw/char/meson.build` | Restore UART source lists for the four original ESP machine symbols. |
| `hw/gpio/meson.build` | Restore GPIO source lists and inherited model dependencies for the four original ESP machine symbols. |
| `hw/i2c/meson.build` | Restore ESP32, S3, and C3 source lists before system_ss consumes i2c_ss; retain the original CONFIG_I2C gate. |
| `hw/rtc/meson.build` | Compile BM8563 and DS3231 on their own CONFIG_BM8563 and CONFIG_DS3231 symbols. |
| `hw/rtc/Kconfig` | Restore BM8563 and DS3231 symbols with their I2C dependency. |
| `hw/nvram/meson.build` | Restore eFuse sources and base classes for the four original ESP machine symbols. |
| `hw/dma/meson.build` | Restore GDMA base/variant sources for the C3, C6, and S3 symbols. |
| `hw/timer/meson.build` | Restore FRC, timer-group, and system-timer sources for the four original ESP machine symbols. |
| `hw/timer/trace-events` | Restore all five FRC events unchanged; trace generation succeeds. |
| `hw/xtensa/Kconfig` | Add only select BM8563 under XTENSA_ESP32S3, matching the reference machine. |
| `hw/riscv/Kconfig` | Add only select DS3231 under RISCV_ESP32C3, matching the reference machine. |
| `eink-qemu/porting/dev-io.md` | Record file decisions, build evidence, compatibility checks, and integration limits. |

There are no matching RTC headers or S3 I2C header in the old fork: those
models define their private state in the C source. Kconfig/trace files in the
other owned directories had no reference additions and were left unchanged.

## Validation

Configured with the exact guide command, without extra flags or host source
workarounds:

```sh
mkdir -p build
cd build
../configure --target-list=xtensa-softmmu,riscv32-softmmu --enable-gcrypt --enable-sdl \
  --enable-slirp --disable-gnutls --disable-strip --disable-user --disable-capstone \
  --disable-vnc --disable-gtk --disable-docs
ninja qemu-system-xtensa qemu-system-riscv32
```

Configure and the final build exited 0. Configure used the network access
already described in `host.md` to fetch worktree-local dependencies. The final
build recompiled every assigned C file and linked both executables; its entire
output is pasted below. No `warning:` or `error:` diagnostics were emitted.
The log is retained locally as `build/acceptance-dev-io.log`.

```text
[1/39] Generating qemu-version.h with a custom command (wrapped by meson to capture output)
[2/36] Compiling C object libsystem.a.p/hw_dma_esp32c6_gdma.c.o
[3/36] Compiling C object libsystem.a.p/hw_rtc_bm8563.c.o
[4/36] Compiling C object libsystem.a.p/hw_gpio_esp32c3_gpio.c.o
[5/36] Compiling C object libsystem.a.p/hw_gpio_esp32c6_gpio.c.o
[6/36] Compiling C object libsystem.a.p/hw_char_esp32c6_uart.c.o
[7/36] Compiling C object libsystem.a.p/hw_dma_esp32s3_gdma.c.o
[8/36] Compiling C object libsystem.a.p/hw_gpio_esp32_gpio.c.o
[9/36] Compiling C object libsystem.a.p/hw_i2c_esp32_i2c.c.o
[10/36] Compiling C object libsystem.a.p/hw_char_esp32c3_uart.c.o
[11/36] Compiling C object libsystem.a.p/hw_gpio_esp32s3_gpio.c.o
[12/36] Compiling C object libsystem.a.p/hw_nvram_esp32c6_efuse.c.o
[13/36] Compiling C object libsystem.a.p/hw_char_esp32s3_uart.c.o
[14/36] Compiling C object libsystem.a.p/hw_nvram_esp32c3_efuse.c.o
[15/36] Compiling C object libsystem.a.p/hw_dma_esp32c3_gdma.c.o
[16/36] Compiling C object libsystem.a.p/hw_char_esp32_uart.c.o
[17/36] Compiling C object libsystem.a.p/hw_i2c_esp32s3_i2c.c.o
[18/36] Compiling C object libsystem.a.p/hw_nvram_esp32s3_efuse.c.o
[19/36] Compiling C object libsystem.a.p/hw_nvram_esp32_efuse.c.o
[20/36] Compiling C object libsystem.a.p/hw_nvram_esp_efuse.c.o
[21/36] Compiling C object libsystem.a.p/hw_dma_esp_gdma.c.o
[22/36] Compiling C object libsystem.a.p/hw_rtc_ds3231.c.o
[23/36] Compiling C object libsystem.a.p/hw_timer_esp32c3_timg.c.o
[24/36] Compiling C object libsystem.a.p/hw_timer_esp32c6_timg.c.o
[25/36] Compiling C object libsystem.a.p/hw_timer_esp32s3_timg.c.o
[26/36] Compiling C object libsystem.a.p/hw_timer_esp32c3_systimer.c.o
[27/36] Compiling C object libsystem.a.p/hw_timer_esp32c6_systimer.c.o
[28/36] Compiling C object libsystem.a.p/hw_timer_esp32s3_systimer.c.o
[29/36] Compiling C object libsystem.a.p/hw_timer_esp32_frc_timer.c.o
[30/36] Compiling C object libsystem.a.p/hw_timer_esp_systimer.c.o
[31/36] Compiling C object libsystem.a.p/hw_timer_esp32_timg.c.o
[32/36] Compiling C object libsystem.a.p/hw_timer_esp_timg.c.o
[33/36] Linking target qemu-system-riscv32-unsigned
[34/36] Linking target qemu-system-xtensa-unsigned
[35/36] Generating qemu-system-xtensa with a custom command
[36/36] Generating qemu-system-riscv32 with a custom command
```

QMP smoke validation started both new executables and the read-only reference
executables with `-machine none -S -display none -nodefaults -qmp stdio`.
`qom-list-types` confirmed all 31 unique device types (including four abstract
base types) across the two targets. For every concrete type available on each
target, `device-list-properties` matched every reference property by name,
type, and default value; 11.1 additionally exposes instance child/link
properties. All processes exited cleanly through QMP `quit`.

```text
$ python3 build/check-dev-io-qom.py
xtensa: 14 types registered; old property names/types/defaults match
riscv32: 17 types registered; old property names/types/defaults match
31 unique device types covered; both current and reference processes exited cleanly
```

The local comparison script and raw results are retained in
`build/check-dev-io-qom.py` and `build/dev-io-qom.json`. Review of the diff
against the old tag confirms that runtime register, GPIO, IRQ, reset, and
timing logic was not changed. No unresolved cross-layer link symbols remain
in this worktree. Firmware boot and end-to-end peripheral workloads still
belong to the later integrated machine validation; this layer does not claim
those tests were run.
