#!/bin/sh
# Generate the files the app bundles: boot assets for composing release apps
# (served to the WebView from public/boot), and the turbo overrides and a
# compressed blank 64 MiB SD card (embedded in the Rust binary).
# Needs Python 3, like the rest of eink-qemu's tooling.
set -eu
desktop=$(cd "$(dirname "$0")/.." && pwd)
root=$(cd "$desktop/../.." && pwd)
boot=$desktop/public/boot
res=$desktop/src-tauri/resources
mkdir -p "$boot" "$res"
cp "$root"/eink-qemu/boot/bootloader-*.bin "$boot/"
cp "$root/eink-qemu/boot/LICENSE" "$boot/bootloader.LICENSE.txt"
cp "$root/eink-qemu/boot/LICENSE.partitions" "$boot/partitions.LICENSE.txt"
python3 - "$root" "$boot" "$res" <<'PY'
from pathlib import Path
from importlib import import_module
import json, subprocess, sys
root, boot, res = (Path(arg) for arg in sys.argv[1:])
sys.path.insert(0, str(root))
mkflash = import_module('eink-qemu.mkflash')
mknvs = import_module('eink-qemu.mknvs')
(boot / 'partitions.bin').write_bytes(mkflash.partition_table())
(boot / 'partitions-picco.bin').write_bytes(mkflash.partition_table(mkflash.BOOT_DIR / 'partitions-picco.csv'))
(boot / 'nvs-x3.bin').write_bytes(mknvs.image(0x5000, 'cphw', {'dev_det': 2}))
turbo = {machine: subprocess.check_output(['sh', str(root / 'eink-qemu/turbo-args.sh'), machine], text=True).splitlines()
         for machine in ('x3', 'x4', 'x4pro', 'picco')}
text = json.dumps(turbo, indent=2) + '\n'
target = res / 'turbo-args.json'
if not target.exists() or target.read_text() != text:
    target.write_text(text)
PY
if [ ! -f "$res/blank-sd.img.gz" ]; then
    # The browser's compact card; stock X3 timing comparisons need 32 KiB clusters.
    tmp=$(mktemp -d)
    python3 "$root/eink-qemu/mksd.py" "$tmp/sd.img" 64 --cluster-size 512 >/dev/null
    gzip -9 -n -c "$tmp/sd.img" > "$res/blank-sd.img.gz"
    rm -rf "$tmp"
fi
