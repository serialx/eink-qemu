# Storage, display, and input device port

All 15 owned C sources compile without warnings against QEMU v11.1.2 with
both `xtensa-softmmu` and `riscv32-softmmu` configured. The RISC-V executable
links and passes SPI SD protocol and virtual-clock latency checks. Xtensa
reaches its final link and needs `esp32_flash_encryption_enabled` and
`esp32_flash_encryption_get_result` from the parallel dev-misc layer.

The coordinator changed this wave's acceptance to warning-free owned objects
with temporary dependency headers (messages `msg_88353d9d5e59` and
`msg_35dd55951334`). No dependency implementation was copied or stubbed.
Full Xtensa linking, firmware boot, and visual/input integration remain for
the combined machine layer; they are not claimed by this report.

Source: `x4prosim-pre-rebase` (`f9339150cd`), originally based on v9.2.2.
This layer starts from `8980d7be81`, which already includes the core and CPU
ports. Changes in those inherited commits are outside this layer's diff.

## Integration requirements

- Create `TYPE_SD_CARD_SPI` (`sd-card-spi`) under an `ssi-sd` bus. Upstream
  removed the old `sd-card` plus `spi=true` interface. Native DWC cards still
  use `TYPE_SD_CARD`. The upstream SPI card now supplies framed R1/R2/R3/R7
  responses, including their idle/error bits.
- Supply the dev-misc encryption implementations and the dependency headers
  listed below. No machine source was edited here.
- Existing `-global` property names, types, and defaults are unchanged. A
  source comparison checked every imported `DEFINE_PROP_*` definition against
  the fork, excluding removed end markers. SSI SD's 11 latency properties
  remain zero by default, including signed `write-stop-decrement-us`.
- The SiFive qtest follows card initialization with CMD9 before data commands,
  as the upstream SPI model uses CID/CSD reads to enter transfer state.

## Per-file decisions

All imported class initializers use `const void *data`. Includes moved to
`hw/core/` and `system/`; no compatibility headers were reintroduced. Property
arrays are const and have no removed `DEFINE_PROP_END_OF_LIST` sentinel.
Existing reset phase callbacks remain intact.

