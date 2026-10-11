#!/usr/bin/env python3
"""Summarize SD probe console text or monitor events.jsonl (times in us)."""
import argparse
from collections import defaultdict
import json
import math
from pathlib import Path
import re
import statistics

FIELDS = ('total', 'cmd', 'wait', 'xfer', 'stop')


def percentile(values, fraction):
    ordered = sorted(values)
    position = (len(ordered) - 1) * fraction
    lower = math.floor(position)
    upper = math.ceil(position)
    return ordered[lower] + (ordered[upper] - ordered[lower]) * (position - lower)


def parse(text):
    ops, geometry, done = [], [], []
    for line in text.splitlines():
        if line.lstrip().startswith('{'):
            try:
                event = json.loads(line)
            except json.JSONDecodeError:
                continue
            if event.get('type') != 'log':
                continue
            line = event.get('line', '')
        match = re.search(r'\[SDP\] (OP|GEOMETRY|DONE) (.*)', line)
        if not match:
            continue
        fields = {key: int(value) if value.isdigit() else value
                  for key, value in re.findall(r'(\w+)=([^\s]+)', match[2])}
        if match[1] == 'OP':
            required = ('g', 'i', 'op', 'pat', 'n', 'sector', *FIELDS, 'polls')
            if any(key not in fields for key in required):
                raise ValueError(f'Incomplete OP line: {line}')
            for key in ('g', 'i', 'n', 'sector', *FIELDS, 'polls'):
                if not isinstance(fields[key], int) or fields[key] < 0:
                    raise ValueError(f'Invalid {key}: {line}')
            ops.append(fields)
        elif match[1] == 'GEOMETRY':
            geometry.append(fields)
        else:
            done.append(fields)
    return ops, geometry, done


def summarize(ops):
    groups = defaultdict(list)
    for op in ops:
        groups[op['op'], op['pat'], op['n']].append(op)
    result = []
    for (op, pattern, count), records in sorted(groups.items()):
        stats = {field: {'median': statistics.median(r[field] for r in records),
                         'p90': percentile([r[field] for r in records], .9)}
                 for field in FIELDS}
        stats['MB_s'] = {key: count * 512 / value if value else None
                         for key, value in stats['total'].items()}
        result.append(dict(op=op, pattern=pattern, count=count, samples=len(records), stats=stats))
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('log', type=Path)
    parser.add_argument('--json', type=Path)
    args = parser.parse_args()
    ops, geometry, done = parse(args.log.read_text(errors='replace'))
    if not ops:
        parser.error('No SD probe operations found')
    groups = summarize(ops)
    print('op pat  n samples | total med/p90 | cmd med/p90 | wait med/p90 | xfer med/p90 | stop med/p90 | MB/s med/p90-time')
    for group in groups:
        times = ' | '.join(f"{group['stats'][field]['median']:.1f}/{group['stats'][field]['p90']:.1f}" for field in FIELDS)
        speed = '/'.join('n/a' if value is None else f'{value:.3f}' for value in group['stats']['MB_s'].values())
        print(f"{group['op']:2} {group['pattern']:3} {group['count']:2} {group['samples']:7} | {times} | {speed}")
    complete = len(done) == 1 and done[0].get('ops') == len(ops) and done[0].get('errors') == 0
    print(f'Captured {len(ops)} operations; DONE={done}; complete={complete}')
    if args.json:
        args.json.write_text(json.dumps(dict(groups=groups, ops=ops, geometry=geometry, done=done,
                                             complete=complete), indent=2) + '\n')


if __name__ == '__main__':
    main()
