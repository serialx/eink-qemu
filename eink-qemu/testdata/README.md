# X4 Pro device test data

Real logs and battery data from an Xteink X4 Pro (UC8179 panel) running CrossDink.
The serial number and LAN address are scrubbed.

| File | What it is |
| --- | --- |
| `device/boot-reader-1002p.txt` | Firmware log (PSRAM ring dump) from the device: deep-sleep wake, boot, Home, Library, opening and reading a book. Firmware `1.6.0-test-combined-dink-1002p`, env `x4-pro-debug`. |
| `device/panel-otp.txt` | The panel's OTP as the firmware read it on the device. `hw/display/uc8179.c` serves the same bytes. |
| `sdcard/debug/logs/battery.csv` | Battery log from the device's SD card (`/debug/logs/battery.csv`), 2026-09-30 to 10-03: one row per event (boot, wake, sleep, Wi-Fi, light) with %, mV, charging, temperature. |
| `sdcard/debug/logs/battery.sum` | The firmware's cached parse of that CSV (binary, `src/util/BatteryLogSum.cpp`). |

## 1. Compare a simulated boot with the device

The X4 Pro defaults to UC8279 revision `0x68`. Select the legacy UC8179
profile to compare with this device log:

```sh
X4PANEL=uc8179 eink-qemu/drive.py flash.bin sd.img sim.log wait:30
grep -h "\[PANEL\] VER=\|\[PANEL\] MTP\|OTP VCOM" eink-qemu/testdata/device/boot-reader-1002p.txt sim.log
```

Expected in both: `VER=00 00 01 FF FF FLG=13`, `productId=580000`, the same
`MTP[0x000..0x02F]` bytes. (The device line says "from NVS cache" because it
had read the OTP on an earlier boot; the sim reads it fresh each time.)

Other lines to compare (grep the tag in both logs):
- `[ACT] Entering activity:` order of screens.
- `Wait complete: 8179_DRF (N ms)` panel busy time. Device: about 1492 ms full
  refresh, 545-560 ms fast. The legacy profile defaults to `busy-ms=0`,
  deriving BUSY from waveform frames (30 frames each way at 25 ms gives
  1500 ms for a full OTP refresh). A nonzero `busy-ms` overrides every
  refresh with a fixed duration.
- `[SYS] heap free=... maxAlloc=...` internal heap after boot.

## 2. Boot the sim with the device's battery history

```sh
rm -f sd.img
eink-qemu/mksd.py sd.img 2050 eink-qemu/testdata/sdcard   # add your books folder the same way
eink-qemu/run.sh flash.bin sd.img
```

The image uses 32 KiB clusters, matching the measured stock X3 card. See
[SD image geometry](../../README.md#sd-image-geometry) for timing comparisons
and explicit compact-image overrides.

The firmware finds `/debug/logs/battery.csv` and appends to it (log line
`[BAT] Flushed N bytes to /debug/logs/battery.csv`). Open Goodies > Battery &
Stats to see the history. To test the battery parser against the raw data:

```sh
python3 -c "import csv; r=list(csv.DictReader(open('eink-qemu/testdata/sdcard/debug/logs/battery.csv'))); print(len(r), r[0]['pct'], r[-1]['pct'])"
```

## 3. Estimated power (reference)

The sim does not model current. These are not measurements: the firmware
estimated them from the fuel gauge's % drop over time in each state. They
were first computed with a wrong 2000 mAh capacity and are rescaled here to
the real 1100 mAh pack (x 0.55). Treat them as rough.

| State | mA (est.) |
| --- | --- |
| idle (light sleep, screen static) | ~7 |
| main loop running | ~12 |
| Wi-Fi on, power save | ~14 |
| frontlight 50% | ~30 |
| CPU busy | ~57 |
| frontlight 100% | ~76 |
| Wi-Fi awake | ~110 |

Frontlight current (same estimate) is about linear in PWM duty (gamma 1.6554, so 50% = 31.7% duty).

Battery voltage sag under load (read from the gauge, not capacity-dependent): frontlight about −25 mV, Wi-Fi −1.2 mV,
CPU −1.0 mV, panel refresh +6 mV.

## Adding more

Copy new device logs into `device/` and scrub them first: replace the serial
(`serial=` in the dump header) and LAN IPs, and check that no Wi-Fi SSIDs,
passwords, remote PINs, OPDS/KOSync credentials or API tokens are left
(`grep -niE "ssid|pass|pin=|token|opds|kosync|http"`).
