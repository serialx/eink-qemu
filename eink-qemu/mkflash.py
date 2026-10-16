#!/usr/bin/env python3
"""Compose emulator flash from a release app, merged image, build dir, or dump.

A release app gets the bundled bootloader and a partition table: Arduino-ESP32's
16 MB default (boot/partitions.csv), the Picco's stock layout with --machine
picco, or the firmware's own with --partitions (CSV or binary). The app goes
where `idf.py flash` puts it: the factory partition, else ota_0. The only
firmware-specific step is CrossPoint's (profiles/crosspoint.json): its X3 build
expects a cached device type in NVS, written only into CrossPoint images.

Only Python's standard library is needed. App bytes are never modified.
"""
import argparse
import csv
import hashlib
import json
from pathlib import Path
import struct
import sys
import zlib

if __package__:
    from . import mknvs
else:
    import mknvs

FLASH_SIZE = 0x1000000
TABLE_OFFSET = 0x8000
TABLE_SIZE = 0xC00
HERE = Path(__file__).resolve().parent
BOOT_DIR = HERE / "boot"
CROSSPOINT = json.loads((HERE / "profiles" / "crosspoint.json").read_text())
CHIPS = {5: ("x3", "esp32c3"), 9: ("x4pro", "esp32s3")}
ENTRY = struct.Struct("<2sBBII16sI")
APP_SUBTYPES = {"factory": 0, "test": 0x20, **{f"ota_{n}": 0x10 + n for n in range(16)}}
DATA_SUBTYPES = {"ota": 0, "phy": 1, "nvs": 2, "coredump": 3, "nvs_keys": 4, "efuse": 5,
                 "undefined": 6, "esphttpd": 0x80, "fat": 0x81, "spiffs": 0x82, "littlefs": 0x83}
FLAGS = {"encrypted": 1, "readonly": 2}


def detect_chip(data, machine=None):
    if len(data) < 24 or data[0] != 0xE9:
        raise ValueError("expected an ESP image header (0xE9 magic, at least 24 bytes)")
    chip = struct.unpack_from("<H", data, 12)[0]
    if chip not in CHIPS:
        raise ValueError(f"unsupported ESP chip ID {chip}; expected 5 (C3) or 9 (S3)")
    if machine and {"x3": 5, "x4": 5, "x4pro": 9, "picco": 9}.get(machine) != chip:
        raise ValueError(f"--machine {machine} conflicts with {CHIPS[chip][1]} image")
    return chip


def _number(text):
    """A CSV number: decimal or 0x hex, with an optional K or M suffix."""
    scale = {"k": 1024, "m": 1024 * 1024}.get(text[-1:].lower(), 1)
    digits = text[:-1] if scale != 1 else text
    try:
        return int(digits, 0) * scale
    except ValueError:
        raise ValueError(f"bad number {text!r} in partition CSV") from None


