"""Check MMIO replay, learned TB boundaries and self-modifying code.

Set RISCV32_CC to a RISC-V bare-metal GCC; QEMU_RISCV32 overrides the emulator.
"""
import os
from pathlib import Path
import platform
import shutil
import struct
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
QEMU = Path(os.environ.get('QEMU_RISCV32', ROOT / 'build/qemu-system-riscv32'))
CC = shutil.which(os.environ.get('RISCV32_CC', 'riscv32-esp-elf-gcc'))


@unittest.skipUnless(QEMU.exists() and CC, 'requires native QEMU and RISCV32_CC')
class IcountIO(unittest.TestCase):
    def test_repeated_io_and_code_invalidation(self):
        with tempfile.TemporaryDirectory() as directory:
            elf = Path(directory) / 'io.elf'
            trace = Path(directory) / 'io.trace'
            subprocess.run([CC, '-march=rv32imc_zifencei', '-mabi=ilp32',
                            '-nostdlib', '-Wl,-Ttext=0x80000000', '-Wl,--no-relax',
                            '-o', str(elf), str(Path(__file__).with_name('icount-io.S'))],
                           check=True, capture_output=True)
            command = [str(QEMU), '-M', 'virt', '-bios', 'none', '-kernel', str(elf),
                       '-icount', 'shift=0,sleep=off', '-nographic']
            normal = subprocess.run(command + ['-d', 'exec', '-D', str(trace)],
                                    check=True, capture_output=True, timeout=30)
            single = subprocess.run(command + ['-accel', 'tcg,one-insn-per-tb=on'],
                                    check=True, capture_output=True, timeout=30)
            # UART LSR reads return 0x60; every read must contribute exactly once.
            self.assertEqual(normal.stdout, struct.pack('<Ii', 2000 * 0x60, -2000 * 0x60))
            self.assertEqual(normal.stdout, single.stdout)
            if platform.machine() in ('x86_64', 'AMD64'):
                # Warm loops must not take an I/O exception on every iteration.
                self.assertLess(trace.read_text().count('cpu_io_recompile:'), 20)


if __name__ == '__main__':
    unittest.main()
