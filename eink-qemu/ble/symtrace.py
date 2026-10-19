"""Attribute iotrace lines to functions: symtrace.py fw-syms.txt io.txt [base]

fw-syms.txt is `nm -n firmware.elf | grep ' [TtWw] '`. Prints, in trace order,
runs of accesses grouped by function, and a per-function register summary.
"""
import bisect
import collections
import sys

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


import json, os
WINS = []
if os.path.exists('emwins.json'):
    WINS = [w for w in json.load(open('emwins.json')) if w['dram'] != 0x3fc00000]
    WINS.sort(key=lambda w: w['dram'])


def em(a):
    best = None
    for w in WINS:
        if w['dram'] <= a:
            best = w
    if best and a < best['dram'] + best['size']:
        return 'em+%05x' % (best['em'] + a - best['dram'])
    return None


def reg(a):
    e = em(a)
    if e:
        return e
    if 0x60031000 <= a < 0x60032000:
        return 'ble+%03x' % (a - 0x60031000)
    if 0x60011000 <= a < 0x60013000:
        return 'bb+%03x' % (a - 0x60011000)
    if 0x600c2000 <= a < 0x600c2200:
        return 'intm+%03x' % (a - 0x600c2000)
    return '%08x' % a


# A window counts only once the controller has written its table entry, so
# earlier users of the same heap bytes are left out.
live = set()
rows = []
for ln in open(sys.argv[2]):
    pc, rw, addr, size, val = ln.split()
    a = int(addr, 16)
    if rw == 'W' and 0x60031204 <= a < 0x600312a4:
        v = int(val, 16)
        if v & 0x3ffff:
            live.add((a - 0x60031204) // 4)
        else:
            live.discard((a - 0x60031204) // 4)
    label = reg(a)
    if label.startswith('em+'):
        off = int(label[3:], 16)
        w = [w for w in WINS if w['em'] <= off < w['em'] + w['size']]
        if not w or w[0]['idx'] not in live:
            continue
    elif not label.startswith(('ble+', 'bb+', 'intm+')):
        continue
    rows.append((sym(int(pc, 16)), rw, a, int(val, 16)))

mode = sys.argv[3] if len(sys.argv) > 3 else 'runs'
if mode == 'runs':
    last = None
    run = []

    def flush():
        if run:
            print('%s (%d): %s' % (last, len(run), ' '.join(run[:24]) + (' ...' if len(run) > 24 else '')))

    for f, rw, a, v in rows:
        if f != last:
            flush()
            last, run = f, []
        run.append('%s%s=%x' % (rw, reg(a), v))
    flush()
else:
    per = collections.OrderedDict()
    for f, rw, a, v in rows:
        per.setdefault(f, collections.Counter())[(rw, a)] += 1
    for f, c in per.items():
        print('%s: %d' % (f, sum(c.values())))
        print('   ' + ' '.join('%s%s' % (rw, reg(a)) for (rw, a), n in sorted(c.items())))
