# dev-misc port to QEMU 11.1.2

The layer restores 51 C sources and 48 public headers from
`x4prosim-pre-rebase`. All 51 objects compile without warnings, and all 48
headers pass independent syntax checks with `-Werror`. Both system targets
reach final linking and fail only on three GDMA helpers owned by dev-io;
integrated linking and firmware boot remain for the machine layer.

The coordinator explicitly changed this wave’s acceptance to compilation of
every owned object, permitting temporary cross-layer headers and expected
link failures (Orca message `msg_fcdce5a3054a`, 2026-10-09).

## API changes and preservation

- Use the relocated `hw/core/` and `system/` headers; remove the obsolete
  `hw/hw.h` umbrella instead of restoring compatibility shims.
- Adapt QOM class initializers to `const void *data`.
- Make nonempty property arrays const and remove end markers. Remove empty
  arrays and their registration calls: QEMU 11.1 rejects zero-sized arrays.
- Rename USB-JTAG frontend state from `CharBackend` to `CharFrontend`.
- Make ESP32 DPORT and RTC headers include their register constants directly.
- Existing `ResettableClass` hold callbacks already match QEMU 11.1 and are
  retained. I2C event/send/recv signatures, IRQ and memory-region calls,
  chardev handler registration, timers and `qemu_system_suspend_request()`
  remain compatible; no behavioral substitutions were needed.
- Crypto retains the original gcrypt calls and core-layer AES, SHA and HMAC
  helpers. The configured gcrypt build compiles every crypto model.
- Register values, timing, property names/defaults, and sensor behavior are
  preserved. No owned model or guest-visible feature was dropped.

## Per-file record

Every row is a file added to upstream v11.1.2. The adaptation column compares
that restored file with `x4prosim-pre-rebase`; “unchanged” means byte-identical.

