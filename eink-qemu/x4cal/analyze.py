#!/usr/bin/env python3
"""Validate published measurements and print hardware/emulator median residuals.

Run without arguments for the checked-in evidence. --fit-cpu reconstructs the
nonnegative relative-error fit from the baseline and one-nanosecond sweeps.
All durations are guest microseconds, not host execution times.
"""
import argparse
import csv
import json
from collections import defaultdict
from pathlib import Path
from statistics import median


def grouped(rows, keys):
    result = defaultdict(list)
    for row in rows:
        result[tuple(row[k] for k in keys)].append(row)
    return result


def compare(rows, keys, exclude=()):
    hardware = grouped([r for r in rows if r['origin'] == 'hardware'], keys)
    emulator = grouped([r for r in rows if r['origin'] == 'emulator'], keys)
    result = []
    for key, samples in hardware.items():
        if key[0] in exclude:
            continue
        if key not in emulator:
            raise ValueError(f'Missing emulator measurement: {key}')
        other = emulator[key]
        if 'sum' in samples[0]:
            a, b = {r['sum'] for r in samples}, {r['sum'] for r in other}
            if len(a) != 1 or a != b:
                raise ValueError(f'Checksum mismatch: {key}: {a} != {b}')
        hw, emu = median(int(r['us']) for r in samples), median(int(r['us']) for r in other)
        result.append(dict(zip(keys, key), hardware_us=hw, emulator_us=emu,
                           error_pct=round(100 * (emu / hw - 1), 3),
                           hardware_samples=len(samples), emulator_samples=len(other)))
    return result


def fit_cpu(rows):
    """Coordinate descent with nonnegative coefficients; render/PSRAM held out."""
    keys = ('name', 'mhz', 'bytes')
    def med(origin, run=None):
        groups = grouped([r for r in rows if r['origin'] == origin and
                          (run is None or r['run'] == run)], keys)
        return {k: median(int(r['us']) for r in v) for k, v in groups.items()}
    hw, base = med('training'), med('baseline')
    props = ['load', 'store', 'mul', 'div', 'branch', 'call', 'fpu', 'fma', 'rom-ns']
    differences = [med('sensitivity', p) for p in props]
    train = [k for k in hw if k[1] == '240' and not k[0].startswith(('psram', 'render'))]
    a = [[(d[k] - base[k]) / hw[k] for d in differences] for k in train]
    y = [(hw[k] - base[k]) / hw[k] for k in train]
    x = [0.] * len(props)
    for _ in range(10000):
        for j in range(len(x)):
            den = sum(row[j] ** 2 for row in a)
            if den:
                x[j] = max(0, sum(row[j] * (b - sum(row[t] * x[t] for t in range(len(x)) if t != j))
                                  for row, b in zip(a, y)) / den)
    x = [round(v) for v in x]
    def loss(z):
        return sum((sum(v * w for v, w in zip(row, z)) - b) ** 2 for row, b in zip(a, y))
    for _ in range(10):
        for j in range(len(x)):
            trials = []
            for v in range(max(0, x[j] - 3), x[j] + 4):
                z = x.copy(); z[j] = v
                trials.append((loss(z), v))
            x[j] = min(trials)[1]
    return {'base_ns': 4, 'extra_ns': dict(zip(props, x)),
            'training_rows': len(train), 'relative_squared_error': loss(x)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--evidence', type=Path, default=Path(__file__).with_name('evidence'))
    parser.add_argument('--fit-cpu', action='store_true')
    args = parser.parse_args()
    tables = {}
    for kind in ('cpu', 'sd', 'panel', 'spi'):
        with (args.evidence / (kind + '.csv')).open() as stream:
            tables[kind] = list(csv.DictReader(stream))
        for row in tables[kind]:
            for key in ('err', 'mismatch', 'timeout'):
                if key in row and int(row[key], 16 if key == 'err' else 10):
                    raise ValueError(f'Failed measurement: {row}')
    sd_hw = [r for r in tables['sd'] if r['origin'] == 'hardware']
    if len(sd_hw) != 6480:
        raise ValueError(f'Expected 6480 verified hardware SD operations, got {len(sd_hw)}')
    report = {
        'cpu': compare(tables['cpu'], ('name', 'mhz', 'bytes')),
        'sd': compare(tables['sd'], ('op', 'khz', 'n')),
        'sd_by_pattern': compare(tables['sd'], ('op', 'khz', 'n', 'pat')),
        'panel': compare(tables['panel'], ('name', 'pll', 'frames'), exclude=('pof',)),
        'spi': compare(tables['spi'], ('hz', 'chunk')),
    }
    if args.fit_cpu:
        report['cpu_fit'] = fit_cpu(tables['cpu'])
    print(json.dumps(report, indent=2))


if __name__ == '__main__':
    main()
