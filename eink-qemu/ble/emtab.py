"""The controller's exchange-memory (EM) mapping table, replayed from a trace.

Each entry `EMI_BASE + 4*i` holds bits [31:18] = EM base offset >> 2 and
bits [17:0] = DRAM address >> 2 (from 0x3fc00000). An entry whose DRAM field
is zero maps nothing; the firmware ends its list with one such entry at the
top of the EM (0xcc00, the 51 KiB of the ROM page table). The enable registers
span 48 entries, so that many are watched, although the firmware programs 40.

A window's DRAM block is a heap allocation: it is as large as the EM extent of
its entry (to the next EM base in the table) but no larger than the gap to the
next-higher block mapped while it is. Both are known only once the whole trace
has been read, so the table is recorded in a first pass and an access is then
resolved against the entry that mapped its address at that line of the trace.
"""
import bisect

EMI_BASE = 0x60031204
EMI_COUNT = 48
EMI_END = EMI_BASE + 4 * EMI_COUNT
DRAM_BASE = 0x3fc00000
EM_TOP = 0xcc00
INF = float('inf')


def decode(value):
    """Return (em_base, dram) for a table value; dram is None when unmapped."""
    em = (value >> 18) << 2
    dram_field = value & 0x3ffff
    return em, DRAM_BASE + dram_field * 4 if dram_field else None


def is_table_write(addr):
    return EMI_BASE <= addr < EMI_END


class EmTable:
    def __init__(self):
        self.writes = []        # (seq, idx, em, dram) in trace order
        self.epochs = []        # (start, end, em, dram, size), dram mapped

    def record(self, seq, addr, value):
        """First pass: feed a write; returns True when it was a table entry."""
        if not is_table_write(addr):
            return False
        em, dram = decode(value)
        self.writes.append((seq, (addr - EMI_BASE) // 4, em, dram))
        return True

    def finish(self):
        """Turn the writes into epochs with their sizes."""
        ems = sorted({em for _, _, em, _ in self.writes} | {EM_TOP})
        spans = []
        for n, (start, idx, em, dram) in enumerate(self.writes):
            end = next((s for s, i, _, _ in self.writes[n + 1:] if i == idx), INF)
            if dram is not None and em < EM_TOP:
                spans.append([start, end, em, dram])
        self.epochs = []
        for start, end, em, dram in spans:
            size = ems[bisect.bisect_right(ems, em)] - em
            for s2, e2, _, d2 in spans:
                if d2 > dram and s2 < end and start < e2:
                    size = min(size, d2 - dram)
            self.epochs.append((start, end, em, dram, size))
        return self

    def resolve(self, seq, addr):
        """EM offset for a DRAM address at trace line seq, or None."""
        for start, end, em, dram, size in self.epochs:
            if start <= seq < end and dram <= addr < dram + size:
                return em + addr - dram
        return None

    def windows(self):
        """Every (em, dram, size) that was ever mapped, by DRAM address."""
        return sorted({(em, dram, size) for _, _, em, dram, size in self.epochs},
                      key=lambda w: (w[1], w[0]))


def read_table(path):
    """First pass over an iotrace file."""
    t = EmTable()
    with open(path) as f:
        for seq, ln in enumerate(f):
            pc, rw, addr, size, val = ln.split()
            if rw == 'W':
                t.record(seq, int(addr, 16), int(val, 16))
    return t.finish()