| File | Adaptation |
| --- | --- |
| `hw/misc/bq27220.c` | Use relocated core/system headers; Const class initializer data; Const properties without end marker. |
| `hw/misc/cw2017.c` | Const class initializer data. |
| `hw/misc/esp32_aes.c` | Use relocated core/system headers; Const class initializer data. |
| `hw/misc/esp32_crosscore_int.c` | Use relocated core/system headers; Const class initializer data; Const properties without end marker. |
| `hw/misc/esp32_dport.c` | Remove obsolete hw/hw.h; Use relocated core/system headers; Const class initializer data; Const properties without end marker. |
| `hw/misc/esp32_flash_enc.c` | Remove obsolete hw/hw.h; Use relocated core/system headers; Put osdep.h first; Const class initializer data. |
| `hw/misc/esp32_ledc.c` | Use relocated core/system headers; Const class initializer data. |
| `hw/misc/esp32_rng.c` | Remove obsolete hw/hw.h; Use relocated core/system headers. |
| `hw/misc/esp32_rsa.c` | Remove obsolete hw/hw.h; Use relocated core/system headers; Const class initializer data. |
| `hw/misc/esp32_rtc_cntl.c` | Remove obsolete hw/hw.h; Use relocated core/system headers; Const class initializer data; Drop empty property array and registration. |
| `hw/misc/esp32_sha.c` | Remove obsolete hw/hw.h; Use relocated core/system headers. |
| `hw/misc/esp32c3_aes.c` | Unchanged; compatible with 11.1. |
| `hw/misc/esp32c3_cache.c` | Remove obsolete hw/hw.h; Use relocated core/system headers; Const class initializer data; Drop empty property array and registration. |
| `hw/misc/esp32c3_ds.c` | Const class initializer data. |
| `hw/misc/esp32c3_hmac.c` | Const class initializer data. |
| `hw/misc/esp32c3_jtag.c` | Remove obsolete hw/hw.h; Use relocated core/system headers; Const class initializer data. |
| `hw/misc/esp32c3_rsa.c` | Const class initializer data. |
| `hw/misc/esp32c3_rtc_cntl.c` | Remove obsolete hw/hw.h; Use relocated core/system headers; Const class initializer data. |
| `hw/misc/esp32c3_saradc.c` | Use relocated core/system headers. |
| `hw/misc/esp32c3_sha.c` | Const class initializer data. |
| `hw/misc/esp32c3_xts_aes.c` | Use relocated core/system headers; Const class initializer data; clean whitespace. |
| `hw/misc/esp32c6_cache.c` | Remove obsolete hw/hw.h; Use relocated core/system headers; Const class initializer data; Drop empty property array and registration. |
| `hw/misc/esp32c6_i2c_ana_mst.c` | Use relocated core/system headers; Const class initializer data. |
| `hw/misc/esp32c6_intpri.c` | Use relocated core/system headers; Const class initializer data. |
| `hw/misc/esp32c6_jtag.c` | Remove obsolete hw/hw.h; Use relocated core/system headers; Const class initializer data; Const properties without end marker. |
| `hw/misc/esp32c6_lp.c` | Use relocated core/system headers; Const class initializer data. |
| `hw/misc/esp32c6_modem.c` | Use relocated core/system headers; Const class initializer data. |
| `hw/misc/esp32c6_pcr.c` | Use relocated core/system headers; Const class initializer data. |
| `hw/misc/esp32c6_sha.c` | Const class initializer data. |
| `hw/misc/esp32c6_spi_mem.c` | Use relocated core/system headers; Const class initializer data. |
| `hw/misc/esp32s3_aes.c` | Unchanged; compatible with 11.1. |
| `hw/misc/esp32s3_ana.c` | Remove obsolete hw/hw.h; Use relocated core/system headers; clean whitespace. |
| `hw/misc/esp32s3_cache.c` | Remove obsolete hw/hw.h; Use relocated core/system headers; Const class initializer data; Drop empty property array and registration. |
| `hw/misc/esp32s3_ds.c` | Const class initializer data. |
| `hw/misc/esp32s3_hmac.c` | Const class initializer data. |
| `hw/misc/esp32s3_pms.c` | Remove obsolete hw/hw.h; Use relocated core/system headers. |
| `hw/misc/esp32s3_regstub.c` | Use relocated core/system headers; Const class initializer data; Const properties without end marker. |
| `hw/misc/esp32s3_rng.c` | Remove obsolete hw/hw.h; Use relocated core/system headers. |
| `hw/misc/esp32s3_rsa.c` | Const class initializer data. |
| `hw/misc/esp32s3_rtc_cntl.c` | Remove obsolete hw/hw.h; Use relocated core/system headers; Const class initializer data; Drop empty property array and registration. |
| `hw/misc/esp32s3_sens.c` | Use relocated core/system headers; Const class initializer data; Const properties without end marker. |
| `hw/misc/esp32s3_sha.c` | Const class initializer data. |
| `hw/misc/esp32s3_usb_jtag.c` | Use relocated core/system headers; Const class initializer data; Const properties without end marker; Use CharFrontend. |
| `hw/misc/esp32s3_xts_aes.c` | Use relocated core/system headers; Const class initializer data; clean whitespace. |
| `hw/misc/esp_aes.c` | Use relocated core/system headers; Const class initializer data. |
| `hw/misc/esp_ds.c` | Use relocated core/system headers; Const class initializer data. |
| `hw/misc/esp_hmac.c` | Use relocated core/system headers; Const class initializer data. |
| `hw/misc/esp_rsa.c` | Remove obsolete hw/hw.h; Use relocated core/system headers; Const class initializer data. |
| `hw/misc/esp_sha.c` | Remove obsolete hw/hw.h; Use relocated core/system headers; Const class initializer data. |
| `hw/misc/qmi8658.c` | Const class initializer data. |
| `hw/misc/ssi_psram.c` | Use relocated core/system headers; Const class initializer data; Const properties without end marker. |
| `include/hw/misc/esp32_aes.h` | Remove obsolete hw/hw.h; Use relocated core/system headers. |
| `include/hw/misc/esp32_crosscore_int.h` | Remove obsolete hw/hw.h; Use relocated core/system headers. |
| `include/hw/misc/esp32_dport.h` | Remove obsolete hw/hw.h; Use relocated core/system headers; Include ESP32 register constants directly; clean whitespace. |
| `include/hw/misc/esp32_flash_enc.h` | Remove obsolete hw/hw.h; Use relocated core/system headers. |
| `include/hw/misc/esp32_ledc.h` | Remove obsolete hw/hw.h; Use relocated core/system headers. |
| `include/hw/misc/esp32_reg.h` | No API changes; clean whitespace. |
| `include/hw/misc/esp32_rng.h` | Remove obsolete hw/hw.h; Use relocated core/system headers; clean whitespace. |
| `include/hw/misc/esp32_rsa.h` | Remove obsolete hw/hw.h; Use relocated core/system headers. |
| `include/hw/misc/esp32_rtc_cntl.h` | Remove obsolete hw/hw.h; Use relocated core/system headers; Include ESP32 register constants directly. |
| `include/hw/misc/esp32_sha.h` | Remove obsolete hw/hw.h; Use relocated core/system headers. |
| `include/hw/misc/esp32c3_aes.h` | Unchanged; compatible with 11.1. |
| `include/hw/misc/esp32c3_cache.h` | Remove obsolete hw/hw.h; Use relocated core/system headers; clean whitespace. |
| `include/hw/misc/esp32c3_ds.h` | Unchanged; compatible with 11.1. |
| `include/hw/misc/esp32c3_hmac.h` | Unchanged; compatible with 11.1. |
| `include/hw/misc/esp32c3_jtag.h` | Remove obsolete hw/hw.h; Use relocated core/system headers; clean whitespace. |
| `include/hw/misc/esp32c3_reg.h` | Unchanged; compatible with 11.1. |
| `include/hw/misc/esp32c3_rsa.h` | Unchanged; compatible with 11.1. |
| `include/hw/misc/esp32c3_rtc_cntl.h` | Remove obsolete hw/hw.h; Use relocated core/system headers. |
| `include/hw/misc/esp32c3_sha.h` | Unchanged; compatible with 11.1. |
| `include/hw/misc/esp32c3_xts_aes.h` | Remove obsolete hw/hw.h; Use relocated core/system headers. |
| `include/hw/misc/esp32c6_cache.h` | Remove obsolete hw/hw.h; Use relocated core/system headers. |
| `include/hw/misc/esp32c6_i2c_ana_mst.h` | Use relocated core/system headers. |
| `include/hw/misc/esp32c6_intpri.h` | Use relocated core/system headers. |
| `include/hw/misc/esp32c6_jtag.h` | Remove obsolete hw/hw.h; Use relocated core/system headers; Use CharFrontend. |
| `include/hw/misc/esp32c6_lp.h` | Use relocated core/system headers. |
| `include/hw/misc/esp32c6_modem.h` | Use relocated core/system headers. |
| `include/hw/misc/esp32c6_pcr.h` | Use relocated core/system headers. |
| `include/hw/misc/esp32c6_reg.h` | Unchanged; compatible with 11.1. |
| `include/hw/misc/esp32c6_sha.h` | Unchanged; compatible with 11.1. |
| `include/hw/misc/esp32c6_spi_mem.h` | Use relocated core/system headers. |
| `include/hw/misc/esp32s3_aes.h` | Unchanged; compatible with 11.1. |
| `include/hw/misc/esp32s3_ana.h` | Remove obsolete hw/hw.h; Use relocated core/system headers; clean whitespace. |
| `include/hw/misc/esp32s3_cache.h` | Remove obsolete hw/hw.h; Use relocated core/system headers; clean whitespace. |
| `include/hw/misc/esp32s3_ds.h` | Unchanged; compatible with 11.1. |
| `include/hw/misc/esp32s3_hmac.h` | Unchanged; compatible with 11.1. |
| `include/hw/misc/esp32s3_pms.h` | Remove obsolete hw/hw.h; Use relocated core/system headers. |
| `include/hw/misc/esp32s3_reg.h` | Unchanged; compatible with 11.1. |
| `include/hw/misc/esp32s3_rng.h` | Remove obsolete hw/hw.h; Use relocated core/system headers; clean whitespace. |
| `include/hw/misc/esp32s3_rsa.h` | Unchanged; compatible with 11.1. |
| `include/hw/misc/esp32s3_rtc_cntl.h` | Remove obsolete hw/hw.h; Use relocated core/system headers. |
| `include/hw/misc/esp32s3_sha.h` | Unchanged; compatible with 11.1. |
| `include/hw/misc/esp32s3_xts_aes.h` | Remove obsolete hw/hw.h; Use relocated core/system headers. |
| `include/hw/misc/esp_aes.h` | Remove obsolete hw/hw.h; Use relocated core/system headers. |
| `include/hw/misc/esp_ds.h` | Remove obsolete hw/hw.h; Use relocated core/system headers; clean whitespace. |
| `include/hw/misc/esp_hmac.h` | Remove obsolete hw/hw.h; Use relocated core/system headers. |
| `include/hw/misc/esp_rsa.h` | Remove obsolete hw/hw.h; Use relocated core/system headers. |
| `include/hw/misc/esp_sha.h` | Remove obsolete hw/hw.h; Use relocated core/system headers. |
| `include/hw/misc/ssi_psram.h` | Remove obsolete hw/hw.h; Use relocated core/system headers; clean whitespace. |

