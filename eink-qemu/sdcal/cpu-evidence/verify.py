#!/usr/bin/env python3
"""Check actual virtual time on RV32IMC, including MMIO unwind and short TBs.

Usage: verify.py path/to/qemu-system-riscv32 path/to/riscv32-esp-elf-gcc
No device, CrossPoint build, Python packages, or writable checkout required.
"""
import json
import re
import time
from pathlib import Path
import struct
import subprocess
import sys
import tempfile

qemu, compiler = map(lambda p: str(Path(p).resolve()), sys.argv[1:3])
source = Path(__file__).with_name('instruction-costs.S')
with tempfile.TemporaryDirectory(prefix='qemu-cpu-cost-') as tmp:
    elf = str(Path(tmp) / 'test.elf')
    subprocess.run([compiler, '-march=rv32imc', '-mabi=ilp32', '-nostdlib',
                    '-Wl,-Ttext=0x80000000', '-Wl,--no-relax',
                    '-o', elf, str(source)], check=True)
    cmd = [qemu, '-M', 'virt', '-bios', 'none', '-kernel', elf,
           '-icount', 'shift=0,sleep=off', '-nographic']
    rows = {}

    def run(name, args, expected, tolerance=1):
        result = subprocess.run(cmd + args, capture_output=True,
                                check=True, timeout=30)
        values = struct.unpack('<8I', result.stdout)
        assert all(abs(a - b) <= tolerance for a, b in zip(values, expected)), (
            name, values, expected, result.stderr.decode())
        rows[name] = values

    run('default', [], [300] * 8)
    # Each interval executes 10,000 (operation, decrement, branch) iterations.
    # virt's mtime tick is 100 ns; icount shift=0 makes each cost tick 1 ns.
    indices = {'load': [1, 6], 'store': [2, 7], 'mul': [3], 'div': [4, 5],
               'branch': list(range(8)), 'flash-ns': []}
    for prop, affected in indices.items():
        expected = [600 if i in affected else 300 for i in range(8)]
        run(prop, ['-global', f'riscv-cpu.cost-{prop}=3'], expected)
    run('base', ['-global', 'riscv-cpu.cost-sram-ns=3'], [900] * 8)
    run('single-insn-tbs', ['-accel', 'tcg,one-insn-per-tb=on',
                           '-global', 'riscv-cpu.cost-div=34'],
        [300, 300, 300, 300, 3700, 3700, 300, 300])
    args = []
    for prop in ['sram-ns', 'rom-ns', *indices]:
        args.extend(['-global', f'riscv-cpu.cost-{prop}=255'])
    run('maximum-costs', args, [102000] + [127500] * 7, tolerance=25)
    # Arrange a timer deadline inside a maximum-cost instruction. Execution
    # must progress and retain the full cost, including the six setup insns.
    deadline_source = Path(tmp) / 'deadline.S'
    deadline_source.write_text(source.read_text().replace(' lw t2, 0(s0)',
        ' lw t2, 0(s0)\n li t4, 0x2004000\n li t5, -1\n'
        ' sw t5, 4(t4)\n addi t5, t2, 100\n sw t5, 0(t4)\n'
        ' sw zero, 4(t4)'))
    subprocess.run([compiler, '-march=rv32imc', '-mabi=ilp32', '-nostdlib',
                    '-Wl,-Ttext=0x80000000', '-Wl,--no-relax',
                    '-o', elf, str(deadline_source)], check=True)
    trace = Path(tmp) / 'deadline.trace'
    run('deadline-overrun', args + ['-d', 'exec', '-D', str(trace)],
        [value + 23 for value in rows['maximum-costs']], tolerance=1)
    overrun_tbs = sum(bool(int(line.split(']')[0].rsplit('/', 1)[1], 16) &
                           0x10000) for line in trace.read_text().splitlines()
                      if line.startswith('Trace '))
    assert overrun_tbs > 0, 'deadline did not exercise CF_NOIRQ overrun path'
    rows['deadline_overrun_tbs'] = overrun_tbs
    # Reuse the same SRAM loop after changing the C3 clock from 160 to 10
    # MHz. This checks both the factor and invalidation of its cached TBs.
    clock_source = source.with_name('clock-scale.S')
    subprocess.run([compiler, '-march=rv32imc_zicsr',
                    '-mabi=ilp32', '-nostdlib',
                    '-Wl,-Ttext=0x40380000', '-Wl,-Tdata=0x3fc90000',
                    '-Wl,--no-relax', '-o', elf, str(clock_source)], check=True)
    with (Path(tmp) / 'clock.stderr').open('w') as err:
        process = subprocess.Popen([qemu, '-M', 'x3', '-kernel', elf,
            '-icount', 'shift=0,sleep=on',
            '-display', 'none', '-serial', 'null',
            '-monitor', 'none', '-qmp', 'stdio'], stdin=subprocess.PIPE,
            stdout=subprocess.PIPE, stderr=err, text=True, bufsize=1)
        try:
            json.loads(process.stdout.readline())

            def qmp(command):
                process.stdin.write(json.dumps(command) + '\n')
                process.stdin.flush()
                while True:
                    response = json.loads(process.stdout.readline())
                    if 'return' in response or 'error' in response:
                        return response

            qmp({'execute': 'qmp_capabilities'})
            deadline = time.monotonic() + 10
            while True:
                response = qmp({'execute': 'human-monitor-command',
                    'arguments': {'command-line': 'xp /3wx 0x3fc90000'}})
                fast, slow, done = [int(value, 16) for value in
                    re.findall(r'0x([0-9a-f]{8})', response['return'])]
                if done == 1:
                    break
                assert time.monotonic() < deadline, response
                time.sleep(0.05)
            assert fast > 0 and abs(slow / fast - 16) < 0.001, (fast, slow)
            rows['clock_scale'] = dict(fast_ticks=fast, slow_ticks=slow,
                                      ratio=slow / fast)
            qmp({'execute': 'quit'})
            process.wait(timeout=5)
        finally:
            if process.poll() is None:
                process.terminate()
                process.wait(timeout=5)
    print(json.dumps(rows, indent=2))
