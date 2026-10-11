#!/usr/bin/env python3
"""Aggregate Node CPU profiles or macOS sample trees into X3 phase categories."""
import argparse
import collections
import json
from pathlib import Path
import re

ROOT = Path(__file__).resolve().parents[3]


def device_symbols():
    result = {}
    for subdir in ('ssi', 'sd', 'gpio', 'timer', 'misc', 'display', 'i2c', 'char', 'intc'):
        for p in (ROOT / 'hw' / subdir).glob('*.c'):
            if any(s in p.name for s in ('esp', 'uc8', 'sd', 'bq', 'ds3231', 'qmi', 'ssi')):
                for match in re.finditer(r'^\w[\w\s*]*?\b([A-Za-z_]\w*)\([^;]*?\)\s*\{', p.read_text(errors='replace'), re.M):
                    result[match[1]] = str(p.relative_to(ROOT))
    return result


DEVICES = device_symbols()


def classify(name, url=''):
    if url.startswith('wasm://') and 'qemu-system-riscv32' not in url:
        return 'JIT TB modules', 'TB entry (generated modules)'
    if any(x in name for x in ('futex_wait', 'psynch_cvwait', 'psynch_mutexwait', 'semaphore_wait', 'mach_msg', '__select', '__poll', 'kevent', '__semwait', 'ulock_wait')) or name == '(idle)':
        return 'Wait', name
    if name in DEVICES:
        return 'Device models: ' + DEVICES[name], name
    if name.startswith(('tcg_qemu_tb_exec_tci', 'ffi_')):
        return 'TCI / libffi', name
    if 'asyncify' in name.lower() or name in ('helper.u', 'saveRewindArguments', 'restoreRewindArguments', 'wrapper', 'maybeStopUnwind', 'finishContextSwitch', 'doRewind', 'trampoline'):
        return 'Asyncify / fibers (visible)', name
    if name in ('Module', 'Instance', 'instantiate_wasm') or 'profile-hooks.cjs' in url:
        return 'Module constructors / hooks', name
    if name.startswith(('helper_', 'do_ld', 'do_st', 'mmu_', 'memory_region_', 'flatview_', 'address_space_', 'subpage_', 'tlb_', 'get_page_addr')) or name == 'access_with_adjusted_size':
        return 'Helpers / softmmu / MMIO', name
    if name.startswith(('cpu_', 'riscv_get_tb_cpu_state', 'tcg_cpu_exec', 'cpu_loop', 'icount_', 'timer', 'qemu_clock', 'main_loop', 'rr_cpu_', 'replay_')):
        return 'Dispatch / main loop / timers / icount', name
    if name.startswith(('tcg_', 'tb_', 'tgen_', 'liveness_', 'qht_')):
        return 'Translation / lookup', name
    if url and not url.startswith('wasm://'):
        return 'JS glue / V8', name or 'anonymous JS'
    if name.startswith('???'):
        return 'Unresolved / native generated code', name
    return 'Other QEMU / runtime', name


def summarize(functions, scale, unit):
    cats = collections.Counter()
    for (cat, name), value in functions.items():
        cats[cat] += value
    total = sum(cats.values())
    return {'unit': unit, 'total': total * scale,
            'categories': {k: v * scale for k,v in cats.most_common()},
            'self_symbols': [{'category': c, 'symbol': n, 'self': v * scale,
                              'percent': 100 * v / total if total else 0}
                             for (c,n),v in functions.most_common()],
            'top30': [{'category': c, 'symbol': n, 'self': v * scale,
                       'percent': 100 * v / total if total else 0}
                      for (c,n),v in functions.most_common(30)]}


