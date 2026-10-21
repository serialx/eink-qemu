#!/usr/bin/env python3
"""Inspect a desktop Debian package without installing or executing its payloads."""
from __future__ import annotations
import argparse
import json
from pathlib import Path, PurePosixPath
import shutil
import subprocess
import tarfile
import tempfile

from stage_linux import elf, MACHINES


def unpack(archive: Path, output: Path) -> dict[str, int]:
    modes = {}
    total = 0
    with tarfile.open(archive, 'r:*') as source:
        for entry in source:
            path = PurePosixPath(entry.name)
            if path.is_absolute() or '..' in path.parts or not (entry.isdir() or entry.isfile()):
                raise ValueError(f'Unsafe package member: {entry.name}')
            name = str(path)
            if name == '.' and entry.isdir():
                continue
            if name in modes or entry.uid != 0 or entry.gid != 0 or entry.mode & 0o7022:
                raise ValueError(f'Unsafe or duplicate package member: {entry.name}')
            total += entry.size
            if entry.size > 512 * 1024 * 1024 or total > 2 * 1024 * 1024 * 1024:
                raise ValueError('Package exceeds the payload size limit')
            modes[name] = entry.mode
            destination = output / name
            if entry.isdir():
                destination.mkdir(parents=True, exist_ok=True)
            else:
                destination.parent.mkdir(parents=True, exist_ok=True)
                with source.extractfile(entry) as data, destination.open('xb') as target:
                    shutil.copyfileobj(data, target)
                destination.chmod(0o600)
    return modes


def verify(package: Path, target: str) -> None:
    desktop = Path(__file__).resolve().parents[2]
    expected_arch = {'x86_64-unknown-linux-gnu': 'amd64', 'aarch64-unknown-linux-gnu': 'arm64'}[target]
    architecture = subprocess.check_output(['dpkg-deb', '-f', str(package), 'Architecture'], text=True).strip()
    if architecture != expected_arch:
        raise ValueError('Package architecture does not match the selected target')
    with tempfile.TemporaryDirectory(prefix='eink deb inspection ') as scratch:
        root = Path(scratch)
        modes = {}
        for part, option in [('data', '--fsys-tarfile'), ('control', '--ctrl-tarfile')]:
            archive = root / f'{part}.tar'
            with archive.open('wb') as output:
                subprocess.run(['dpkg-deb', option, str(package)], stdout=output, check=True)
            modes[part] = unpack(archive, root / part)
        data = root / 'data'
        base = data / 'usr/lib/eink-qemu'
        manifest = json.loads((base / 'native-dependencies.json').read_text())
        if manifest['target'] != target or manifest['baseline'] != 'Ubuntu 22.04':
            raise ValueError('Native dependency manifest does not match this package')
        mapping = {name: base / name for name in [
            'qemu/bin/qemu-system-riscv32', 'qemu/bin/qemu-system-xtensa',
            'qemu/plugins/libheaptrace.so', 'bridge/eink-qemu-bridge-helper', 'bridge/eink-bridge-helper']}
        mapping[f'binaries/eink-qemu-{target}'] = data / 'usr/bin/eink-qemu'
        if set(mapping) != set(manifest['files']):
            raise ValueError('Unexpected native manifest payloads')
        for name, path in mapping.items():
            if modes['data'].get(str(path.relative_to(data))) != 0o755:
                raise ValueError(f'Unexpected packaged executable permissions: {name}')
            if elf(path, target, 'readelf') != manifest['files'][name]:
                raise ValueError(f'Packaged native metadata differs from staging: {name}')
        application = data / 'usr/bin/eink-qemu-desktop'
        elf(application, target, 'readelf')
        if modes['data'].get(str(application.relative_to(data))) != 0o755:
            raise ValueError('Desktop permissions are incorrect')
        templates = desktop / 'scripts/bridge/linux'
        for name in ['preinst', 'postinst', 'prerm', 'postrm']:
            if modes['control'].get(name) != 0o755 or (root / 'control' / name).read_bytes() != (templates / 'maintainer.sh').read_bytes():
                raise ValueError(f'Missing or outdated lifecycle hook: {name}')
        for installed, source in {
            'usr/lib/systemd/system/eink-bridge.service': templates / 'eink-bridge.service',
            'usr/share/polkit-1/actions/net.serialx.eink-qemu.bridge.policy': templates / 'net.serialx.eink-qemu.bridge.policy',
            'usr/lib/eink-qemu/qemu/COPYING': desktop.parents[1] / 'COPYING',
            'usr/lib/eink-qemu/bridge/COPYING': desktop.parents[1] / 'COPYING',
            'usr/lib/eink-qemu/qemu/share/esp32c3-rom.bin': desktop.parents[1] / 'pc-bios/esp32c3-rom.bin',
            'usr/lib/eink-qemu/qemu/share/esp32s3_rev0_rom.bin': desktop.parents[1] / 'pc-bios/esp32s3_rev0_rom.bin',
        }.items():
            if (data / installed).read_bytes() != source.read_bytes():
                raise ValueError(f'Missing or outdated package resource: {installed}')
        print(f'{package.name}: payloads, architecture, libc baseline, permissions, and lifecycle hooks verified')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('package', type=Path)
    parser.add_argument('target', choices=MACHINES)
    args = parser.parse_args()
    verify(args.package.resolve(), args.target)
