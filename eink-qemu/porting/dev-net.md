# dev-net port to QEMU 11.1

The network layer is ported from `x4prosim-pre-rebase` onto the dispatch base
`8980d7be81`. All nine owned C sources compile with no warnings using the
prescribed two-target configuration. Both final links stop only on
`esp32_wifi_channel`, whose definition belongs to dev-misc's
`hw/misc/esp32s3_ana.c`.

The coordinator explicitly changed this wave's acceptance to warning-free
object compilation with expected cross-layer link dependencies documented
(message `msg_a10256f3a4cb`). Full linking, runtime NIC enumeration and firmware
network tests remain integration checks; they are not claimed as passed here.
No cross-layer headers, sources, link stubs or temporary configuration changes
were needed.

## Per-file decisions

| File | Port decision |
| --- | --- |
| `hw/net/can/esp32_twai.c` | Use the moved qdev property header and const class data; remove the empty sentinel-only property array. Retain SJA1000 register mapping, CAN bus connection, reset hold phase and migration layout. |
| `hw/net/can/esp32_twai.h` | Use `hw/core/sysbus.h` and `hw/core/irq.h`; retain device and CAN state layouts. |
| `hw/net/can/esp32c3_twai.c` | Const class data; retain parent realize/reset chaining and PeliCAN defaults. |
| `hw/net/can/esp32c3_twai.h` | Preserve subclass state and parent callback declarations. |
| `hw/net/can/esp32s3_twai.c` | Const class data; retain parent realize/reset chaining and PeliCAN defaults. |
| `hw/net/can/esp32s3_twai.h` | Preserve subclass state and parent callback declarations. |
| `hw/net/can/meson.build` | Build base TWAI for ESP32, C3 and S3; build chip variants under their existing machine configuration symbols. |
| `hw/net/Kconfig` | Default `CAN_SJA1000` on for the three TWAI machine symbols, bringing in its existing `CAN_BUS` dependency without machine edits. |
| `hw/net/opencores_eth.c` | Apply only fork MAC initialization/default address and DP83848C PHYSTS changes onto the 11.1 file, retaining upstream MMIO access-size constraints, `physical_memory_*`, const properties and legacy reset registration. PHYSTS is read-only, tracks link changes, and out-of-range MII reads return zero. |
| `hw/misc/esp32_wifi_ap.c` | Move IRQ include; retain the six-argument `qemu_new_nic` call and named `NetClientInfo` callbacks, which remain compatible. Preserve `PICSimLabWifi`, MAC/AP state, timers and Ethernet/802.11 conversion. |
| `hw/misc/esp32_wlan.h` | Preserve packet definitions and external channel declaration. |
| `hw/misc/esp32_wlan_packet.c` | Include `qemu/timer.h` directly; retain packet construction and clock choices. |
| `hw/misc/esp32_wlan_packet.h` | Preserve packet helper declarations. |
| `hw/misc/esp32s3_wifi.c` | Use moved IRQ/sysbus/qdev/address-space headers and explicit module/timer headers, const class data and a const sentinel-free NIC property array. Set the existing reset-enter callback directly on `ResettableClass`, dropping an unused saved-parent structure. Preserve the QOM type name `esp32_wifi`, DMA/register behavior and RX packet format. |
| `hw/misc/esp32_phya.c` | Remove obsolete umbrella include; use core sysbus and explicit module headers. Preserve RF packet-status offsets and state. |
| `hw/misc/esp32_fe.c` | Remove obsolete umbrella include; use core sysbus and explicit module headers. Preserve RF register behavior. |
| `include/hw/misc/esp32_wifi.h` | Use core registerfields/sysbus headers; remove unused hw/sysemu umbrella includes. Preserve `TYPE_ESP32_WIFI`, state, descriptors and register offsets. |
| `include/hw/misc/esp32s3_wifi.h` | Use core registerfields/sysbus headers; remove unused umbrella includes. Preserve chip register definitions and original compatibility macro. |
| `include/hw/misc/esp32_phya.h` | Use core sysbus and remove unused umbrella include; preserve device layout and packet-status API. |
| `include/hw/misc/esp32_fe.h` | Use core sysbus and remove unused umbrella include; preserve device layout. |
| `hw/misc/meson.build` | Add only a clearly marked BEGIN/END dev-net block with the five shared Wi-Fi/RF C sources under C3 and S3 gates. Merge this block with dev-misc's independent additions. |

The fork added no matching `include/hw/net` headers: TWAI headers live next to
their sources in `hw/net/can`. QEMU 11.1 CAN client methods and SJA1000 helper
signatures still match the fork; no compatibility shim or CAN-core change is
needed. The added NetClientInfo offload/tunnel hooks do not apply to this NIC.
`DEFINE_NIC_PROPERTIES` still exposes `mac` and `netdev` with the same types.
Trailing whitespace and missing final newlines in imported files were cleaned.

## Build evidence

Configured on macOS with the command in `porting/README.md`:

```sh
mkdir -p build
cd build
../configure --target-list=xtensa-softmmu,riscv32-softmmu --enable-gcrypt --enable-sdl \
  --enable-slirp --disable-gnutls --disable-strip --disable-user --disable-capstone \
  --disable-vnc --disable-gtk --disable-docs
```

No extra flags were required. As already documented in `host.md`, initial
sandboxed Python dependency resolution failed; rerunning with network access
completed configuration. Logs remain in ignored `build/configure.log` and
`build/configure-network.log`.

After fixing the packet helper's missing direct timer include, force-recompiled
all nine owned objects. The exact object command, run from the repository root:

