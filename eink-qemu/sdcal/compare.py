#!/usr/bin/env python3
"""Check the calibrated probe against the supplied pooled device JSON."""
import json
from pathlib import Path
import sys
import parse

reference = json.loads(Path(sys.argv[1]).read_text())
ops, geometry, done = parse.parse(Path(sys.argv[2]).read_text(errors='replace'))
assert len(done) == 1 and done[0]['errors'] == 0 and done[0]['ops'] == len(ops) == 2880
actual = parse.summarize(ops)
expected = {(g['op'], g['pattern'], g['count']): g for g in reference['groups']}
assert len(actual) == len(expected) == 30
print('| Op | Pattern | Sectors | Device median (us) | Emulator median (us) | Error |')
print('| --- | --- | ---: | ---: | ---: | ---: |')
worst = {'r': 0, 'w': 0}
for row in actual:
    key = row['op'], row['pattern'], row['count']
    device = expected[key]['stats']['total']['median']
    emulator = row['stats']['total']['median']
    error = 100 * (emulator / device - 1)
    assert row['samples'] == 96 and expected[key]['samples'] == 288
    assert abs(error) <= (5 if row['op'] == 'r' else 15), (key, error)
    worst[row['op']] = max(worst[row['op']], abs(error))
    print(f'| {key[0]} | {key[1]} | {key[2]} | {device:g} | {emulator:g} | {error:+.2f}% |')
print(f'\nPASS: 30/30 rows; worst read {worst["r"]:.2f}%, write {worst["w"]:.2f}%.')