| File | Port decision |
| --- | --- |
| `hw/ssi/esp32_spi.c` | Preserve SSI transfers, CS outputs, registers, interrupts, and flash-encryption calls; remove the empty property array and its registration. |
| `hw/ssi/esp32c3_spi.c` | Preserve transfers, command decoding, encryption links, and reset behavior; remove the empty property array. |
| `hw/ssi/esp32c6_spi.c` | Import the C3-derived subtype unchanged; the inherited SSI API remains supported. |
| `hw/ssi/esp32s3_spi.c` | Preserve flash transfers and encryption link behavior; remove the empty property array. |
| `hw/ssi/esp32s3_gpspi.c` | Preserve deferred wire transfer, 80 MHz divider calculation, interrupt timing, and all three overhead properties. |
| `hw/ssi/meson.build` | Add the five controllers under their original machine symbols; Meson deduplicates the C3/GPSPI sources used by multiple symbols. |
| `hw/ssi/trace-events` | Restore `esp32s3_gpspi_transfer` with identical fields and units. |
| `include/hw/ssi/esp32_spi.h` | Use explicit core sysbus/registerfield headers; preserve state layout and register definitions. |
| `include/hw/ssi/esp32c3_spi.h` | Same include adaptation; retain the dev-misc XTS-AES dependency and register definitions. |
| `include/hw/ssi/esp32c6_spi.h` | Import unchanged; derives from the C3 controller. |
| `include/hw/ssi/esp32s3_spi.h` | Use core includes; retain the S3 XTS-AES link and all register definitions. |
| `hw/sd/dwc_sdmmc.c` | Pass the 16-byte response-buffer size to `sdbus_do_command`; use its `size_t` result and remove the impossible negative-result branch. Preserve response registers, timeout handling, byte/DMA transfers, and interrupts. Use `device_class_set_legacy_reset`; remove the empty properties and duplicate includes. |
| `include/hw/sd/dwc_sdmmc.h` | Move sysbus include; keep state and register-facing fields. |
| `hw/sd/ssi-sd.c` | Start with the 11.1 file, preserve its bounded command API, card-inserted guard, and native SPI framing; reapply the fork hunks detailed below. |
| `hw/sd/Kconfig` | Restore `DWC_SDMMC`, selecting `SD`. |
| `hw/sd/meson.build` | Restore the DWC source entry. |
| `hw/sd/trace-events` | Restore `ssi_sd_latency` with identical sector/kind/us/deadline fields. |
| `hw/display/esp_rgb.c` | Use `qemu_graphic_console_create`, `qemu_console_set_surface`, and `qemu_console_update`. The update callback returns true, including its early exit, to indicate synchronous completion. Keep 16/32-bit surface formats, allocation flags, framebuffer addressing, and deferred resize behavior. |
| `include/hw/display/esp_rgb.h` | Include core sysbus/registerfields directly; preserve the state layout and registers. |
| `hw/display/uc8179.c` | Use the new console creation/update names and bool update result; retain `DisplaySurface` accessors and `qemu_console_resize`. Ink, ghosting, relaxation, LUT drive, animation, BUSY, and frame timing code is unchanged. |
| `hw/display/uc8279.c` | Same console API adaptation; preserve both UC8279 and `uc8253=true` paths, LUT/frame calculations, power timings, ink model, and RAM-plane trace timing. |
| `hw/display/ssd1677.c` | Same console API adaptation; preserve RAM windows, plane/shade interpretation, and BUSY timing. |
| `hw/display/Kconfig` | Restore `ESP_RGB`. |
| `hw/display/meson.build` | Restore RGB and the three panel model entries. |
| `hw/display/trace-events` | Restore both UC8279 events, preserving all upstream 11.1 events. |
| `hw/input/gt911.c` | Accept `QemuInputEvent`; read embedded `abs`/`btn` members. Preserve absolute-coordinate scaling, left-touch/right-home semantics, I2C register protocol, GPIOs, QOM scripting properties, and tap timer. |
| `hw/input/x3_keys.c` | Accept embedded `QemuInputKeyEvent`; translate Linux keycodes with `qemu_input_linux_to_qcode`, retaining all key mappings and ADC values. Store the required handler-registration result and unregister on unrealize; delete the boot timer on unrealize. Preserve the 1500 ms boot hold. |
| `hw/input/x4pro_keys.c` | Apply the same keyboard conversion and handler lifetime handling; preserve Up/Down/P, GPIO levels, and QOM properties. |
| `hw/input/Kconfig` | Restore `GT911`; select its required I2C bus so selecting the device builds it even before the machine layer arrives. |
| `hw/input/meson.build` | Restore GT911 and both key models. |
| `hw/block/m25p80.c` | Reapply all five manufacturer/mode hunks to the 11.1 structure, as detailed below. |
| `hw/xtensa/Kconfig` | Add only owned-device selects: DWC/RGB on ESP32 and ESP32-S3; GT911/SSI_SD on ESP32-S3. |
| `hw/riscv/Kconfig` | Add only owned SSI/flash/RGB selects: ESP_RGB/SSI_M25P80 on C3, SSI/SSI_M25P80 on C6. |

There are no fork changes to import under `include/hw/input` or
`include/hw/block`; upstream headers there remain unchanged. The shared
upstream `include/hw/ssi/ssi.h` and `include/hw/sd/sd.h` also remain unchanged.
All three e-paper panels retain the exact clockwise portrait transform
`dx = H - 1 - y; dy = x` and swapped console dimensions. The update callbacks
complete synchronously; BUSY and ink-animation timers still run independently
on `QEMU_CLOCK_VIRTUAL`.

## Every SSI SD fork hunk

Hunk numbers refer in order to
`git diff v9.2.2 x4prosim-pre-rebase -- hw/sd/ssi-sd.c`.

