# Boot assets

`mkflash.py` (and the CLI, the desktop app and the browser page) use these
assets to compose release app `.bin`s into 16 MiB emulator flash images. They
were taken from CrossPoint's build, which uses the stock Arduino-ESP32
bootloaders and partition table, so they suit any app built for that layout.
No firmware app is included. Composition needs
only Python 3; PlatformIO and esptool are needed only to regenerate bootloaders.

## Bootloaders

Both binaries are ESP-IDF second-stage bootloaders, licensed Apache-2.0
([license text](LICENSE)), distributed in pioarduino's packages:

- Platform `platform-espressif32` 55.03.311.
- `framework-arduinoespressif32` 3.3.11 (Arduino package metadata says
  LGPL-2.1-or-later; the ESP-IDF bootloaders are Apache-2.0).
- `framework-arduinoespressif32-libs` 5.5.5+sha.b774170ff46, from
  `espressif/esp32-arduino-lib-builder`.
- Conversion tool: esptool 5.3.0.

ELFs, relative to `~/.platformio/packages/framework-arduinoespressif32-libs/`:

| Output | Input ELF | Bytes | Output SHA256 |
| --- | --- | ---: | --- |
| bootloader-esp32c3.bin | esp32c3/bin/bootloader_dio_80m.elf | 18688 | `46893df6619f0c7d7689fa309e76551e964e3d83d68c38ccb3e52ed8c3537226` |
| bootloader-esp32s3.bin | esp32s3/bin/bootloader_dio_80m.elf | 18720 | `15f88ae11793eddc5b7e1f770fba54b10a6f59809c14d7c51cfbf0db8d2fa3ca` |

Input ELF SHA256:

```text
C3 1304c8822867eae5c177ad810d6672f9e7ae4108251d3a9f0ee89e412041c1fb
S3 7695a6106ab080db7162b521fad8267f1c7dc2ce10ed4a30b610eb961f94341a
```

Exact conversion commands, from the repository root:

```sh
python3 -m esptool --chip esp32c3 elf2image --flash-mode dio --flash-freq 80m --flash-size 16MB -o eink-qemu/boot/bootloader-esp32c3.bin ~/.platformio/packages/framework-arduinoespressif32-libs/esp32c3/bin/bootloader_dio_80m.elf
python3 -m esptool --chip esp32s3 elf2image --flash-mode dio --flash-freq 80m --flash-size 16MB -o eink-qemu/boot/bootloader-esp32s3.bin ~/.platformio/packages/framework-arduinoespressif32-libs/esp32s3/bin/bootloader_dio_80m.elf
```

`regen.sh` runs these commands. `PLATFORMIO_PACKAGES_DIR` and `PYTHON` override
its package root and interpreter. Install the versions above to reproduce the
hashes; a newer package can contain different ELFs.

The recipe follows `tools/pioarduino-build.py:get_bootloader_image` and
`generate_bootloader_image`, with mode/frequency helpers in the platform's
`builder/main.py`. CrossPoint's base sets `board_build.flash_mode=dio` and
`board_upload.flash_size=16MB`; both boards default to 80 MHz flash. The X4 Pro
`esp32-s3-devkitc1-n16r8` environment sets `board_build.arduino.memory_type=dio_opi`.
Its octal PSRAM does not select the octal-flash bootloader. The C3 output was
compared byte for byte with CrossPoint's `.pio/build/default/bootloader.bin`.

## Partition table and NVS

`partitions.csv` is copied from `crosspoint-reader/crosspoint-reader` release
1.6.5, commit `93e98bb78702e29868a16a13b80c40e6b36ccdff`, with a provenance
comment added. It is MIT-licensed ([license text](LICENSE.partitions)). All
CrossPoint environments share this table. The generated 3072-byte binary has
SHA256 `bd0f7954aca2ef7d925ee21aaa1f3dc8822d1d6ce5cbbd26a135e5886bfff6ce`,
matching `.pio/build/default/partitions.bin` byte for byte. It is, entry for
entry, Arduino-ESP32's `tools/partitions/default_16MB.csv`; firmware with
another layout passes its own table with `--partitions`.

The composer adds the ESP-IDF MD5 record and erased padding itself. It preserves
the app bytes and leaves OTA metadata erased so the bootloader selects app0.
For a CrossPoint X3 app (one that carries its `cphw` and `dev_det` strings,
per `eink-qemu/profiles/crosspoint.json`), `mknvs.py` seeds `cphw/dev_det=2`
into the NVS partition: this represents an X3 whose hardware type is already
cached and avoids CrossPoint 1.6.5's first-boot Wire lock deadlock. Every other
app, and every S3 app, keeps NVS erased; the official S3 app reaches hardware
and panel initialization without a seed.

Build directories supply their own bootloader and table. The app goes where
`idf.py flash` puts it (the factory partition, else ota_0); its table entry
bounds the app size.
`--machine x3|x4pro` checks the detected chip and rejects a mismatch. A file of
exactly 16 MiB passes through unchanged; with `--machine`, its boot header is
also checked. These helpers target emulator images, not encrypted/secure-boot
provisioning.

Run `python3 -m unittest eink-qemu.tests.test_mkflash`. The optional full-image
comparison uses `CROSSPOINT_RELEASE_APP` and `CROSSPOINT_REFERENCE_FLASH` paths
and skips when either input is absent.

## Picco app images

`--machine picco` selects `partitions-picco.csv`. Its first four records match
Picco build 2920's OTA layout validator: NVS at `0x9000` (48 KiB), OTA metadata
at `0x15000` (8 KiB), `safeboot` at `0x20000` (768 KiB), and OTA app at
`0xE0000` (`0xE70000` bytes). The app is copied unchanged. A valid OTA sequence
selects it instead of the erased recovery slot; recovery firmware is not
included or synthesized.

The firmware mounts LittleFS using the label `spiffs`. The composer provisions
the remaining `0xB0000` bytes at `0xF50000` for it. This is a simulator choice;
the original filesystem partition bounds are not present in the supplied app.
Picco NVS is initially erased, and the bundled ESP32-S3 bootloader is reused.