| Build/configuration file | Change |
| --- | --- |
| `hw/misc/meson.build` | Restore all owned source lists under the four existing ESP machine symbols; retain gcrypt conditions; omit Wi-Fi/RF sources owned by dev-net. |
| `hw/misc/Kconfig` | Restore CW2017, BQ27220 and QMI8658 symbols with I2C dependencies. |
| `hw/xtensa/Kconfig` | Add `select CW2017` under XTENSA_ESP32S3. |
| `hw/riscv/Kconfig` | Add `select BQ27220` and `select QMI8658` under RISCV_ESP32C3. |

## Cross-layer headers used for validation

These headers were copied from the old tag solely for compilation and removed
before committing. They are not part of this layer’s commit. Their owners must
supply the corresponding ports before rebuilding this checkout. Changes to
the temporary copies were limited to the following include relocations:

| Temporary header | Required adaptation |
| --- | --- |
| `include/hw/nvram/esp32c3_efuse.h` | `esp_efuse.h` → `hw/nvram/esp_efuse.h`. |
| `include/hw/riscv/esp32c3_clk.h` | Remove `hw/hw.h`; `hw/sysbus.h` → `hw/core/sysbus.h`; `hw/registerfields.h` → `hw/core/registerfields.h`. |
| `include/hw/dma/esp_gdma.h` | Remove `hw/hw.h`; `hw/sysbus.h` → `hw/core/sysbus.h`; `hw/registerfields.h` → `hw/core/registerfields.h`. |
| `include/hw/nvram/esp_efuse.h` | Remove `hw/hw.h`; `hw/registerfields.h` → `hw/core/registerfields.h`; `hw/sysbus.h` → `hw/core/sysbus.h`; `sysemu/block-backend.h` → `system/block-backend.h`. |
| `include/hw/xtensa/esp32s3_clk.h` | Remove `hw/hw.h`; `hw/sysbus.h` → `hw/core/sysbus.h`; `hw/registerfields.h` → `hw/core/registerfields.h`. |
| `include/hw/nvram/esp32_efuse.h` | Remove `hw/hw.h`; `hw/registerfields.h` → `hw/core/registerfields.h`; `hw/sysbus.h` → `hw/core/sysbus.h`; `sysemu/block-backend.h` → `system/block-backend.h`. |
| `include/hw/timer/esp_timg.h` | Remove `hw/hw.h`; `hw/registerfields.h` → `hw/core/registerfields.h`; `hw/sysbus.h` → `hw/core/sysbus.h`. |