| Hunk / original starting line | Disposition on 11.1 |
| --- | --- |
| 1 / 1 | Restore the complete latency-model documentation. |
| 2 / 24 | Restore timer and trace includes; add the moved core qdev-properties include needed by the latency properties. Keep upstream's bswap include and removed unused includes. |
| 3 / 64 | Restore all latency properties, deadlines, sector/history counters, flags, and cached idle state. |
| 4 / 100 | Restore `ssi_sd_deadline` and write-busy polling. Keep the upstream five-byte response buffer and card-inserted guard ahead of polling. |
| 5 / 128 | Restore the `dispatch` label for same-byte command/token handling. |
| 6 / 158 | Restore STOP_TRAN busy: random extra, signed per-block decrement, saturation, and virtual-clock deadline. |
| 7 / 170 | Restore per-command read-deadline clearing and CMD0 history/capacity/busy/write-response reset. |
| 8 / 180 | The manual R3/R7 idle-bit fix and fake CMD13-from-CSD R2 conversion are obsolete: upstream now builds both response forms in `sd.c`. Retain direct upstream response copying, and recover CCS from bytes 1–4 of the framed CMD58 reply instead of old bytes 0–3. |
| 9 / 236 | Cache idle from the native R1 byte for the existing timing migration subsection. Restore accepted-command sector decoding, repeat/random write classification, start sector, and block-count reset. |
| 10 / 251 | Restore write data-response deadline creation, completed-block accounting/history, and first read-token deadline selection (repeat takes precedence over sequential). |
| 11 / 261 | Restore same-byte dispatch after R1, allowing an immediate write token; restore read-token polling until the deadline. |
| 12 / 290 | Restore read history and CMD18 next-token deadline after the completed block CRC. |
| 13 / 317 | Restore write-response flag at completed write CRC. |
| 14 / 345 | Restore optional timing VMState subsection, version 2/minimum 1, with all fields and version gates unchanged. |
| 15 / 363 | Restore the timing subsection registration under the unchanged base VMState version 7. |
| 16 / 387 | Restore all timing/history reset values and all 11 property names/types/zero defaults. Use a const array without an end marker. |
| 17 / 398 | Restore property registration while keeping upstream's legacy-reset helper and const class initializer. |

The removed card-status translation and negative response-size handling stay
removed. They belong to the obsolete upstream transport API, not to the fork's
latency model. Byte data transfers remain supported by the 11.1 SD bus helpers.

## Every m25p80 fork hunk

Hunk numbers refer to
`git diff v9.2.2 x4prosim-pre-rebase -- hw/block/m25p80.c`.

1. Original line 173: restore the one-byte
   `GIGADEVICE_CONTINUOUS_READ_MODE_CMD_LEN` constant.
2. Original line 465: restore `MAN_GIGADEVICE` in the manufacturer enum.
3. Original line 548: map JEDEC manufacturer `0xc8` to that enum, fixing the
   original mixed whitespace without changing behavior.
4. Original line 1048: DIO consumes one continuous-read-mode byte before data.
   Keep 11.1's other manufacturers and dummy-cycle helpers intact.
5. Original line 1095: preserve the fork's Gigadevice QIO consumption of one
   mode byte plus four dummy bytes. Use its own case: upstream changed Winbond
   from four to two dummy bytes, so simply restoring the shared case would
   change Gigadevice behavior. Winbond retains upstream's two-byte count.

## Build dependencies and reproducibility

Configure command, run in `build/`:

```sh
../configure --target-list=xtensa-softmmu,riscv32-softmmu --enable-gcrypt --enable-sdl \
  --enable-slirp --disable-gnutls --disable-strip --disable-user --disable-capstone \
  --disable-vnc --disable-gtk --disable-docs
```

No extra configure flags or new host workaround were needed. Configure needed
network access for Python packages/subprojects, as already documented in
`host.md`. Logs are in the ignored `build/` directory.

Eight temporary headers were copied from the fork to compile the cross-layer
interfaces, then removed before committing. To reproduce this isolated layer,
restore these headers and apply only the listed include-path adjustments;
a combined tree should use their owners' ported versions instead.

| Temporary header | Minimal adjustment |
| --- | --- |
| `include/hw/misc/esp32_flash_enc.h` | Replace hw.h/sysbus.h with core sysbus, registerfields with core registerfields. |
| `include/hw/misc/esp32c3_xts_aes.h` | Same include moves. |
| `include/hw/misc/esp32s3_xts_aes.h` | Same include moves. |
| `include/hw/nvram/esp32c3_efuse.h` | Verbatim. |
| `include/hw/nvram/esp32s3_efuse.h` | Verbatim. |
| `include/hw/nvram/esp_efuse.h` | Core sysbus/registerfields and `system/block-backend.h` includes. |
| `include/hw/riscv/esp32c3_clk.h` | Core sysbus/registerfields includes. |
| `include/hw/xtensa/esp32s3_clk.h` | Core sysbus include. |

