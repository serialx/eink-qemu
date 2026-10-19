"""The controller's exchange-memory (EM) mapping table, replayed from a trace.

Each entry `EMI_BASE + 4*i` holds bits [31:18] = EM base offset >> 2 and
bits [17:0] = DRAM address >> 2 (from 0x3fc00000). An entry whose DRAM field
is zero maps nothing; the firmware ends its list with one such entry at the
top of the EM (0xcc00, the 51 KiB of the ROM page table). The enable registers
span 48 entries, so that many are watched, although the firmware programs 40.

A window's DRAM block is a heap allocation: it is as large as the EM extent of
its entry (to the next EM base in the table) but no larger than the gap to the
next-higher mapped block. Both follow the table as it is at the time, so the
table writes are recorded in a first pass, every write starts a new table
state with its own windows, and an access is then resolved against the state
in force at its line of the trace.

A trace line is `<pc> <R|W> <addr> <size> <value>`; lines starting with `#`
are the plugin's notes (its limit marker) and are skipped.
"""
import bisect

EMI_BASE = 0x60031204
EMI_COUNT = 48
EMI_END = EMI_BASE + 4 * EMI_COUNT
DRAM_BASE = 0x3fc00000
EM_TOP = 0xcc00


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
        self.starts = []        # trace line of each table state
        self.states = []        # windows of that state: [(em, dram, size)]

    def record(self, seq, addr, value):
        """First pass: feed a write; returns True when it was a table entry."""
        if not is_table_write(addr):
            return False
        em, dram = decode(value)
        self.writes.append((seq, (addr - EMI_BASE) // 4, em, dram))
        return True

    @staticmethod
    def layout(entries):
        """Windows of one table state: a mapped entry's size runs to the
        next EM base in the table (the end marker included) or EM_TOP, and
        no further than the next-higher mapped DRAM block."""
        ems = sorted(em for em, _ in entries.values())
        drams = sorted(d for _, d in entries.values() if d is not None)
        out = []
        for em, dram in entries.values():
            if dram is None or em >= EM_TOP:
                continue
            size = EM_TOP - em
            k = bisect.bisect_right(ems, em)
            if k < len(ems):
                size = min(size, ems[k] - em)
            above = bisect.bisect_right(drams, dram)
            if above < len(drams):
                size = min(size, drams[above] - dram)
            out.append((em, dram, size))
        return out

    def finish(self):
        """Every table write starts a new state; the windows of each state
        are computed from that state alone."""
        entries = {}
        self.starts, self.states = [], []
        for seq, idx, em, dram in self.writes:
            entries[idx] = (em, dram)
            if self.starts and self.starts[-1] == seq:
                self.states[-1] = self.layout(entries)
            else:
                self.starts.append(seq)
                self.states.append(self.layout(entries))
        return self

    def resolve(self, seq, addr):
        """EM offset for a DRAM address at trace line seq, or None."""
        k = bisect.bisect_right(self.starts, seq) - 1
        if k < 0:
            return None
        for em, dram, size in self.states[k]:
            if dram <= addr < dram + size:
                return em + addr - dram
        return None

    def windows(self):
        """Every (em, dram, size) that was ever in force, by DRAM address."""
        return sorted({w for state in self.states for w in state},
                      key=lambda w: (w[1], w[0]))


def records(path):
    """The accesses in an iotrace file as (seq, pc, rw, addr, size, value),
    seq counting only accesses so both passes agree on it."""
    with open(path) as f:
        seq = 0
        for n, ln in enumerate(f, 1):
            if ln.startswith('#') or not ln.strip():
                continue
            try:
                pc, rw, addr, size, val = ln.split()
                yield (seq, int(pc, 16), rw, int(addr, 16), int(size),
                       int(val, 16))
            except ValueError:
                raise SystemExit('%s:%d: not a trace line: %s'
                                 % (path, n, ln.rstrip()))
            seq += 1


def read_table(path):
    """First pass over an iotrace file."""
    t = EmTable()
    for seq, pc, rw, addr, size, val in records(path):
        if rw == 'W':
            t.record(seq, addr, val)
    return t.finish()
