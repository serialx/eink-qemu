"""symtrace.py fw-syms.txt io.txt runs|summary

fw-syms.txt is `nm -n firmware.elf | grep ' [TtWw] '`. `runs` prints the
accesses in trace order grouped into runs by function; `summary` prints the
registers each function touched. Exchange-memory accesses (em+) are resolved
by replaying the EM table writes in the same trace, so a DRAM block counts only
while its table entry maps it.
"""
import bisect
import collections
import sys

from emtab import read_table, records

MODES = ('runs', 'summary')
if len(sys.argv) != 4 or sys.argv[3] not in MODES:
    sys.exit(__doc__)
mode = sys.argv[3]

syms = []
for ln in open(sys.argv[1]):
    parts = ln.split()
    if len(parts) >= 3:
        syms.append((int(parts[0], 16), parts[2]))
syms.sort()
addrs = [a for a, _ in syms]


def sym(pc):
    i = bisect.bisect_right(addrs, pc) - 1
    return syms[i][1] if i >= 0 else '?'


def label(table, seq, a):
    em = table.resolve(seq, a)
    if em is not None:
        return 'em+%05x' % em
    if 0x60031000 <= a < 0x60032000:
        return 'ble+%03x' % (a - 0x60031000)
    if 0x60011000 <= a < 0x60013000:
        return 'bb+%03x' % (a - 0x60011000)
    if 0x600c2000 <= a < 0x600c2200:
        return 'intm+%03x' % (a - 0x600c2000)
    return None


SHOW = 24


class Runs:
    def __init__(self):
        self.fn, self.n, self.head = None, 0, []

    def add(self, fn, text):
        if fn != self.fn:
            self.flush()
            self.fn, self.n, self.head = fn, 0, []
        self.n += 1
        if len(self.head) < SHOW:
            self.head.append(text)

    def flush(self):
        if self.fn is not None:
            print('%s (%d): %s' % (self.fn, self.n, ' '.join(self.head)
                                   + (' ...' if self.n > SHOW else '')))


runs = Runs()
per = collections.OrderedDict()
table = read_table(sys.argv[2])
for seq, pc, rw, a, size, v in records(sys.argv[2]):
    name = label(table, seq, a)
    if name is None:
        continue
    fn = sym(pc)
    if mode == 'runs':
        runs.add(fn, '%s%s=%x' % (rw, name, v))
    else:
        per.setdefault(fn, collections.Counter())[(rw, name)] += 1
if mode == 'runs':
    runs.flush()
else:
    for fn, c in per.items():
        print('%s: %d' % (fn, sum(c.values())))
        print('   ' + ' '.join('%s%s' % k for k in sorted(c)))
