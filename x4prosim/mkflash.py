#!/usr/bin/env python3
"""Compose emulator flash from a release app, merged image, build dir, or dump.

Only Python's standard library is needed. App bytes are never modified.
"""
import argparse
import csv
import hashlib
from pathlib import Path
import struct
import sys

if __package__:
    from . import mknvs
else:
    import mknvs

FLASH_SIZE = 0x1000000
TABLE_OFFSET = 0x8000
TABLE_SIZE = 0xC00
APP_OFFSET = 0x10000
PICCO_APP_OFFSET = 0xE0000
BOOT_DIR = Path(__file__).resolve().parent / "boot"
CHIPS = {5: ("x3", "esp32c3"), 9: ("x4pro", "esp32s3")}
ENTRY = struct.Struct("<2sBBII16sI")


def detect_chip(data, machine=None):
    if len(data) < 24 or data[0] != 0xE9:
        raise ValueError("expected an ESP image header (0xE9 magic, at least 24 bytes)")
    chip = struct.unpack_from("<H", data, 12)[0]
    if chip not in CHIPS:
        raise ValueError(f"unsupported ESP chip ID {chip}; expected 5 (C3) or 9 (S3)")
    if machine and {"x3": 5, "x4": 5, "x4pro": 9, "picco": 9}.get(machine) != chip:
        raise ValueError(f"--machine {machine} conflicts with {CHIPS[chip][1]} image")
    return chip


def partition_table(path=BOOT_DIR / "partitions.csv"):
    """Encode an explicit-offset CSV in ESP-IDF partition format."""
    types = {"app": 0, "data": 1}
    subtypes = {"factory": 0, "ota_0": 0x10, "ota_1": 0x11, "nvs": 2, "ota": 0,
                "spiffs": 0x82, "coredump": 3}
    result = bytearray()
    with Path(path).open(newline="") as source:
        for row in csv.reader(line for line in source if not line.lstrip().startswith("#")):
            if not row:
                continue
            name, kind, subtype, offset, size, flags = (s.strip() for s in row)
            if flags:
                raise ValueError("partition CSV flags are not supported")
            label = name.encode("utf-8")
            if len(label) > 16:
                raise ValueError("partition label exceeds 16 bytes")
            result.extend(ENTRY.pack(b"\xaa\x50", types[kind], subtypes[subtype],
                                     int(offset, 0), int(size, 0), label, 0))
    result.extend(b"\xeb\xeb" + b"\xff" * 14 + hashlib.md5(result).digest())
    if len(result) > TABLE_SIZE:
        raise ValueError("partition table exceeds 0xC00 bytes")
    return bytes(result).ljust(TABLE_SIZE, b"\xff")


def read_partitions(table):
    if not table or len(table) > 0x1000 or len(table) % ENTRY.size:
        raise ValueError("invalid partition table size")
    entries = []
    for pos in range(0, len(table), ENTRY.size):
        record = table[pos:pos + ENTRY.size]
        if record == b"\xff" * ENTRY.size:
            break
        if record[:2] == b"\xeb\xeb":
            if record[16:] != hashlib.md5(table[:pos]).digest():
                raise ValueError("partition table MD5 mismatch")
            break
        magic, kind, subtype, offset, size, name, flags = ENTRY.unpack(record)
        if magic != b"\xaa\x50" or not size or offset < 0x9000 or offset + size > FLASH_SIZE:
            raise ValueError("invalid partition entry or partition outside 16 MiB flash")
        if any(offset < old[2] + old[3] and old[2] < offset + size for old in entries):
            raise ValueError("overlapping partitions")
        entries.append((kind, subtype, offset, size))
    if not entries:
        raise ValueError("empty partition table")
    return entries


