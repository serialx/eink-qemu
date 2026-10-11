# Fixes after review

Verified on 2026-10-09 in `serialx/qemu-rebase`. The selective-reset regression
is fixed, both requested qtest suites pass, and X4 Pro Home matches a freshly
built **exact** pre-rebase baseline. X3 Home remains unchanged. Firmware and
reference images/checkouts were not modified. Evidence is under ignored
`build/fixes/`; the temporary `build/pre-rebase` worktree was removed after testing.

## Changes and scope

| Commit | Change | Classification |
| --- | --- | --- |
| `d01ff66f48` — `x4prosim: machines: keep CPU-only resets selective` | Set both Xtensa SoC classes' `ResettableClass.child_foreach` to NULL; the existing selective handler remains responsible for its domain resets. | Fixes review P1, a port regression. |
| `c55594dcd6` — `x4prosim: esp_rgb: allocate VRAM at realize` | Move VRAM and its DMA address-space initialization from construction to realization, alongside the existing internal-RAM address space. | Fixes inherited RGB introspection failure; unrealized X4 Pro RGB stays unused. |
| `a2719591d4` — `x4prosim: esp32_ledc: give QOM an owned LED color string` | Use `qdev_prop_set_string` for the existing blue color instead of assigning a string literal. | Inherited invalid free exposed by Xtensa introspection; separately authorized by the coordinator because `hw/misc/esp32_ledc.c` was outside the original ownership. |
| `6ed165fa8b` — `x4prosim: machines: defer CPU memory setup until realize` | Move CPU memory roots, aliases and address spaces out of SoC construction; set the strong `memory` link through QOM. | Inherited temporary-SoC lifetime defects exposed after the preceding fixes; confined to the owned machine files. |

The RGB and machine changes preserve register maps, property values, console
selection and realized CPU memory mappings. No `hw/display/uc8179.c` change was
needed. The machine preservation statement is corrected in [machines.md](machines.md).

## Reset acceptance

The review's paused, diskless qtest protocol reproduction was extended to the
APPCPU row and to digital/full-system reset controls. Old means
`/Users/serialx/workspace/x4prosim/build/qemu-system-xtensa`; new means
`./build/qemu-system-xtensa`. All three requested rows now match:

| Machine / request | UART0 CLKDIV | RTC OPTIONS0 write | Old before → after | New before → after |
| --- | --- | --- | --- | --- |
| ESP32 PROCPU | `0x3ff40014` | `0x3ff48000 = 0x20` | `0x345 → 0x345` | `0x345 → 0x345` |
| ESP32-S3 PROCPU | `0x60000014` | `0x60008000 = 0x20` | `0x345 → 0x345` | `0x345 → 0x345` |
| ESP32-S3 APPCPU | `0x60000014` | `0x60008000 = 0x10` | `0x1234 → 0x1234` | `0x1234 → 0x1234` |

Additional controls also match. ESP32 APPCPU preserves `0x1234`. Both machines'
digital reset (`OPTIONS0 = 0x80000000`) and QMP `system_reset` change UART CLKDIV
from `0x345` to the reset default `0x2b6`.

RTC time-base checks use the exact pre-rebase build and `-accel qtest`, advance
the virtual clock by 10 ms, write TIME_UPDATE bit 31, then read TIME0 before
and after each reset. Both trees report these results:

| Machine | PROCPU | APPCPU | Digital domain | Full system |
| --- | --- | --- | --- | --- |
| ESP32 | `1500 → 1500` | `1500 → 1500` | `1500 → 1500` | `1500 → 0` |
| ESP32-S3 | `1500 → 1500` | `1500 → 1500` | `1500 → 1500` | `1500 → 1500` |

ESP32 full reset still resets the RTC time base; its digital domain excludes
RTC. The old ESP32-S3 selective handler does not reset RTC on full-system reset,
and that behavior is preserved. These are comparisons with the fork, not new
claims about silicon behavior.

Commands and evidence:

```sh
python3 build/fixes/reset-check.py > build/fixes/reset-final.log 2>&1
python3 build/fixes/rtc-running-check.py > build/fixes/rtc-final.log 2>&1
```

The drivers and JSON results are retained beside those logs. The latter driver
references the temporary exact-baseline binary, so recreate that worktree to
rerun it. Unlike the paused UART check, the RTC check omits `-S`: QEMU 9.2's
clock-step implementation does not advance a disabled virtual clock, whereas
11.1's does. An initial paused-clock experiment therefore produced incomparable
0/1500 readings; the corrected check enables both qtest clocks and executes no
guest instructions.

## Build and qtest acceptance

```sh
ninja -C build
build/pyvenv/bin/meson test -C build \
  --suite qtest-riscv32 --suite qtest-xtensa --print-errorlogs
```

The final full incremental build exits 0 with no compiler/linker warnings.
The combined suite command exits 0:

| Suite | Pass | Fail | Skip | `device-introspect-test` | `qom-test` |
| --- | ---: | ---: | ---: | --- | --- |
| `qtest-riscv32` | 10 | 0 | 1 | PASS, 6 subtests | PASS, 11 subtests |
| `qtest-xtensa` | 8 | 0 | 2 | PASS, 6 subtests | PASS, 15 subtests |
| Combined | 18 | 0 | 3 | Both pass | Both pass |

RISC-V skips `cdrom-test`; Xtensa skips `cdrom-test` and `qos-test` because they
have no applicable subtests. Logs: [final build](../../build/fixes/final-build.log),
[suite summary](../../build/fixes/qtest.log), [full suite output](../../build/fixes/qtest-full.log).

The requested suites revealed defects beyond the RGB allocation:

