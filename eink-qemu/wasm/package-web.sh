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
"$ROOT/eink-qemu/wasm/build-browser-net.sh"
# Optional static files for the browser's Wi-Fi: CrossPoint Reader's font
# catalog, which its font downloader then fetches from the page itself. The
# page's Wi-Fi works without them (downloads go through the browser's fetch).
# WIFI_ASSETS=none packages none; otherwise a catalog that cannot be prepared
# (offline) is left out with a warning.
ASSETS="${WIFI_ASSETS:-$ROOT/build-wasm-deps/wifi-assets}"
if [ "$ASSETS" != none ] && [ ! -f "$ASSETS/index.json" ]; then
    python3 "$ROOT/eink-qemu/wasm/prepare-wifi-assets.py" "$ASSETS" ||
        echo "package-web.sh: could not prepare the font catalog for browser Wi-Fi; packaging without it" >&2
fi
mkdir -p "$DEST"
if [ "$ASSETS" != none ] && [ -f "$ASSETS/index.json" ]; then
    python3 "$ROOT/eink-qemu/wasm/prepare-wifi-assets.py" "$ASSETS" --copy-to "$DEST/wifi-assets"
else
    rm -rf "$DEST/wifi-assets"
fi
cp "$ROOT/build-wasm-deps/browser-net/build/"{browser-net.mjs,browser-net.wasm,lwip.LICENSE.txt,mbedtls.LICENSE.txt} "$DEST/"
# Node resolves the adapter next to its QEMU module.
cp "$ROOT/build-wasm-deps/browser-net/build/"{browser-net.mjs,browser-net.wasm} "$BUILD/"
cp "$ROOT/eink-qemu/wasm/web/browser-network.mjs" "$BUILD/"
# The page is the front end shared with the desktop app (eink-qemu/desktop),
# built for the browser: relative URLs, the runtime's classic scripts loaded by
# index.html, and the boot assets under boot/ (scripts/prepare-resources.sh).
(cd "$ROOT/eink-qemu/desktop" && pnpm install --frozen-lockfile && pnpm build:browser)
cp -R "$ROOT/eink-qemu/desktop/dist-web/." "$DEST/"
# The runtime iframe and what it loads, the isolation service worker, the
# network adapter's page half, and the notices.
for f in runtime.html runtime.js fat.js sd-ops.js sd-store.js browser-network.mjs \
         coi-serviceworker.js coi-serviceworker.LICENSE.txt fonts.LICENSE.txt README.txt; do
    cp "$ROOT/eink-qemu/wasm/web/$f" "$DEST/"
done
for arch in riscv32 xtensa; do
    cp "$BUILD/qemu-system-$arch.js" "$BUILD/qemu-system-$arch.wasm" "$DEST/"
    if [ -f "$BUILD/qemu-system-$arch.worker.js" ]; then
        cp "$BUILD/qemu-system-$arch.worker.js" "$DEST/"
    fi
done
cp "$ROOT/pc-bios/esp32c3-rom.bin" "$ROOT/pc-bios/esp32s3_rev0_rom.bin" "$DEST/"
# Boot asset provenance, beside the assets the build placed under boot/.
python3 - "$ROOT" "$DEST/boot" <<'PYBOOT'
from pathlib import Path
from importlib import import_module
import sys
sys.path.insert(0, sys.argv[1])
mkflash = import_module('eink-qemu.mkflash')
out = Path(sys.argv[2])
provenance = (mkflash.BOOT_DIR / 'README.md').read_text()
(out / 'boot-provenance.md').write_text(provenance.replace('(LICENSE)', '(bootloader.LICENSE.txt)')
                                       .replace('(LICENSE.partitions)', '(partitions.LICENSE.txt)'))
PYBOOT
if [ ! -f "$DEST/blank-sd.img" ]; then
    # Compact browser fixture; stock X3 timing comparisons need 32 KiB clusters.
    python3 "$ROOT/eink-qemu/mksd.py" "$DEST/blank-sd.img" 64 --cluster-size 512
fi
printf 'Web package: %s\nRun: python3 eink-qemu/wasm/serve.py %s\n' "$DEST" "$DEST"
