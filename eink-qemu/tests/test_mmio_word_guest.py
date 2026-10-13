"""Run live-word polling in guest code, including invalidation and a timer edge."""
import json
import os
from pathlib import Path
import re
import selectors
import shutil
import subprocess
import tempfile
import time
import unittest

ROOT = Path(__file__).resolve().parents[2]
CC = shutil.which(os.environ.get('RISCV32_CC', 'riscv32-esp-elf-gcc'))
QEMU = Path(os.environ.get('QEMU_RISCV32', ROOT / 'build/qemu-system-riscv32'))
WASM = Path(os.environ.get('WASM_BUILD_DIR', ROOT / 'build-wasm-32limit'))
NODE = shutil.which(os.environ.get('NODE', 'node'))


@unittest.skipUnless(CC and QEMU.exists(), 'requires native QEMU and RISCV32_CC')
class MMIOWordGuest(unittest.TestCase):
    def execute(self, elf, wasm=False, single=False):
        prefix = '/host' if wasm else ''
        command = ([NODE, str(WASM / 'qemu-system-riscv32.js')] if wasm else [str(QEMU)])
        command += ['-L', prefix + str(ROOT / 'pc-bios'), '-M', 'x3',
                    '-kernel', prefix + str(elf), '-icount', 'shift=0,sleep=off',
                    '-display', 'none', '-serial', 'none', '-monitor', 'none',
                    '-nic', 'none', '-qmp', 'stdio']
        if single:
            command += ['-accel', 'tcg,one-insn-per-tb=on']
        with tempfile.TemporaryFile() as err:
            p = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                 stderr=err)
            selector = selectors.DefaultSelector()
            selector.register(p.stdout, selectors.EVENT_READ)
            pending = bytearray()
            deadline = time.monotonic() + 30
            def reply():
                while time.monotonic() < deadline:
                    if b'\n' in pending:
                        line, _, tail = pending.partition(b'\n')
                        pending[:] = tail
                        try:
                            value = json.loads(line)
                        except json.JSONDecodeError:
                            continue
                        if 'event' not in value:
                            return value
                    if selector.select(.1):
                        chunk = os.read(p.stdout.fileno(), 65536)
                        if not chunk:
                            break
                        pending.extend(chunk)
                err.seek(0)
                self.fail('QMP timeout/exit: ' + err.read().decode(errors='replace'))
            def qmp(name, **args):
                p.stdin.write(json.dumps({'execute': name, 'arguments': args}).encode() + b'\n')
                p.stdin.flush()
                result = reply()
                self.assertIn('return', result)
                return result['return']
            try:
                self.assertIn('QMP', reply())
                qmp('qmp_capabilities')
                while time.monotonic() < deadline:
                    output = qmp('human-monitor-command', **{'command-line': 'xp /8wx 0x3fc90000'})
                    values = [int(x, 16) for x in re.findall(r'0x([0-9a-f]{8})', output)]
                    if len(values) == 8 and values[-1] == 0x1234abcd:
                        qmp('quit')
                        p.wait(timeout=10)
                        self.assertEqual(p.returncode, 0)
                        return values
                    time.sleep(.01)
                self.fail('guest did not finish: ' + output)
            finally:
                if p.poll() is None:
                    p.kill()
                    p.wait(timeout=5)
                p.stdin.close()
                p.stdout.close()
                selector.close()

    def test_live_word_pmp_invalidation_and_icount_boundary(self):
        with tempfile.TemporaryDirectory() as directory:
            elf = Path(directory) / 'word.elf'
            subprocess.run([CC, '-march=rv32im_zicsr', '-mabi=ilp32', '-nostdlib',
                            '-Wl,-Ttext=0x40380000', '-Wl,-e,_start', '-Wl,--no-relax',
                            '-o', str(elf), str(Path(__file__).with_name('mmio-word.S'))],
                           check=True, capture_output=True)
            normal = self.execute(elf)
            self.assertEqual(normal[:3], [5000 * n for n in (0x123, 0x45, 0x89)])
            self.assertGreater(normal[3], 1)
            self.assertEqual(normal[4], 1 << 12)
            self.assertEqual(normal[5:7], [5, 0x76543210])
            self.assertEqual(normal, self.execute(elf, single=True))
            with self.subTest(wasm=True):
                if not NODE or not (WASM / 'qemu-system-riscv32.js').exists():
                    self.skipTest('Wasm comparison requires Node and the Wasm build')
                self.assertEqual(normal, self.execute(elf, wasm=True))


if __name__ == '__main__':
    unittest.main()