- The ESP32 LEDC model assigned `LEDState.color` the literal `"blue"`. Property
  teardown attempted to free that literal. A single `device-list-properties`
  request for `xtensa.esp32` aborted on the stale reference binary, exact
  pre-rebase binary, and port; the macOS crash stack and QOM trace identify LED
  color release. The coordinator authorized the minimal setter fix.
- Both old SoCs also assigned `CPUState.memory` directly, bypassing the strong
  QOM link's reference acquisition. Once LED teardown worked, the CPU property
  release could finalize a still-parented MemoryRegion. Using the property setter
  resolves that imbalance.
- CPU memory setup in `instance_init` created NULL-owner regions and aliases
  that outlived temporary introspection instances. The test then correctly
  rejected a changed QOM tree. Deferring that setup until `realize` removes the
  construction side effect while retaining the realized map and initialization
  order before ROM mapping and CPU realization.

Intermediate failure logs and focused QOM traces remain in `build/fixes/`.
The final result above supersedes those attempts. Realized machine teardown is
not newly supported by these fixes.

## Exact X4 Pro baseline and X3 regression

The scratch worktree was an unmodified detached checkout of
`f9339150cd030918e72c8470800fe5ef5cc86e1d` (`x4prosim-pre-rebase`).
It was configured with the [host.md](host.md) options and an explicit selection
of the same Homebrew Python, then only the Xtensa target was built:

```sh
git worktree add build/pre-rebase x4prosim-pre-rebase
mkdir -p build/pre-rebase/build
cd build/pre-rebase/build
../configure --python=/opt/homebrew/bin/python3 \
  --target-list=xtensa-softmmu,riscv32-softmmu --enable-gcrypt --enable-sdl \
  --enable-slirp --disable-gnutls --disable-strip --disable-user --disable-capstone \
  --disable-vnc --disable-gtk --disable-docs
ninja qemu-system-xtensa
```

No baseline source patch was needed. Its 1505-step build exits 0, with the old
linker's already-documented duplicate `-liconv` warning. Version:
`9.2.2 (v9.2.2-179-gf9339150cd)`; SHA-256:
`42333909a92a449c0c590228f0b2c686fcead30d4d696258e95b4cbe44cbdc4f`.
The source status was clean before removal. Configuration/build logs and
[metadata](../../build/fixes/pre-rebase-build.json) are retained.

The unchanged 16 MiB S3 input prepared during acceptance was copied for each
run; each received a fresh 512 MiB SD image from `mksd.py`. Both were launched
using their own tree's unchanged `drive.py`, `QEMU_EXTRA='-nic none'`, and
`wait:25 shot:home.png`, as in acceptance. No drift, animation or timing property
was overridden. Exact command arrays are in the run directories. The final port
was retested after all code fixes:

| Comparison | Pixel difference | PNG byte comparison |
| --- | --- | --- |
| Exact pre-rebase X4 Pro Home versus final port | **0 / 384000** | Identical |
| Original acceptance X3 Home versus final port | **0 / 418176** | Identical |

The earlier X4 difference of 613 one-level pixels disappears with the exact
baseline; it was **baseline staleness, not a UC8179 port regression**. The prior
explanation based on possible drift timing is superseded by this direct result.
The X3 PNG SHA-256 remains
`490aa22ae0643baa89afea79abd903a8c10b741612d5e485e92b0b5c040bb684`.

Evidence: [baseline X4 Home](../../build/fixes/pre-rebase/home.png),
[final X4 Home](../../build/fixes/verified-x4/home.png),
[final X3 Home](../../build/fixes/verified-x3/home.png),
[pixel results](../../build/fixes/final-pixels.json). Pixel comparison uses the
same standard-library PNG decoder as acceptance, plus direct byte comparison.
After all baseline-dependent tests, `git worktree remove build/pre-rebase`
succeeded; no scratch worktree remains.

## Remaining review concerns

Each inherited concern from [review.md](review.md) was checked against the
pre-rebase source. None below is a port regression; leave it for a separate task
as requested. The limited RGB construction fix above is the explicit exception.

| Concern | Classification and disposition |
| --- | --- |
| `hw/misc/esp32s3_wifi.c`: guest TX length versus fixed stack frame | **Pre-existing; left.** Both trees use the same unchecked descriptor length and copy; no new bounds behavior was introduced. |
| `hw/char/esp32_uart.c`: RX timeout timer/latch survives reset | **Pre-existing; left.** Both reset paths cancel the throttle timer but omit RX-timeout cancellation and latch clearing. |
| `hw/net/can/esp32_twai.c`: private IRQ/CAN-client teardown and error unwind | **Pre-existing; left.** The allocation/connection and missing lifetime cleanup are inherited unchanged. |
| `hw/dma/esp_gdma.c`: private AddressSpace teardown | **Pre-existing; left.** The old and ported realization paths both initialize without matching destruction. |
| `hw/misc/esp32s3_cache.c`: private AddressSpace teardown | **Pre-existing; left.** The port retains the same lifetime assumptions and missing cleanup. |
| `hw/display/esp_rgb.c`: private AddressSpace teardown | **Pre-existing; partly addressed as requested.** Temporary/unrealized RGB no longer allocates VRAM/address spaces; teardown for a realized device remains outside this minimal fix. |
| Retained diagnostics, UART TODO/model limitations, raw evidence trailing whitespace | **Pre-existing; left.** The review identifies unchanged old-fork code/data, not incomplete port changes. |

No inherited Wi-Fi length exploit, CAN teardown stress test, or UART timeout
stress test was run, and this report does not claim those concerns are resolved.