def partition_table(path=BOOT_DIR / "partitions.csv", text=None):
    """Encode a partition CSV in ESP-IDF's binary format, as gen_esp32part.py
    reads it: named or numeric types and subtypes, K/M sizes, empty offsets
    placed after the previous partition (apps on 64 KiB), flags."""
    if text is None:
        text = Path(path).read_text()
    result = bytearray()
    following = TABLE_OFFSET + 0x1000
    for row in csv.reader(line for line in text.splitlines() if not line.lstrip().startswith("#")):
        if not row or not "".join(row).strip():
            continue
        row = [s.strip() for s in row] + ([""] if len(row) == 5 else [])
        if len(row) != 6:
            raise ValueError(f"bad partition CSV line: {','.join(row)}")
        name, kind, subtype, offset, size, flags = row
        kind = {"app": 0, "data": 1}.get(kind) if kind in ("app", "data") else _number(kind)
        names = APP_SUBTYPES if kind == 0 else DATA_SUBTYPES if kind == 1 else {}
        subtype = names[subtype] if subtype in names else _number(subtype)
        if not 0 <= kind <= 0xFE or not 0 <= subtype <= 0xFF:
            raise ValueError("partition type or subtype out of range")
        size = _number(size)
        if offset:
            offset = _number(offset)
        else:
            align = 0x10000 if kind == 0 else 0x1000
            offset = -(-following // align) * align
        following = offset + size
        bits = 0
        for flag in filter(None, (f.strip() for f in flags.split(":"))):
            if flag not in FLAGS:
                raise ValueError(f"unknown partition flag {flag}")
            bits |= FLAGS[flag]
        label = name.encode("utf-8")
        if len(label) > 16:
            raise ValueError("partition label exceeds 16 bytes")
        result.extend(ENTRY.pack(b"\xaa\x50", kind, subtype, offset, size, label, bits))
    result.extend(b"\xeb\xeb" + b"\xff" * 14 + hashlib.md5(result).digest())
    if len(result) > TABLE_SIZE:
        raise ValueError("partition table exceeds 0xC00 bytes")
    table = bytes(result).ljust(TABLE_SIZE, b"\xff")
    read_partitions(table)
    return table


def read_partition_file(path):
    """A partition table file: a CSV or ESP-IDF's binary table."""
    data = Path(path).read_bytes()
    if data[:2] == b"\xaa\x50":
        table = data[:TABLE_SIZE].ljust(TABLE_SIZE, b"\xff")
    else:
        table = partition_table(text=data.decode("utf-8"))
    read_partitions(table)
    return table


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


def app_partition(entries, picco=False):
    """Where `idf.py flash` puts the app: factory, else ota_0, else the first
    app. The Picco's stock layout keeps factory for recovery: ota_0."""
    def find(subtype):
        return next((p for p in entries if p[0] == 0 and p[1] == subtype), None)
    if picco:
        return find(0x10)
    return find(0) or find(0x10) or next((p for p in entries if p[0] == 0), None)


def is_crosspoint(app):
    """Whether an app holds CrossPoint's NVS strings (NUL-terminated)."""
    return all(s.encode() + b"\0" in app for s in CROSSPOINT["detect"]["image_strings"])


def compose(app, boot, table, machine=None):
    chip = detect_chip(app, machine)
    if detect_chip(boot) != chip:
        raise ValueError("bootloader and app chips differ")
    if len(boot) > TABLE_OFFSET:
        raise ValueError("bootloader overlaps partition table")
    entries = read_partitions(table)
    target = app_partition(entries, machine == "picco")
    if target is None:
        raise ValueError("the partition table has no " + ("ota_0 app" if machine == "picco" else "app") + " partition")
    _, subtype, app_offset, app_size = target
    if len(app) > app_size:
        raise ValueError(f"app ({len(app)} bytes) exceeds its partition ({app_size:#x} bytes at "
                         f"{app_offset:#x}); give the firmware's own table with --partitions")
    flash = bytearray(b"\xff" * FLASH_SIZE)
    flash[:len(boot)] = boot
    flash[TABLE_OFFSET:TABLE_OFFSET + len(table)] = table
    flash[app_offset:app_offset + len(app)] = app
    if 0x10 <= subtype < 0x20 and any(p[0] == 0 and p[1] == 0 for p in entries):
        # Boot the OTA slot, not the factory app: an ESP-IDF select entry with
        # sequence n+1, erased label, VALID state and the sequence CRC.
        ota = next((p for p in entries if p[0] == 1 and p[1] == 0), None)
        if ota is None:
            raise ValueError("the app goes to an OTA slot beside a factory app, but there is no OTA data partition")
        sequence = subtype - 0x10 + 1
        struct.pack_into("<I", flash, ota[2], sequence)
        struct.pack_into("<II", flash, ota[2] + 24, 2, zlib.crc32(struct.pack("<I", sequence), 0xFFFFFFFF))
    seed = CROSSPOINT["nvs_seed"]
    board = machine or CHIPS[chip][0]
    nvs = next((p for p in entries if p[0] == 1 and p[1] == 2), None)
    if board in seed["machines"] and is_crosspoint(app) and nvs:
        flash[nvs[2]:nvs[2] + nvs[3]] = mknvs.image(nvs[3], seed["namespace"], seed["u8"])
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


def make_flash(source, machine=None, partitions=None):
    source = Path(source)
    table = read_partition_file(partitions) if partitions else None
    if source.is_dir():
        app, boot, own = build_files(source)
        return compose(app, boot, table or own, machine=machine)
    data = source.read_bytes()
    whole = len(data) == FLASH_SIZE or data[TABLE_OFFSET:TABLE_OFFSET + 2] == b'\xaa\x50'
    if whole and table:
        raise ValueError("--partitions is for an app .bin or a build directory; "
                         "a merged image or flash dump has its own table")
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
    if table is None:
        table = partition_table(BOOT_DIR / ("partitions-picco.csv" if machine == "picco" else "partitions.csv"))
    return compose(data, boot, table, machine)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path, help="app or merged .bin, build directory, or 16 MiB image")
    parser.add_argument("output", type=Path)
    parser.add_argument("--machine", choices=("x3", "x4", "x4pro", "picco"),
                        help="select the board's app layout and check the detected chip")
    parser.add_argument("--partitions", type=Path, metavar="CSV|BIN",
                        help="partition table for an app or build directory (default: "
                             "Arduino-ESP32's 16 MB table; the Picco's stock layout for picco)")
    args = parser.parse_args()
    try:
        image = make_flash(args.source, args.machine, args.partitions)
        args.output.write_bytes(image)
    except (OSError, ValueError) as exc:
        parser.exit(1, f"mkflash: {exc}\n")
    print(f"wrote {args.output} ({len(image)} bytes)")


if __name__ == "__main__":
    main()
