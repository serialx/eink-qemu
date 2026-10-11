#!/usr/bin/env python3
"""Write a minimal ESP-IDF NVS partition image holding u8 keys.

usage: mknvs.py out.bin SIZE namespace key=value [key=value...]
Entries go in one NVS page (version 2 format, CRC32 seeded with 0xffffffff like the IDF generator);
the remaining pages are left erased. Used to seed CrossPoint's cached device
type (cphw/dev_det) on an X3 flash image, which the firmware would write
itself on its first boot."""
import struct, sys, zlib

ENTRY = 32
U8 = 0x01


def entry(ns, key, value):
    e = bytearray(b"\xff" * ENTRY)
    e[0] = ns
    e[1] = U8
    e[2] = 1            # span
    e[3] = 0xff         # chunk index: not a blob
    e[8:24] = key.encode().ljust(16, b"\0")
    e[24] = value
    e[4:8] = struct.pack("<I", zlib.crc32(bytes(e[0:4]) + bytes(e[8:32]), 0xffffffff) & 0xffffffff)
    return bytes(e)


def page(entries):
    p = bytearray(b"\xff" * 4096)
    hdr = bytearray(b"\xff" * 32)
    hdr[0:4] = struct.pack("<I", 0xfffffffe)    # ACTIVE
    hdr[4:8] = struct.pack("<I", 0)             # sequence number
    hdr[8] = 0xfe                               # NVS version 2
    hdr[28:32] = struct.pack("<I", zlib.crc32(bytes(hdr[4:28]), 0xffffffff) & 0xffffffff)
    p[0:32] = hdr
    bitmap = bytearray(b"\xff" * 32)
    for i, e in enumerate(entries):
        bitmap[i // 4] &= ~(1 << ((i % 4) * 2))  # state "written" = 0b10
        p[64 + i * ENTRY:64 + (i + 1) * ENTRY] = e
    p[32:64] = bitmap
    return bytes(p)


def image(size, namespace, values):
    """Return the same seeded partition used by the command-line tool."""
    if size < 4096 or size % 4096:
        raise ValueError("NVS size must be a positive multiple of 4096")
    entries = [entry(0, namespace, 1)]
    entries.extend(entry(1, key, value) for key, value in values.items())
    return page(entries) + b"\xff" * (size - 4096)


if __name__ == "__main__":
    out, size, ns = sys.argv[1], int(sys.argv[2], 0), sys.argv[3]
    values = dict(kv.split("=") for kv in sys.argv[4:])
    with open(out, "wb") as f:
        f.write(image(size, ns, {k: int(v, 0) for k, v in values.items()}))
