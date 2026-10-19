"""emwins.py io.txt [pad] -> writes emwins.json from the EMI table writes in io.txt; prints range args."""
import json, sys
emi = {}
for ln in open(sys.argv[1]):
    pc, rw, addr, size, val = ln.split(); a = int(addr, 16)
    if rw == 'W' and 0x60031204 <= a < 0x600312a4:
        emi[(a - 0x60031204) // 4] = int(val, 16)
pad = int(sys.argv[2], 0) if len(sys.argv) > 2 else 0
wins = [(i, (emi[i] >> 18) << 2, 0x3fc00000 + ((emi[i] & 0x3ffff) << 2)) for i in sorted(emi)]
out = []
for k, (i, base, dram) in enumerate(wins):
    end = wins[k + 1][1] if k + 1 < len(wins) else base
    out.append(dict(idx=i, em=base, end=end, dram=dram, size=end - base))
# A window's DRAM block is the controller's own allocation: it ends where the
# next-higher allocation starts, at most the window's exchange-memory size.
mapped = sorted((w['dram'] for w in out if w['dram'] != 0x3fc00000))
for w in out:
    nxt = [d for d in mapped if d > w['dram']]
    if nxt:
        w['size'] = min(w['size'], nxt[0] - w['dram'])
json.dump(out, open('emwins.json', 'w'))
print(','.join('range=0x%x-0x%x' % (w['dram'] - pad, w['dram'] + w['size'] + pad) for w in out if w['dram'] != 0x3fc00000))
