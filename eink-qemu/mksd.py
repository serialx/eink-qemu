#!/usr/bin/env python3
"""Make a reproducible MBR/FAT32 SD image with stock Xteink X3 32 KiB clusters."""
import argparse
import os
from pathlib import Path
import struct
import subprocess


def copy_tree(source, destination, device, env):
    # mcopy -s follows host directory order, which changes FAT allocation and
    # short-name aliases. Walk every directory in a stable order instead.
    for path in sorted(source.iterdir()):
        target = f'{destination}/{path.name}'
        if path.is_dir():
            subprocess.run(['mmd', '-i', device, target], check=True, env=env)
            copy_tree(path, target, device, env)
        else:
            subprocess.run(['mcopy', '-i', device, str(path), target],
                           check=True, env=env)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('image')
    parser.add_argument('size_mb', nargs='?', type=int, default=2050,
                        help='image size in MiB (default: 2050)')
    parser.add_argument('dir_to_copy', nargs='?', type=Path)
    parser.add_argument('--cluster-size', type=int, default=32768,
                        choices=(512, 1024, 2048, 4096, 8192, 16384, 32768),
                        help='bytes per cluster (default: 32768, stock X3 card)')
    args = parser.parse_args()
    sectors_per_cluster = args.cluster_size // 512
    # Leave room for at least 65525 data clusters, both FATs and the 1 MiB MBR gap.
    minimum_mb = 32 * sectors_per_cluster + 2
    if args.size_mb < minimum_mb:
        parser.error(f'{args.cluster_size}-byte FAT32 clusters require an image of at least '
                     f'{minimum_mb} MiB; increase size_mb or explicitly choose --cluster-size')

    total = args.size_mb * 2048
    start = 2048
    with open(args.image, 'wb') as image:
        image.truncate(total * 512)
        entry = struct.pack('<B3sB3sII', 0, b'\xfe\xff\xff', 0x0C,
                            b'\xfe\xff\xff', start, total - start)
        image.seek(446)
        image.write(entry)
        image.seek(510)
        image.write(b'\x55\xaa')
    # FAT timestamps are local time. Pin both the clock and timezone for
    # dosfstools/mtools, including directory creation and access timestamps.
    # 1980-01-01 is the earliest date FAT can represent; source files stay intact.
    env = dict(os.environ, SOURCE_DATE_EPOCH='315532800', TZ='UTC')
    subprocess.run(['mkfs.vfat', '-F', '32', '-s', str(sectors_per_cluster),
                    '-i', '45494e4b',
                    '--offset', str(start), args.image, str((total - start) // 2)],
                   check=True, stdout=subprocess.DEVNULL, env=env)
    if args.dir_to_copy:
        copy_tree(args.dir_to_copy, '::', f'{args.image}@@{start * 512}', env)


if __name__ == '__main__':
    main()
