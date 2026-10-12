"""Execute real Xtensa code to check cost accounting and clock transitions."""
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import time
import unittest

ROOT = Path(__file__).resolve().parents[2]
QEMU = Path(os.environ.get('QEMU_XTENSA', ROOT / 'build/qemu-system-xtensa'))
CC = Path(os.environ.get('XTENSA_CC', Path.home() / '.platformio/packages/toolchain-xtensa-esp-elf/bin/xtensa-esp32s3-elf-gcc'))


@unittest.skipUnless(QEMU.exists() and CC.exists(), 'requires Xtensa QEMU and compiler')
class XtensaCosts(unittest.TestCase):
    def execute(self, operations, props=(), clock=False, single=False, setup=(), machine="esp32s3",
                globals=(), iterations=10000):
        with tempfile.TemporaryDirectory() as tmp:
            src = Path(tmp) / 'probe.S'
            elf = Path(tmp) / 'probe.elf'
            code = ['.text', '.global _start', '_start:',
                    'movi a1, 0x3fce0000', 'movi a2, 0', 'wsr a2, PS', 'rsync',
                    'movi a2, 1', 'wsr a2, CPENABLE', 'rsync',
                    'movi a8, 0x3fc90000', 'movi a9, 0x60023004',
                    'movi a6, 0x3fc91000', 'movi a7, 0x60023044']
            code += list(setup)
            for i, op in enumerate(operations):
                if clock:
                    # Same function/TBs at 240, 160, 80 and XTAL 40 MHz.
                    code += ['movi a2, 0x600c0010', f'movi a3, {max(0,2-i)}',
                             's32i a3, a2, 0', 'movi a3, 0x28400' if i<3 else 'movi a3, 0x28000',
                             's32i a3, a2, 0x50', 'memw']
                code += ['movi a2, 0x40000000', 's32i a2, a9, 0', 'memw',
                         'l32i a4, a9, 0x40', f'movi a10, {iterations}', 'movi a11, 17', 'movi a12, 3']
                if clock:
                    code += ['call0 common_loop']
                else:
                    code += [f'loop{i}:', op, 'addi a10, a10, -1', f'bnez a10, loop{i}']
                code += ['movi a2, 0x40000000', 's32i a2, a9, 0', 'memw',
                         'l32i a5, a9, 0x40', 'sub a5, a5, a4', f's32i a5, a8, {4*i}']
            code += ['movi a2, 1', f's32i a2, a8, {4*len(operations)}', 'done: j done',
                     'common_loop: nop', 'addi a10, a10, -1', 'bnez a10, common_loop', 'ret',
                     'callee: ret']
            src.write_text('\n'.join(code)+'\n')
            subprocess.run([str(CC), '-nostdlib', '-Wl,-Ttext=0x40380000',
                            '-Wl,-e,_start', '-Wl,--no-relax', '-o', str(elf), str(src)],
                           check=True, capture_output=True)
            cmd=[str(QEMU), '-L', str(ROOT/'pc-bios'), '-M', machine, '-kernel', str(elf),
                 '-icount', 'shift=0,sleep=off', '-display', 'none', '-serial', 'none',
                 '-monitor', 'none', '-nic', 'none', '-qmp', 'stdio']
            for prop in props: cmd += ['-global', 'xtensa-cpu.'+prop]
            for prop in globals: cmd += ['-global', prop]
            if single: cmd += ['-accel', 'tcg,one-insn-per-tb=on']
            with tempfile.TemporaryFile() as err:
                p=subprocess.Popen(cmd,stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=err,text=True)
                try:
                    greeting=p.stdout.readline()
                    if not greeting:
                        err.seek(0); self.fail(err.read().decode())
                    def qmp(command, **args):
                        p.stdin.write(json.dumps({'execute':command,'arguments':args})+'\n');p.stdin.flush()
                        while True:
                            line=p.stdout.readline()
                            if not line:
                                err.seek(0);self.fail(err.read().decode())
                            reply=json.loads(line)
                            if 'event' not in reply:
                                self.assertIn('return', reply);return reply['return']
                    qmp('qmp_capabilities')
                    end=time.monotonic()+10
                    while time.monotonic()<end:
                        reply=qmp('human-monitor-command', **{'command-line':f'xp /{len(operations)+1}wx 0x3fc90000'})
                        values=[int(x,16) for x in re.findall(r'0x([0-9a-f]{8})',reply)]
                        if values[-1]==1:return values[:-1]
                        time.sleep(0.01)
                    self.fail(reply)
                finally:
                    p.terminate();p.wait(timeout=5);p.stdin.close();p.stdout.close()

    def test_instruction_classes_and_mmio_refunds(self):
        ops=['nop','l32i a11, a6, 0','s32i a11, a6, 0','mull a11, a11, a12',
             'quou a11, a11, a12','call0 callee','add.s f0, f0, f0',
             'l32i a11, a7, 0','s32i a11, a7, 0','madd.s f0, f1, f2']
        baseline=self.execute(ops)
        for i,n in enumerate(baseline): self.assertAlmostEqual(n,800 if i==5 else 480,delta=2)
        for prop,indices in [('load',[1,7]),('store',[2,8]),('mul',[3]),('div',[4]),('call',[5]),('fpu',[6]),('fma',[9]),('branch',list(range(10)))]:
            with self.subTest(prop=prop):
                values=self.execute(ops,[f'cost-{prop}=3'])
                for i,(a,b) in enumerate(zip(values,baseline)):
                    # call0 and ret both receive the call/return cost.
                    expected=960 if prop=='call' and i==5 else 480
                    # The assembler expands the far call into l32r/callx0.
                    affected = i in indices or (prop=='load' and i==5)
                    self.assertAlmostEqual(a-b,expected if affected else 0,delta=3)

    def test_cached_blocks_follow_clock_changes(self):
        values=self.execute(['nop']*4,['cost-clock-scaling=on','cost-sram-ns=4'],clock=True)
        for a,scale in zip(values,[1,1.5,3,6]):self.assertAlmostEqual(a/values[0],scale,delta=.005)

    def test_single_instruction_blocks_and_cost_bounds(self):
        for single in [False,True]:
            values=self.execute(['quou a11, a11, a12'],['cost-sram-ns=255','cost-div=255'],single=single)
            self.assertAlmostEqual(values[0],163200,delta=100)
