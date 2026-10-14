"""python3 -m unittest eink-qemu.tests.test_heapview

Walks synthetic ESP-IDF 5.x TLSF heaps; no firmware or QEMU build needed.
"""
from pathlib import Path
import struct
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import heapview  # noqa: E402

HEAD = 0x50000000          # &registered_heaps
NODE = 0x50000100          # heap_t
START, END = 0x3FC90000, 0x3FC91000
CONTROL = 0x3A4            # TLSF control size; the walker must find it


def build_heap(blocks, task_tracking=False):
    """blocks: [(size, free, requested or None)]; returns guest memory {addr: bytes}."""
    region = bytearray(END - START)
    # multi_heap_info: lock, free_bytes, minimum_free_bytes, pool_size, heap_data
    free = sum(size for size, is_free, _ in blocks if is_free)
    struct.pack_into("<5I", region, 0, 0, free, free - 64, END - START - 20, START + 20)
    pool = START + 20 + CONTROL
    b, prev_free = pool - 4, 0
    for size, is_free, req in blocks:
        struct.pack_into("<I", region, b + 4 - START, size | is_free | prev_free << 1)
        if req is not None:
            struct.pack_into("<II", region, b + 8 - START, heapview.HEAD_CANARY, req)
            struct.pack_into("<I", region, b + 16 + req - START, heapview.TAIL_CANARY)
        prev_free, b = is_free, b + size + 4
    struct.pack_into("<I", region, b + 4 - START, prev_free << 1)   # sentinel
    assert END - (b + 8) < 64, "blocks must fill the region"
    node = struct.pack("<9I", 0x1802, 0, 0, START, END, 0xB33FFFFF, 0, START, 0)
    if task_tracking:
        node = struct.pack("<I", 0x3C000000) + node
    return {HEAD: struct.pack("<I", NODE), NODE: node.ljust(40, b"\0"), START: bytes(region)}


class FakeMonitor:
    def __init__(self, mem):
        self.mem, self.commands = mem, []

    def running(self):
        return True

    def cmd(self, line):
        self.commands.append(line)
        return ""

    def read(self, addr, size):
        for base, data in self.mem.items():
            if base <= addr and addr + size <= base + len(data):
                return data[addr - base:addr - base + size]
        raise IOError(f"unmapped 0x{addr:x}")


def fill(blocks):
    """Make the last free block absorb the rest of the region."""
    used = sum(size + 4 for size, _, _ in blocks)
    last = END - (START + 20 + CONTROL - 4) - 8 - used - 4
    return blocks + [(last & ~3, 1, None)]