def compose(app, boot, table, machine=None):
    chip = detect_chip(app, machine)
    if detect_chip(boot) != chip:
        raise ValueError("bootloader and app chips differ")
    if len(boot) > TABLE_OFFSET:
        raise ValueError("bootloader overlaps partition table")
    entries = read_partitions(table)
    app_offset = PICCO_APP_OFFSET if machine == "picco" else APP_OFFSET
    apps = [p for p in entries if p[0] == 0 and p[2] == app_offset]
    if len(apps) != 1:
        raise ValueError(f"build must have an app partition at {app_offset:#x}")
    if len(app) > apps[0][3]:
        raise ValueError(f"app exceeds app0 partition size ({apps[0][3]:#x})")
    flash = bytearray(b"\xff" * FLASH_SIZE)
    flash[:len(boot)] = boot
    flash[TABLE_OFFSET:TABLE_OFFSET + len(table)] = table
    flash[app_offset:app_offset + len(app)] = app
    if machine == "picco":
        if apps[0][1] != 0x10 or (1, 0, 0x15000, 0x2000) not in entries:
            raise ValueError("Picco requires ota_0 app and OTA metadata at 0x15000")
        # Select ota_0 explicitly: the factory safeboot slot is intentionally blank.
        # ESP-IDF select entry: sequence 1, erased label, VALID state, sequence CRC.
        struct.pack_into("<I", flash, 0x15000, 1)
        struct.pack_into("<II", flash, 0x15018, 2, 0x4743989A)
    if chip == 5 and machine != "x4":
        # Same cached X3 device detection as mkflash.sh; S3 remains erased.
        if (1, 2, 0x9000, 0x5000) not in entries:
            raise ValueError("C3 seed requires NVS at 0x9000 with size 0x5000")
        flash[0x9000:0xE000] = mknvs.image(0x5000, "cphw", {"dev_det": 2})
    return bytes(flash)


def build_files(directory):
    files = sorted(p for p in directory.glob("*") if p.is_file())
    nested = sorted(p for p in directory.glob("*/*") if p.is_file())

    def unique(paths, description):
        if len(paths) != 1:
            raise ValueError(f"need exactly one {description} in {directory}; found {len(paths)}")
        return paths[0]

    boot = unique([p for p in files + nested if p.name == "bootloader.bin"], "bootloader.bin")
    table = unique([p for p in files + nested if p.match("partition*.bin")], "partition*.bin")
    app = directory / "firmware.bin"
    if not app.is_file():
        app = unique([p for p in files if p.suffix == ".bin" and p != boot
                      and not p.match("partition*.bin")], "app .bin")
    return app.read_bytes(), boot.read_bytes(), table.read_bytes()


def make_flash(source, machine=None):
    source = Path(source)
    if source.is_dir():
        return compose(*build_files(source), machine=machine)
    data = source.read_bytes()
    if len(data) == FLASH_SIZE:
        # Dumps may be encrypted or have no boot header. Preserve every byte.
        if machine:
            detect_chip(data, machine)
        return data
    chip = detect_chip(data, machine)
    if data[TABLE_OFFSET:TABLE_OFFSET + 2] == b'\xaa\x50':
        if len(data) > FLASH_SIZE:
            raise ValueError('merged firmware exceeds 16 MiB flash')
        if len(data) < TABLE_OFFSET + TABLE_SIZE:
            raise ValueError('truncated merged firmware partition table')
        entries = read_partitions(data[TABLE_OFFSET:TABLE_OFFSET + TABLE_SIZE])
        apps = [offset for kind, subtype, offset, size in entries
                if kind == 0 and offset + 24 <= len(data) and data[offset] == 0xE9
                and struct.unpack_from('<H', data, offset + 12)[0] == chip]
        if not apps:
            raise ValueError('merged firmware has no matching application image')
        # Preserve the bundled bootloader, partition layout, and all app/data bytes.
        return data.ljust(FLASH_SIZE, b'\xff')
    boot = (BOOT_DIR / f"bootloader-{CHIPS[chip][1]}.bin").read_bytes()
    table_name = "partitions-picco.csv" if machine == "picco" else "partitions.csv"
    return compose(data, boot, partition_table(BOOT_DIR / table_name), machine)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path, help="app or merged .bin, build directory, or 16 MiB image")
    parser.add_argument("output", type=Path)
    parser.add_argument("--machine", choices=("x3", "x4", "x4pro", "picco"),
                        help="select the board's app layout and check the detected chip")
    args = parser.parse_args()
    try:
        image = make_flash(args.source, args.machine)
        args.output.write_bytes(image)
    except (OSError, ValueError) as exc:
        parser.exit(1, f"mkflash: {exc}\n")
    print(f"wrote {args.output} ({len(image)} bytes)")


if __name__ == "__main__":
    main()