No cross-layer source files or temporary CONFIG overrides were used.

## Validation

Configure used the exact flags in the porting guide:

```sh
mkdir -p build
cd build
../configure --target-list=xtensa-softmmu,riscv32-softmmu --enable-gcrypt --enable-sdl \
  --enable-slirp --disable-gnutls --disable-strip --disable-user --disable-capstone \
  --disable-vnc --disable-gtk --disable-docs
```

The first sandboxed configure could not resolve PyPI. Retrying the same
command with network permission succeeded, matching the already documented
workaround in `host.md`; no new flags or host workaround were needed.

Object coverage was derived from `build/compile_commands.json`, matching its
`file` entries to all 51 owned C files and collecting their `output` entries.
All 51 occur exactly once, under `libsystem.a.p`; this library is used by both
configured system targets. The object build command and result were:

```text
$ ninja -C build -k 0 $(cat build/owned-objects.txt)
51 owned C sources compiled; exit 0; zero compiler warnings/errors.
```

Each of the 48 owned headers was then included after `qemu/osdep.h` in an
otherwise empty translation unit and compiled with the flags from the
`esp32_rsa.c` compile database entry plus `-Werror -fsyntax-only -x c -`.
Result: 48 checked, 0 failed, 0 warnings.

Full target acceptance attempt:

```text
$ ninja -C build -k 0 qemu-system-xtensa qemu-system-riscv32
FAILED: qemu-system-xtensa-unsigned (final link)
FAILED: qemu-system-riscv32-unsigned (final link)
Undefined symbols for architecture arm64:
  _esp_gdma_get_channel_periph
  _esp_gdma_read_channel
  _esp_gdma_write_channel
ninja: build stopped: cannot make progress due to previous errors.
```