```sh
ninja -C build \
  libsystem.a.p/hw_misc_esp32_fe.c.o \
  libsystem.a.p/hw_misc_esp32_phya.c.o \
  libsystem.a.p/hw_misc_esp32s3_wifi.c.o \
  libsystem.a.p/hw_misc_esp32_wifi_ap.c.o \
  libsystem.a.p/hw_misc_esp32_wlan_packet.c.o \
  libsystem.a.p/hw_net_can_esp32_twai.c.o \
  libsystem.a.p/hw_net_can_esp32c3_twai.c.o \
  libsystem.a.p/hw_net_can_esp32s3_twai.c.o \
  libsystem.a.p/hw_net_opencores_eth.c.o
```

Output (`build/dev-net-objects.log`; exit 0, no warnings):

```text
ninja: Entering directory `build'
[1/10] Generating qemu-version.h with a custom command (wrapped by meson to capture output)
[2/10] Compiling C object libsystem.a.p/hw_misc_esp32_phya.c.o
[3/10] Compiling C object libsystem.a.p/hw_misc_esp32_fe.c.o
[4/10] Compiling C object libsystem.a.p/hw_net_can_esp32_twai.c.o
[5/10] Compiling C object libsystem.a.p/hw_net_can_esp32c3_twai.c.o
[6/10] Compiling C object libsystem.a.p/hw_net_can_esp32s3_twai.c.o
[7/10] Compiling C object libsystem.a.p/hw_misc_esp32s3_wifi.c.o
[8/10] Compiling C object libsystem.a.p/hw_misc_esp32_wifi_ap.c.o
[9/10] Compiling C object libsystem.a.p/hw_misc_esp32_wlan_packet.c.o
[10/10] Compiling C object libsystem.a.p/hw_net_opencores_eth.c.o
```

The shared objects above are used by both configured targets. Generated config
headers enable the applicable ESP machine symbols, `CAN_SJA1000`, and `CAN_BUS`.
The full initial build and retry logs are `build/dev-net-build.log` and
`build/dev-net-build-retry.log`. A final attempt allowed both link jobs to run:

```text
$ ninja -C build -k 0 qemu-system-xtensa qemu-system-riscv32
[1/8] Generating qemu-version.h with a custom command (wrapped by meson to capture output)
[2/5] Linking target qemu-system-riscv32-unsigned
FAILED: [code=1] qemu-system-riscv32-unsigned
Undefined symbols for architecture arm64:
  "_esp32_wifi_channel", referenced from:
      _Esp32_sendFrame in hw_misc_esp32s3_wifi.c.o
      _Esp32_WLAN_reset_ap in hw_misc_esp32_wifi_ap.c.o
      _Esp32_WLAN_setup_ap in hw_misc_esp32_wifi_ap.c.o
      _Esp32_WLAN_beacon_timer in hw_misc_esp32_wifi_ap.c.o
      _Esp32_WLAN_handle_frame in hw_misc_esp32_wifi_ap.c.o
      _Esp32_WLAN_handle_frame in hw_misc_esp32_wifi_ap.c.o
      _Esp32_WLAN_handle_frame in hw_misc_esp32_wifi_ap.c.o
      ...
ld: symbol(s) not found for architecture arm64
clang: error: linker command failed with exit code 1 (use -v to see invocation)
[3/5] Linking target qemu-system-xtensa-unsigned
FAILED: [code=1] qemu-system-xtensa-unsigned
Undefined symbols for architecture arm64:
  "_esp32_wifi_channel", referenced from:
      _Esp32_sendFrame in hw_misc_esp32s3_wifi.c.o
      _Esp32_WLAN_reset_ap in hw_misc_esp32_wifi_ap.c.o
      _Esp32_WLAN_setup_ap in hw_misc_esp32_wifi_ap.c.o
      _Esp32_WLAN_beacon_timer in hw_misc_esp32_wifi_ap.c.o
      _Esp32_WLAN_handle_frame in hw_misc_esp32_wifi_ap.c.o
      _Esp32_WLAN_handle_frame in hw_misc_esp32_wifi_ap.c.o
      _Esp32_WLAN_handle_frame in hw_misc_esp32_wifi_ap.c.o
      ...
ld: symbol(s) not found for architecture arm64
clang: error: linker command failed with exit code 1 (use -v to see invocation)
ninja: build stopped: cannot make progress due to previous errors.
```

Exit status: 1. Only the long linker command lines are omitted above. Both
links have the same single undefined global, not a CAN/NIC API mismatch.

Requested NIC listing attempt (exit 127 because no binary was linked):

```text
$ ./build/qemu-system-riscv32 -nic model=help
zsh: no such file or directory: ./build/qemu-system-riscv32
```

## Integration follow-up

Merge dev-misc's `esp32s3_ana.c`, which supplies `int esp32_wifi_channel = 0`,
and its Meson entries. Keep the dev-net Wi-Fi block once when resolving
`hw/misc/meson.build`; then rerun the full Ninja build and NIC listing with an
ESP machine. QEMU 11.1 filters `-nic model=help` by the selected machine's
supported NICs, so the machine layer must retain its `esp32_wifi` NIC
configuration call for the requested `-nic user,model=esp32_wifi` interface.
No machine sources were changed here.

The dispatch base already contains core, CPU and scripts layers. Therefore
`git diff --stat v11.1.2..HEAD` includes those inherited changes; the ownership
check for this commit uses `git diff --stat 8980d7be81..HEAD` instead.

Original Git authors for the ported model paths are Yuan Yu
`<yuanyu@espressif.com>`, Ivan Grokhotkov `<ivan@espressif.com>`, and Claude
`<noreply@anthropic.com>`. Original source header credits are retained.