def node_profile(directory, marks, thread):
    candidates = []
    for p in directory.glob('*.cpuprofile'):
        d = json.loads(p.read_text())
        names = {n['callFrame']['functionName'] for n in d['nodes']}
        selected = (('tcg_qemu_tb_exec_tci' in names or 'cpu_tb_exec' in names) if thread == 'vcpu'
                    else ('main_loop_wait' in names if thread == 'mainloop'
                          else re.search(r'\.0\.\d+\.cpuprofile$', p.name)))
        if selected:
            candidates.append((p,d))
    if len(candidates) != 1:
        raise RuntimeError(f'expected one selected thread profile, found {len(candidates)} in {directory}')
    p,d = candidates[0]
    nodes = {n['id']:n['callFrame'] for n in d['nodes']}
    if not d['startTime'] <= marks['home_us'] <= d['endTime']:
        raise RuntimeError('V8/Python monotonic clock mismatch; do not guess profile offsets')
    parents = {child: n['id'] for n in d['nodes'] for child in n.get('children', [])}
    phases = {'home': (marks['start_us'], marks['home_us']),
              'settled': (marks['home_us'], marks['settled_us'])}
    out = {}
    for phase,(begin,end) in phases.items():
        funcs = collections.Counter()
        waits = collections.Counter()
        timestamp = d['startTime']
        for ident, delta in zip(d['samples'], d['timeDeltas']):
            next_time = timestamp + delta
            overlap = max(0, min(end,next_time)-max(begin,timestamp))
            if overlap:
                f=nodes[ident]
                category = classify(f['functionName'], f['url'])
                funcs[category] += overlap
                if category[0] == 'Wait':
                    chain = []
                    current = ident
                    while current in parents and len(chain) < 22:
                        chain.append(nodes[current]['functionName'])
                        current = parents[current]
                    waits[' <- '.join(chain)] += overlap
            timestamp=next_time
        out[phase]=summarize(funcs, .001, 'sampled elapsed ms')
        out[phase]['profile']=p.name
        out[phase]['wait_stacks_ms'] = {k: v / 1000 for k,v in waits.most_common(20)}
    if thread != "vcpu":
        return out
    events=[]
    for file in directory.glob('modules-*.jsonl'):
        events += [json.loads(s) for s in file.read_text().splitlines()]
    for event in events:
        event['us'] = event['wall_us'] - marks['wall_offset_us']
    for phase,(begin,end) in phases.items():
        before=collections.defaultdict(lambda: {'count':0,'compileMs':0,'instanceMs':0})
        after=collections.defaultdict(lambda: {'count':0,'compileMs':0,'instanceMs':0})
        for event in sorted(events,key=lambda e:e['us']):
            if event['event']!='module': continue
            if event['us'] < begin: before[event['threadId']]=event
            if event['us'] <= end: after[event['threadId']]=event
        out[phase]['constructors']={k:sum(v[k]-before[t][k] for t,v in after.items()) for k in ('count','compileMs','instanceMs')}
    return out


def native_profile(file, thread):
    text=file.read_text()
    graph=text.split('Call graph:',1)[1].split('Total number in stack',1)[0]
    nodes=[]; stack=[]
    for line in graph.splitlines():
        m=re.match(r'^([ +:!|]*)(\d+) (.+)$',line)
        if not m: continue
        indent=len(m[1]); count=int(m[2]); desc=m[3]
        while stack and stack[-1]['indent']>=indent: stack.pop()
        node={'indent':indent,'count':count,'self':count,'desc':desc,'parent':stack[-1] if stack else None}
        if stack: stack[-1]['self']-=count
        nodes.append(node);stack.append(node)
    roots=[n for n in nodes if n['parent'] is None]
    def root(n):
        while n['parent'] is not None:n=n['parent']
        return n
    markers = ('tcg_cpu_exec','cpu_exec_loop','cpu_tb_exec','rr_cpu_thread_fn') if thread == 'vcpu' else ('main_loop_wait',)
    cpu_roots={id(root(n)) for n in nodes if any(x in n['desc'] for x in markers)}
    if not cpu_roots:raise RuntimeError(f'no vCPU stack in {file}')
    funcs=collections.Counter()
    for n in nodes:
        if id(root(n)) not in cpu_roots or n['self']<=0:continue
        name=n['desc'].split('  (in ',1)[0]
        funcs[classify(name)]+=n['self']
    result=summarize(funcs,1,'samples (nominal 1 ms interval)')
    result['profile']=file.name
    result['threads']=[n['desc'] for n in roots if id(n) in cpu_roots]
    return result


def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('directories',nargs='+',type=Path)
    p.add_argument('--output',type=Path,required=True)
    p.add_argument('--thread',choices=['vcpu','mainloop','js-main'],default='vcpu')
    args=p.parse_args()
    result={}
    for directory in args.directories:
        marks=json.loads((directory/'run.json').read_text())
        if marks['profile']=='node':
            result[directory.name]=node_profile(directory,marks,args.thread)
        else:
            result[directory.name]={phase:native_profile(directory/f'native-{phase}.sample', args.thread) for phase in (('home',) if marks.get('stop_at') == 'home' else ('settled',))}
    args.output.write_text(json.dumps(result,indent=2)+'\n')
    print(args.output)


if __name__=='__main__':main()
