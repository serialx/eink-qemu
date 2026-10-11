#!/usr/bin/env python3
"""Regenerate phase measurement tables from benchmark/profile/counter JSON."""
import argparse
import collections
import json
from pathlib import Path
import statistics

ROOT = Path(__file__).resolve().parents[3]


def median(values):
    return statistics.median(values)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--evidence', type=Path, default=ROOT / 'build-wasm/evidence/profile')
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    load = lambda name: json.loads((args.evidence / name).read_text())
    timing = load('timing/results.json')
    node = load('node-analysis.json')
    native_home = load('native-home-analysis.json')
    native_settled = load('native-settled-analysis.json')
    counters = load('counters.json')
    rows = ['## Wall time, sleep disabled', '',
            '| Runtime | Boot → Home s | Home → settled s | Boot → settled s |',
            '| --- | ---: | ---: | ---: |']
    for variant in ('native', 'wasm'):
        m = timing['medians'][variant]
        rows.append(f'| {variant} | {m["home_s"]:.3f} | {m["home_to_settled_s"]:.3f} | {m["settled_s"]:.3f} |')
    rows += ['', '| Run order | Runtime | Home s | Home → settled s | Total s |',
             '| ---: | --- | ---: | ---: | ---: |']
    for number, run in enumerate(timing['runs'], 1):
        rows.append(f'| {number} | {run["variant"]} | {run["home_s"]:.3f} | '
                    f'{run["home_to_settled_s"]:.3f} | {run["settled_s"]:.3f} |')
    native_runs = [r for r in timing['runs'] if r['variant'] == 'native']
    wasm_runs = [r for r in timing['runs'] if r['variant'] == 'wasm']
    ratios = {key: median(w[key] / n[key] for n,w in zip(native_runs,wasm_runs))
              for key in ('home_s','home_to_settled_s','settled_s')}
    rows += ['', 'Median paired Wasm/native ratios: ' + ', '.join(f'{k} **{v:.3f}x**' for k,v in ratios.items()) + '.',
             'Raw: `timing/results.json`; order native, Wasm, repeated three times.',
             '', '## vCPU sampled self cost by phase', '',
             'Percentages are medians of three per-run shares; column sums can differ slightly from 100%.',
             'Native uses stack self samples; Node uses time-delta-weighted elapsed samples, including waits.', '',
             '| Category | Native Home % | Native settled % | Wasm Home % | Wasm settled % |',
             '| --- | ---: | ---: | ---: | ---: |']
    sets = [(native_home,'home'), (native_settled,'settled'), (node,'home'), (node,'settled')]
    categories = {c for data,phase in sets for r in data.values() for c in r[phase]['categories']}
    def shares(category):
        return [median(100 * r[phase]['categories'].get(category,0) / r[phase]['total'] for r in data.values()) for data,phase in sets]
    for category in sorted(categories, key=lambda c: -shares(c)[3]):
        rows.append('| ' + category + ' | ' + ' | '.join(f'{v:.2f}' for v in shares(category)) + ' |')
    rows += ['', 'Raw: `node-analysis.json`, `native-home-analysis.json`, `native-settled-analysis.json`.',
             '', '## Counters at exact firmware phase boundaries', '',
             'Medians of three instrumented runs; each snapshot resets after the Home line.',
             'These builds are excluded from the wall-time and CPU-profile tables.', '',
             '| Counter | Native Home | Native settled | Wasm Home | Wasm settled |',
             '| --- | ---: | ---: | ---: | ---: |']
    def counter_values(key, func=lambda v:v):
        return [median(func(r['phases'][phase]['counts'][key]) for name,r in counters.items() if name.startswith(variant+'-'))
                for variant in ('native','wasm') for phase in (0,1)]
    labels = {'dispatch':'Dispatcher TB-chain entries (exits within ±1)',
              'io_recompile':'cpu_io_recompile exits', 'icount_get':'icount_get calls',
              'bql_lock':'BQL lock attempts', 'clock_notify':'qemu_clock_notify calls',
              'mainloop':'Main-loop iterations', 'ld_helper':'Data load helper entries',
              'st_helper':'Data store helper entries', 'guest_ld':'Executed qemu_ld operations',
              'guest_st':'Executed qemu_st operations', 'native_insn':'Native instruction starts',
              'tci_insn':'TCI instruction starts', 'jit_insn':'JIT instruction starts',
              'spi_tx':'GPSPI transactions', 'spi_bytes':'GPSPI full-duplex byte clocks',
              'sd_read':'SD reads / 512-byte sectors', 'sd_bytes':'SD bytes read',
              'sd_sequential':'Reads contiguous with preceding read', 'sd_repeat':'Immediate same-address rereads',
              'display':'Console update calls', 'sdl':'SDL event polls'}
    for key,label in labels.items():
        rows.append('| '+label+' | '+' | '.join(f'{v:,.0f}' for v in counter_values(key))+' |')
    inline=[]
    for variant in ('native','wasm'):
        for phase in (0,1):
            vals=[]
            for name,r in counters.items():
                if name.startswith(variant+'-'):
                    c=r['phases'][phase]['counts']
                    vals.append(c['guest_ld']+c['guest_st']-c['ld_helper']-c['st_helper'])
            inline.append(median(vals))
    rows.append('| Inline data accesses (operations − helper entries) | '+' | '.join(f'{v:,.0f}' for v in inline)+' |')
    rows += ['', 'Helper entry is **not** a hardware TLB-miss count: TCI uses the C helper even for RAM hits.',
             'Instruction starts include fault/retry attempts; they count guest instructions, not TCG ops or modules.',
             'Subtracting io_recompile counts reconciles native and Wasm instruction totals.', '',
             '| Wasm phase | TCI instruction share | JIT instruction share | Asyncify unwinds | Asyncify rewinds |',
             '| --- | ---: | ---: | ---: | ---: |']
    for phase,label in enumerate(('Home','Home → settled')):
        variants=[r['phases'][phase] for name,r in counters.items() if name.startswith('wasm-')]
        tci=median(100*r['counts']['tci_insn']/(r['counts']['tci_insn']+r['counts']['jit_insn']) for r in variants)
        values=[median(r['asyncify'][0][key] for r in variants) for key in ('unwinds','rewinds')]
        rows.append(f'| {label} | {tci:.3f}% | {100-tci:.3f}% | {values[0]:,.0f} | {values[1]:,.0f} |')
    rows += ['', '| Phase | New JIT modules | Synchronous compile ms | Synchronous instantiate ms |',
             '| --- | ---: | ---: | ---: |']
    for phase in ('home','settled'):
        vals=[median(r[phase]['constructors'][k] for r in node.values()) for k in ('count','compileMs','instanceMs')]
        rows.append(f'| {phase} | {vals[0]:.0f} | {vals[1]:.3f} | {vals[2]:.3f} |')
    rows += ['', 'Constructor costs exclude V8 background optimization. Raw: `counters.json` and `node-cpu/*/modules-*.jsonl`.',
             '', '## Device registers and timer traffic', '',
             'Named MMIO regions only; alias recursion and unnamed subpage routing regions are excluded. Values are medians.', '',
             '| Region | Native Home R/W | Native settled R/W | Wasm Home R/W | Wasm settled R/W |',
             '| --- | ---: | ---: | ---: | ---: |']
    regions={r[0] for d in counters.values() for p in d['phases'] for r in p['mmio']}
    def region_counts(region):
        values=[]
        for variant in ('native','wasm'):
            for phase in (0,1):
                accesses=[r['phases'][phase]['mmio'] for n,r in counters.items() if n.startswith(variant+'-')]
                values.append(tuple(median(sum(a[k] for a in row if a[0]==region) for row in accesses) for k in (2,3)))
        return values
    for region in sorted(regions - {'(null)'}, key=lambda r: -sum(region_counts(r)[3])):
        values=region_counts(region)
        rows.append('| '+region+' | '+' | '.join(f'{r:,.0f} / {w:,.0f}' for r,w in values)+' |')
    rows += ['', '| Wasm GPSPI register | Home reads/writes | Settled reads/writes |',
             '| --- | ---: | ---: |']
    for addr,label in [(0,'CMD'),(12,'CLOCK'),(16,'USER (unmodeled storage)'),(28,'MS_DLEN')] + [(152+i*4,f'W{i}') for i in range(16)]:
        values=[]
        for phase in (0,1):
            for rw in (2,3):
                values.append(median(sum(a[rw] for a in r['phases'][phase]['mmio'] if a[0]=='ssi.esp32s3.gpspi' and a[1]==addr) for n,r in counters.items() if n.startswith('wasm-')))
        rows.append(f'| {label} (0x{addr:02x}) | {values[0]:,.0f} / {values[1]:,.0f} | {values[2]:,.0f} / {values[3]:,.0f} |')
    rows += ['', '| Timer callback | Native Home mod/del | Native settled mod/del | Wasm Home mod/del | Wasm settled mod/del |',
             '| --- | ---: | ---: | ---: | ---: |']
    timers={t[0] for d in counters.values() for phase in d['phases'] for t in phase['timers'] if t[1] or t[2]}
    for timer in sorted(timers):
        vals=[]
        for variant in ('native','wasm'):
            for phase in (0,1):
                vals.append(tuple(median(sum(a[k] for a in r['phases'][phase]['timers'] if a[0]==timer) for n,r in counters.items() if n.startswith(variant+'-')) for k in (1,2)))
        rows.append('| '+timer+' | '+' | '.join(f'{m:,.0f} / {d:,.0f}' for m,d in vals)+' |')
    rows += ['', 'Timers with identical callback names are combined; raw snapshots retain individual timer instances.',
             '`timer_mod_ns` and `timer_mod_anticipate_ns` are calls, not necessarily changes in expiry;',
             '`timer_del` counts explicit public calls, not internal unlinking by every modification.', '',
             '## GPSPI callback host duration', '',
             'Instrumented inclusive elapsed time, including nested SD/block I/O and host scheduling.',
             'It is not exclusive model CPU time; clock reads and instrumentation perturb it.', '',
             '| Runtime / phase | Total callback s | Mean µs/transaction | Transactions / SD sector | SPI byte clocks / SD sector |',
             '| --- | ---: | ---: | ---: | ---: |']
    for variant in ('native','wasm'):
        for phase,label in enumerate(('Home','settled')):
            data=[r['phases'][phase]['counts'] for n,r in counters.items() if n.startswith(variant+'-')]
            vals=[median(c['spi_ns']/1e9 for c in data),median(c['spi_ns']/c['spi_tx']/1000 for c in data),median(c['spi_tx']/c['sd_read'] for c in data),median(c['spi_bytes']/c['sd_read'] for c in data)]
            rows.append(f'| {variant} {label} | {vals[0]:.3f} | {vals[1]:.3f} | {vals[2]:.2f} | {vals[3]:.2f} |')
    rows += ['', 'Ratios divide all SPI traffic in the phase by SD sectors; they include protocol polling and any other SSI traffic.',
             '', '## Top 30 vCPU self symbols per phase', '',
             'Ranked by mean per-run self share. Self columns are mean raw samples for native and mean sampled elapsed ms for Node.',
             'They are statistical attribution, not exact OS CPU accounting.', '']
    for label,(data,phase) in zip(('Native Home','Native Home → settled','Wasm Home','Wasm Home → settled'),sets):
        merged=collections.defaultdict(lambda:[0,0,''])
        for r in data.values():
            for item in r[phase].get('self_symbols',r[phase]['top30']):
                v=merged[item['symbol']]
                v[0]+=item['percent']/len(data);v[1]+=item['self']/len(data);v[2]=item['category']
        rows += ['### '+label,'','| Symbol | Mean self | Mean self % | Category |','| --- | ---: | ---: | --- |']
        for symbol,(percent,value,category) in sorted(merged.items(),key=lambda v:-v[1][0])[:30]:
            rows.append(f'| `{symbol}` | {value:.2f} | {percent:.2f} | {category} |')
        rows.append('')
    args.output.write_text('\n'.join(rows).rstrip()+'\n')
    print(args.output)


if __name__ == '__main__':
    main()
