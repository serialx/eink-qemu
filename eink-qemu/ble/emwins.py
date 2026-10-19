"""emwins.py io.txt [pad] -> emwins.json and the plugin range args for a rerun.

Replays the EM table writes in io.txt; the ranges are the union of every DRAM
block that was ever mapped, so a rerun captures the controller's accesses to
all of them. `pad` (bytes) widens each range to absorb heap drift between runs.
"""
import json
import sys

from emtab import read_table

if len(sys.argv) not in (2, 3):
    sys.exit(__doc__)
try:
    pad = int(sys.argv[2], 0) if len(sys.argv) > 2 else 0
except ValueError:
    sys.exit(__doc__)
if pad < 0:
    sys.exit('emwins: pad must not be negative')
wins = read_table(sys.argv[1]).windows()
if not wins:
    sys.exit('emwins: no EM table writes in %s' % sys.argv[1])
json.dump([dict(em=em, dram=dram, size=size) for em, dram, size in wins],
          open('emwins.json', 'w'))
# One range per contiguous DRAM span, whichever windows lived there.
spans = []
for em, dram, size in wins:
    lo, hi = dram - pad, dram + size + pad
    if spans and lo <= spans[-1][1]:
        spans[-1][1] = max(spans[-1][1], hi)
    else:
        spans.append([lo, hi])
print(','.join('range=0x%x-0x%x' % (lo, hi) for lo, hi in spans))
