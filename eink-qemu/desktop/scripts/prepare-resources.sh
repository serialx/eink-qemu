#!/bin/sh
# Generate the boot assets the WebView composes release apps with (served
# from public/boot). The Rust core embeds the turbo overrides itself
# (core/build.rs). Needs Python 3, like the rest of eink-qemu's tooling.
set -eu
desktop=$(cd "$(dirname "$0")/.." && pwd)
root=$(cd "$desktop/../.." && pwd)
boot=$desktop/public/boot
mkdir -p "$boot"
cp "$root"/eink-qemu/boot/bootloader-*.bin "$boot/"
cp "$root/eink-qemu/boot/LICENSE" "$boot/bootloader.LICENSE.txt"
cp "$root/eink-qemu/boot/LICENSE.partitions" "$boot/partitions.LICENSE.txt"
python3 - "$root" "$boot" <<'PY'
from pathlib import Path
from importlib import import_module
import sys
root, boot = (Path(arg) for arg in sys.argv[1:])
sys.path.insert(0, str(root))
mkflash = import_module('eink-qemu.mkflash')
mknvs = import_module('eink-qemu.mknvs')
(boot / 'partitions.bin').write_bytes(mkflash.partition_table())
(boot / 'partitions-picco.bin').write_bytes(mkflash.partition_table(mkflash.BOOT_DIR / 'partitions-picco.csv'))
(boot / 'nvs-x3.bin').write_bytes(mknvs.image(0x5000, 'cphw', {'dev_det': 2}))
PY