class HeapviewTest(unittest.TestCase):
    def viewer(self, mem):
        v = heapview.Viewer.__new__(heapview.Viewer)
        v.head, v.elf, v.mon, v.interval = HEAD, "test.elf", FakeMonitor(mem), 1
        v.pools, v.history, v.trace, v.sym, v.sites = {}, [], None, None, None
        return v

    def test_walk(self):
        blocks = fill([(64, 0, 40), (128, 1, None), (32, 0, 16), (16, 0, None)])
        snap = self.viewer(build_heap(blocks)).snapshot()
        h, = snap["heaps"]
        self.assertEqual(h["name"], "DRAM")
        self.assertEqual(h["count"], 5)
        self.assertEqual(h["used"], 64 + 32 + 16)
        self.assertEqual(h["nfree"], 2)
        self.assertEqual(h["largest"], blocks[-1][0])
        self.assertEqual(h["pool"], 20 + CONTROL - 4)
        flat = h["blocks"]
        self.assertEqual(flat[0:4], [20 + CONTROL, 68, heapview.BLOCK_POISONED, 40])
        self.assertEqual(flat[6], heapview.BLOCK_FREE)
        self.assertEqual(flat[12:16], [flat[8] + 36, 20, 0, 0])   # unpoisoned allocation
        self.assertEqual(h["corrupt"], 0)

    def test_corrupt_tail_canary(self):
        mem = build_heap(fill([(64, 0, 40), (64, 0, 8)]))
        region = bytearray(mem[START])
        tail = 20 + CONTROL - 4 + 16 + 40
        region[tail] ^= 0xFF
        mem[START] = bytes(region)
        h, = self.viewer(mem).snapshot()["heaps"]
        self.assertEqual(h["corrupt"], 1)
        self.assertTrue(h["blocks"][2] & heapview.BLOCK_CORRUPT)

    def test_task_tracking_layout(self):
        snap = self.viewer(build_heap(fill([(64, 0, None)]), task_tracking=True)).snapshot()
        self.assertEqual(len(snap["heaps"]), 1)

    def test_torn_heap_is_retried_then_skipped(self):
        mem = build_heap(fill([(64, 0, None), (64, 1, None)]))
        v = self.viewer(mem)
        self.assertEqual(len(v.snapshot()["heaps"]), 1)   # learns the pool start
        region = bytearray(mem[START])
        # Clear the prev-free bit of the block after the free one, as a
        # half-finished free would.
        nxt = 20 + CONTROL - 4 + 68 + 68 + 4
        struct.pack_into("<I", region, nxt, struct.unpack_from("<I", region, nxt)[0] & ~2)
        mem[START] = bytes(region)
        v.mon.commands.clear()
        snap = v.snapshot()
        self.assertEqual(snap["heaps"], [])
        self.assertEqual(snap["missing"], 1)
        self.assertGreater(v.mon.commands.count("stop"), 1)
        self.assertEqual(v.mon.commands[-1], "cont")

    def test_elf_symbol(self):
        strtab = b"\0registered_heaps\0other\0"
        syms = bytes(16) + struct.pack("<IIIBBH", 1, HEAD, 4, 0x11, 0, 3)
        shstr = b"\0"
        data_off = 52
        sym_off = data_off + len(strtab)
        sh_off = sym_off + len(syms)
        sh = bytes(40)
        sh += struct.pack("<10I", 0, 3, 0, 0, data_off, len(strtab), 0, 0, 1, 0)           # strtab
        sh += struct.pack("<10I", 0, 2, 0, 0, sym_off, len(syms), 1, 1, 4, 16)            # symtab
        ident = b"\x7fELF\x01\x01\x01" + bytes(9)
        hdr = ident + struct.pack("<HHIIIIIHHHHHH", 2, 0xF3, 1, 0, 0, sh_off, 0, 52, 0, 0, 40, 3, 0)
        with tempfile.NamedTemporaryFile(suffix=".elf") as f:
            f.write(hdr + strtab + syms + sh + shstr)
            f.flush()
            self.assertEqual(heapview.elf_symbol(f.name, "registered_heaps"), HEAD)


class FakeSymbolizer:
    def __init__(self, names):
        self.names = names

    def lookup(self, addr):
        fn, loc = self.names[addr + 1]
        return [(fn, loc)]


def record(kind, ptr, size, frames, old=0, t=0):
    return struct.pack(f"<HHIIIQ{len(frames)}I", kind, len(frames), ptr, size, old, t, *frames)


ALLOC, FREE, REALLOC = 1, 2, 3


