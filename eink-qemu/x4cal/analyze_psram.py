#!/usr/bin/env python3
"""Validate paired PSRAM records and summarize held-out hardware residuals."""
import csv
from collections import defaultdict
import json
from pathlib import Path
from statistics import median

ROOT = Path(__file__).resolve().parent / 'evidence'


def analyze(filename, keys, checksums):
    rows = list(csv.DictReader((ROOT / filename).open()))
    runs = defaultdict(dict)
    for row in rows:
        key = tuple(row[k] for k in keys) + (row['rep'],)
        run = runs[row['source'], row['boot']]
        assert key not in run, (filename, 'duplicate', key)
        run[key] = row
    reference = runs['hardware', '1']
    for name, run in runs.items():
        assert run.keys() == reference.keys(), (filename, name, 'incomplete run')
        for key, row in run.items():
            for field in checksums:
                assert row[field] == reference[key][field], (filename, name, key, field)
    medians = {}
    for name, run in runs.items():
        groups = defaultdict(list)
        for key, row in run.items():
            groups[key[:-1]].append(int(row['us']))
        medians[name] = {k: median(v) for k, v in groups.items()}
    hw = medians['hardware', '1']
    result = {'rows': len(rows), 'groups': len(hw), 'checksums_match': True, 'sources': {}}
    for (source, boot), groups in medians.items():
        if source == 'hardware':
            continue
        errors = {k: 100 * (v / hw[k] - 1) for k, v in groups.items()}
        worst = max(errors, key=lambda k: abs(errors[k]))
        result['sources'][source] = {
            'median_absolute_percent': round(median(abs(v) for v in errors.values()), 3),
            'maximum_absolute_percent': round(abs(errors[worst]), 3),
            'worst_group': dict(zip(keys, worst)),
        }
    if ('native', '0') in medians and ('node', '0') in medians:
        result['native_node_max_median_difference_us'] = max(
            abs(v - medians['node', '0'][k]) for k, v in medians['native', '0'].items())
    if filename == 'psram-cpu.csv':
        result['selected'] = [dict(zip(keys, k), hardware_us=hw[k],
                                   baseline_us=medians['baseline', '0'][k],
                                   native_us=medians['native', '0'][k])
                              for k in hw if k[0] in ('render_sram', 'render_psram') or
                              (k[0] in ('psram_seq', 'psram_random') and k[2] == '524288')]
    return result


if __name__ == '__main__':
    print(json.dumps({
        'capacity_and_patterns': analyze('psram.csv', ['mhz', 'bytes', 'op', 'stride', 'warm'], ['sum']),
        'focused_kernels': analyze('psram-detail.csv', ['mhz', 'bytes', 'stride', 'op', 'offset'], ['sum', 'hash']),
        'original_cpu_validation': analyze('psram-cpu.csv', ['name', 'mhz', 'bytes'], ['sum']),
    }, indent=2))
