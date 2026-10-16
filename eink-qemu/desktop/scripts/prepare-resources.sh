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
# CrossPoint's NVS entry, from its profile, sized for the bundled table's NVS
# partition; firmware.ts writes it only into apps that carry its strings.
rm -f "$boot/nvs-x3.bin"
python3 - "$root" "$boot" <<'PY'
from pathlib import Path
from importlib import import_module
import json, struct, sys
root, boot = (Path(arg) for arg in sys.argv[1:])
sys.path.insert(0, str(root))
mkflash = import_module('eink-qemu.mkflash')
mknvs = import_module('eink-qemu.mknvs')
table = mkflash.partition_table()
(boot / 'partitions.bin').write_bytes(table)
(boot / 'partitions-picco.bin').write_bytes(mkflash.partition_table(mkflash.BOOT_DIR / 'partitions-picco.csv'))
nvs_size = next(size for magic, kind, subtype, _, size in
                (struct.unpack_from('<HBBII', table, at) for at in range(0, len(table), 32))
                if magic == 0x50aa and (kind, subtype) == (1, 2))
seed = json.loads((root / 'eink-qemu/profiles/crosspoint.json').read_text())['nvs_seed']
(boot / 'nvs-crosspoint.bin').write_bytes(mknvs.image(nvs_size, seed['namespace'], seed['u8']))
PY