class TraceTest(unittest.TestCase):
    def test_replay_incremental_and_restart(self):
        with tempfile.TemporaryDirectory() as d:
            path = Path(d) / "trace.bin"
            path.write_bytes(heapview.Trace.MAGIC[:5])           # header still being written
            trace = heapview.Trace(str(path))
            trace.update()
            self.assertEqual(trace.live, {})
            with open(path, "ab") as f:
                f.write(heapview.Trace.MAGIC[5:])
                f.write(record(ALLOC, 0x1000, 8, [1, 2, 3], t=10))
                f.write(record(ALLOC, 0x2000, 16, [5, 6], t=20)[:-2])   # cut mid-write
            trace.update()
            self.assertEqual(trace.live, {0x1000: (8, 0, 0)})
            with open(path, "ab") as f:
                f.write(record(ALLOC, 0x2000, 16, [5, 6], t=20)[-2:])
                f.write(record(FREE, 0x1000, 0, [9], t=30))
                f.write(record(REALLOC, 0x3000, 40, [7], old=0x2000, t=40))
                f.write(record(ALLOC, 0x1000, 24, [1, 2, 3], t=50))    # address reused, same stack
            trace.update()
            self.assertEqual(trace.live, {0x3000: (40, 3, 3), 0x1000: (24, 0, 4)})
            self.assertEqual(len(trace.stacks), 4)
            ev = trace.events(trace.gen, 2, 1)
            self.assertEqual((ev["start"], ev["total"], ev["t"], ev["k"], ev["o"]),
                             (2, 5, [20, 30, 40], [FREE, REALLOC, ALLOC], [0, 0x2000, 0]))
            self.assertEqual(ev["stackStart"], 1)
            self.assertEqual(len(ev["stackSite"]), 3)
            self.assertEqual(trace.events(trace.gen - 1, 2, 1)["start"], 0)   # stale client
            gen = trace.gen
            path.write_bytes(heapview.Trace.MAGIC + record(ALLOC, 0x5000, 4, [9]))   # QEMU restarted
            trace.update()
            self.assertEqual(trace.live, {0x5000: (4, 0, 0)})
            self.assertEqual(trace.gen, gen + 1)

    def test_older_log_is_ignored(self):
        with tempfile.NamedTemporaryFile() as f:
            f.write(struct.pack("<IIII", 0x1000, 8, 1, 5))
            f.flush()
            trace = heapview.Trace(f.name)
            trace.update()
            self.assertFalse(trace.format_ok)
            self.assertEqual(trace.live, {})

    def test_describe_stack(self):
        trace = heapview.Trace("/nonexistent")
        trace.intern((0x100, 0x200, 0x300, 0x400, 0x500))
        sym = FakeSymbolizer({
            0x100: ("multi_heap_malloc", "/x/.platformio/packages/framework-espidf/heap/multi_heap.c:10"),
            0x200: ("operator new(unsigned int)", "/builds/idf/crosstool-NG/new_op.cc:50"),
            0x300: ("std::vector<int, std::allocator<int> >::vector()",
                    "/x/.platformio/packages/toolchain-riscv32-esp/stl_vector.h:1"),
            0x400: ("Reader::open()", "/src/Reader.cpp:42"),
            0x500: ("loopTask(void*)", "/x/.platformio/packages/framework-arduinoespressif32/main.cpp:82"),
        })
        s = heapview.describe_stack(sym, trace, 0)
        self.assertEqual([f["alloc"] for f in s["frames"]], [True, True, False, False, False])
        self.assertEqual(s["caller"], 2)
        self.assertEqual(s["site"], 3)          # first frame in the firmware's own source
        self.assertIsNone(heapview.describe_stack(sym, trace, 1))
        self.assertIsNone(heapview.describe_stack(sym, trace, -1))

    def test_sites(self):
        mem = build_heap(fill([(64, 0, 40), (64, 0, 8), (64, 0, 24), (32, 0, None)]))
        v = HeapviewTest.viewer(self, mem)
        pool = START + 20 + CONTROL - 4
        sym = FakeSymbolizer({
            0x100: ("malloc", "/x/.platformio/packages/framework-espidf/heap/heap_caps.c:1"),
            0x200: ("Reader::open()", "/src/Reader.cpp:42"),
            0x300: ("Reader::open()", "/src/Reader.cpp:50"),
            0x400: ("setup()", "/src/main.cpp:9"),
        })
        sites = heapview.Sites(sym)
        with tempfile.NamedTemporaryFile() as f:
            f.write(heapview.Trace.MAGIC)
            f.write(record(ALLOC, pool + 16, 40, [0x100, 0x200, 0x400]))
            f.write(record(ALLOC, pool + 68 + 16, 8, [0x300, 0x400]))    # same function, other line
            f.write(record(ALLOC, pool + 136 + 16, 99, [0x400]))         # stale: size does not match
            f.flush()
            v.trace, v.sym = heapview.Trace(f.name, sites.of), sym
            snap = v.snapshot()
        h = snap["heaps"][0]
        self.assertEqual(h["sites"], [0, 0, -1, -1, -1])
        self.assertEqual(h["stk"], [0, 1, -1, -1, -1])
        self.assertEqual(h["ev"], [0, 1, -1, -1, -1])
        self.assertEqual(sites.names, [dict(fn="Reader::open()", loc="/src/Reader.cpp:42"),
                                       dict(fn="setup()", loc="/src/main.cpp:9")])
        self.assertEqual(snap["traceEvents"], 3)


if __name__ == "__main__":
    unittest.main()