Both failures have the same three symbols, referenced by `esp_aes.c` and
`esp_sha.c`, and no other undefined symbols. They are implemented by
`hw/dma/esp_gdma.c` in the dev-io layer. No compiler diagnostics remain.
Boot and runtime behavior were not tested in this isolated layer.

Local logs are in the ignored `build/` directory: `configure.log`,
`build-objects.log`, `build-objects-final.log`, `headers-final.log`,
`build-full-final.log`, and `port-changes.diff` (comparison against the old tag).

## Compiled objects

```text
libsystem.a.p/hw_misc_cw2017.c.o
libsystem.a.p/hw_misc_bq27220.c.o
libsystem.a.p/hw_misc_qmi8658.c.o
libsystem.a.p/hw_misc_esp32_crosscore_int.c.o
libsystem.a.p/hw_misc_esp32_dport.c.o
libsystem.a.p/hw_misc_esp32_rng.c.o
libsystem.a.p/hw_misc_esp32_rtc_cntl.c.o
libsystem.a.p/hw_misc_esp32_sha.c.o
libsystem.a.p/hw_misc_esp32_aes.c.o
libsystem.a.p/hw_misc_esp32_ledc.c.o
libsystem.a.p/hw_misc_esp32_flash_enc.c.o
libsystem.a.p/hw_misc_ssi_psram.c.o
libsystem.a.p/hw_misc_esp32c3_cache.c.o
libsystem.a.p/hw_misc_esp_sha.c.o
libsystem.a.p/hw_misc_esp32c3_sha.c.o
libsystem.a.p/hw_misc_esp32c3_jtag.c.o
libsystem.a.p/hw_misc_esp32s3_usb_jtag.c.o
libsystem.a.p/hw_misc_esp32c3_saradc.c.o
libsystem.a.p/hw_misc_esp32s3_regstub.c.o
libsystem.a.p/hw_misc_esp32s3_ana.c.o
libsystem.a.p/hw_misc_esp32c3_rtc_cntl.c.o
libsystem.a.p/hw_misc_esp_hmac.c.o
libsystem.a.p/hw_misc_esp32c3_hmac.c.o
libsystem.a.p/hw_misc_esp32c6_cache.c.o
libsystem.a.p/hw_misc_esp32c6_i2c_ana_mst.c.o
libsystem.a.p/hw_misc_esp32c6_intpri.c.o
libsystem.a.p/hw_misc_esp32c6_jtag.c.o
libsystem.a.p/hw_misc_esp32c6_lp.c.o
libsystem.a.p/hw_misc_esp32c6_modem.c.o
libsystem.a.p/hw_misc_esp32c6_pcr.c.o
libsystem.a.p/hw_misc_esp32c6_spi_mem.c.o
libsystem.a.p/hw_misc_esp32c6_sha.c.o
libsystem.a.p/hw_misc_esp32s3_cache.c.o
libsystem.a.p/hw_misc_esp32s3_sens.c.o
libsystem.a.p/hw_misc_esp32s3_sha.c.o
libsystem.a.p/hw_misc_esp32s3_rtc_cntl.c.o
libsystem.a.p/hw_misc_esp32s3_rng.c.o
libsystem.a.p/hw_misc_esp32s3_pms.c.o
libsystem.a.p/hw_misc_esp32s3_hmac.c.o
libsystem.a.p/hw_misc_esp32_rsa.c.o
libsystem.a.p/hw_misc_esp_aes.c.o
libsystem.a.p/hw_misc_esp32c3_aes.c.o
libsystem.a.p/hw_misc_esp_rsa.c.o
libsystem.a.p/hw_misc_esp32c3_rsa.c.o
libsystem.a.p/hw_misc_esp_ds.c.o
libsystem.a.p/hw_misc_esp32c3_ds.c.o
libsystem.a.p/hw_misc_esp32c3_xts_aes.c.o
libsystem.a.p/hw_misc_esp32s3_aes.c.o
libsystem.a.p/hw_misc_esp32s3_rsa.c.o
libsystem.a.p/hw_misc_esp32s3_ds.c.o
libsystem.a.p/hw_misc_esp32s3_xts_aes.c.o
```
