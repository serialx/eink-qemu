#!/bin/sh
# Regenerate with the package versions recorded in README.md; compose needs neither tool.
set -eu
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
packages=${PLATFORMIO_PACKAGES_DIR:-$HOME/.platformio/packages}
python=${PYTHON:-python3}
for chip in esp32c3 esp32s3; do
    "$python" -m esptool --chip "$chip" elf2image \
        --flash-mode dio --flash-freq 80m --flash-size 16MB \
        -o "$here/bootloader-$chip.bin" \
        "$packages/framework-arduinoespressif32-libs/$chip/bin/bootloader_dio_80m.elf"
done