Duplicate includes created by these substitutions were removed. None of these
scaffolds, generated configuration files, build logs, or test images is committed.

## Acceptance output

All source objects were removed and rebuilt for the final compile check.
The exact command from the repository root is `ninja -C build` followed by
all object paths printed below (one space-separated argument per path).
Exit status: **0**; compiler warnings: **0**.

```text
ninja: Entering directory `build'
[1/16] Generating qemu-version.h with a custom command (wrapped by meson to capture output)
[2/16] Compiling C object libsystem.a.p/hw_input_x4pro_keys.c.o
[3/16] Compiling C object libsystem.a.p/hw_ssi_esp32s3_gpspi.c.o
[4/16] Compiling C object libsystem.a.p/hw_ssi_esp32c6_spi.c.o
[5/16] Compiling C object libsystem.a.p/hw_sd_ssi-sd.c.o
[6/16] Compiling C object libsystem.a.p/hw_input_x3_keys.c.o
[7/16] Compiling C object libsystem.a.p/hw_ssi_esp32_spi.c.o
[8/16] Compiling C object libsystem.a.p/hw_input_gt911.c.o
[9/16] Compiling C object libsystem.a.p/hw_display_ssd1677.c.o
[10/16] Compiling C object libsystem.a.p/hw_ssi_esp32c3_spi.c.o
[11/16] Compiling C object libsystem.a.p/hw_sd_dwc_sdmmc.c.o
[12/16] Compiling C object libsystem.a.p/hw_ssi_esp32s3_spi.c.o
[13/16] Compiling C object libsystem.a.p/hw_display_esp_rgb.c.o
[14/16] Compiling C object libsystem.a.p/hw_display_uc8279.c.o
[15/16] Compiling C object libsystem.a.p/hw_display_uc8179.c.o
[16/16] Compiling C object libsystem.a.p/hw_block_m25p80.c.o
```

Full-target acceptance was also attempted with `ninja -C build -k 0
qemu-system-xtensa qemu-system-riscv32` (exit 1). Its only failure was:

```text
qemu-system-xtensa-unsigned: undefined symbols
  esp32_flash_encryption_enabled
  esp32_flash_encryption_get_result
```

`qemu-system-riscv32` linked successfully and reported QEMU 11.1.2.
The object list is shared by both configured targets; each binary links the
subset selected by its machine symbols. Generated trace sources also compiled.

## Executed protocol/timing check

A local Python qtest driver (`build/check-sd-port.py`) ran the RISC-V binary
on upstream `sifive_u`, with `-bios none -accel qtest -display none`, a fresh
64 MiB raw SD image, and explicit nonzero latency properties. It drove SPI2
MMIO at `0x10050000`, advanced the virtual clock to one microsecond before
each deadline and then to the exact deadline, and verified returned bytes.
CMD24 data tokens were sent immediately after R1, with no fill byte.

```text
PASS CMD8 R7 idle, CMD58 R3 ready, CMD9 CSD, CMD13 R2
PASS immediate CMD24 data token; random/repeated/sequential busy deadlines
PASS random/repeated/sequential CMD17 read deadlines and payload
PASS CMD25 block busy and STOP_TRAN random-extra/decrement (1000 us)
PASS CMD18 first and next block read deadlines
```

The checked delays were read random/repeat/sequential/next = 100/200/300/400 us;
write random/repeat/sequential/block = 500/600/700/800 us; and STOP_TRAN
1000 + 200 random-extra − 100 × 2 blocks = 1000 us. CMD17 and CMD18 payloads
matched the previously written 512-byte blocks. No panel visual QA, touch/key
routing QA, native DWC runtime test, or firmware boot was performed in this
isolated layer. Panel timing and register preservation were checked by source
comparison; input and display APIs were checked by warning-free compilation.
