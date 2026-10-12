"""Run cold/hot indirect guest branches through native and Node QEMU.

Requires RISCV32_CC, a native build, and WASM_BUILD_DIR (default build-wasm-32limit).
"""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
QEMU = Path(os.environ.get('QEMU_RISCV32', ROOT / 'build/qemu-system-riscv32'))
WASM = Path(os.environ.get('WASM_BUILD_DIR', ROOT / 'build-wasm-32limit'))
CC = shutil.which(os.environ.get('RISCV32_CC', 'riscv32-esp-elf-gcc'))
NODE = shutil.which(os.environ.get('NODE', 'node'))


@unittest.skipUnless(QEMU.exists() and (WASM / 'qemu-system-riscv32.js').exists()
                     and CC and NODE, 'requires native/Wasm QEMU, Node and RISCV32_CC')
class WasmIndirect(unittest.TestCase):
    def test_cold_and_hot_indirect_branches(self):
        with tempfile.TemporaryDirectory() as directory:
            elf = Path(directory) / 'indirect.elf'
            subprocess.run([CC, '-march=rv32imc_zicsr', '-mabi=ilp32', '-nostdlib',
                            '-Wl,-Ttext=0x80000000', '-Wl,--no-relax', '-o', str(elf),
                            str(Path(__file__).with_name('wasm-indirect.S'))],
                           check=True, capture_output=True)
            # Include the fetch fault and faulting PC for a PMP-denied target.
            expected = b'00000140\n00004e20\n00000001\n60000000\n'
            for wasm in (False, True):
                with self.subTest(wasm=wasm):
                    prefix = '/host' if wasm else ''
                    command = ([NODE, str(WASM / 'qemu-system-riscv32.js')] if wasm
                               else [str(QEMU)])
                    command += ['-M', 'virt', '-m', '16M', '-bios', 'none',
                                '-kernel', prefix + str(elf),
                                '-icount', 'shift=0,sleep=off', '-nographic']
                    result = subprocess.run(command, check=True, capture_output=True,
                                            timeout=60)
                    self.assertEqual(result.stdout, expected)


if __name__ == '__main__':
    unittest.main()
