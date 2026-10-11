#!/usr/bin/env bash
# Assemble a standalone, dependency-free browser distribution.
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
case "${WASM64_MODE:-32}" in
    32) DEFAULT_BUILD="$ROOT/build-wasm-32limit" ;;
    64) DEFAULT_BUILD="$ROOT/build-wasm" ;;
    *) echo 'WASM64_MODE must be 64 or 32' >&2; exit 1 ;;
esac
BUILD=${WASM_BUILD_DIR:-$DEFAULT_BUILD}
DEST="$BUILD/web-dist"
"$ROOT/x4prosim/wasm/build-browser-net.sh"
ASSETS="${WIFI_ASSETS:-$ROOT/build-wasm-deps/wifi-assets}"
if [ ! -f "$ASSETS/index.json" ]; then
    python3 "$ROOT/x4prosim/wasm/prepare-wifi-assets.py" "$ASSETS"
fi
mkdir -p "$DEST"
python3 "$ROOT/x4prosim/wasm/prepare-wifi-assets.py" "$ASSETS" --copy-to "$DEST/wifi-assets"
cp "$ROOT/build-wasm-deps/browser-net/build/"{browser-net.mjs,browser-net.wasm,lwip.LICENSE.txt,mbedtls.LICENSE.txt} "$DEST/"
# Node resolves the adapter next to its QEMU module.
cp "$ROOT/build-wasm-deps/browser-net/build/"{browser-net.mjs,browser-net.wasm} "$BUILD/"
cp "$ROOT/x4prosim/wasm/web/browser-network.mjs" "$BUILD/"
cp "$ROOT"/x4prosim/wasm/web/* "$DEST/"
for arch in riscv32 xtensa; do
    cp "$BUILD/qemu-system-$arch.js" "$BUILD/qemu-system-$arch.wasm" "$DEST/"
    if [ -f "$BUILD/qemu-system-$arch.worker.js" ]; then
        cp "$BUILD/qemu-system-$arch.worker.js" "$DEST/"
    fi
done
cp "$ROOT/pc-bios/esp32c3-rom.bin" "$ROOT/pc-bios/esp32s3_rev0_rom.bin" "$DEST/"
cp "$ROOT"/x4prosim/boot/bootloader-*.bin "$DEST/"
cp "$ROOT/x4prosim/boot/LICENSE" "$DEST/bootloader.LICENSE.txt"
cp "$ROOT/x4prosim/boot/LICENSE.partitions" "$DEST/partitions.LICENSE.txt"
python3 - "$ROOT" "$DEST" <<'PYBOOT'
from pathlib import Path
import sys
sys.path.insert(0, sys.argv[1])
from x4prosim import mkflash, mknvs
out = Path(sys.argv[2])
provenance = (mkflash.BOOT_DIR / 'README.md').read_text()
(out / 'boot-provenance.md').write_text(provenance.replace('(LICENSE)', '(bootloader.LICENSE.txt)')
                                       .replace('(LICENSE.partitions)', '(partitions.LICENSE.txt)'))
(out / 'partitions.bin').write_bytes(mkflash.partition_table())
(out / 'nvs-x3.bin').write_bytes(mknvs.image(0x5000, 'cphw', {'dev_det': 2}))
PYBOOT
if [ ! -f "$DEST/blank-sd.img" ]; then
    # Compact browser fixture; stock X3 timing comparisons need 32 KiB clusters.
    python3 "$ROOT/x4prosim/mksd.py" "$DEST/blank-sd.img" 64 --cluster-size 512
fi
printf 'Web package: %s\nRun: python3 x4prosim/wasm/serve.py %s\n' "$DEST" "$DEST"
