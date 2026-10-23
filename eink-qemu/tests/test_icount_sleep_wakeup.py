"""Host scheduling delays must not skip the timer that wakes a fixed-rate vCPU."""
import os
from pathlib import Path
import select
import shutil
import signal
import struct
import subprocess
import tempfile
import time
import unittest

ROOT = Path(__file__).resolve().parents[2]
QEMU = Path(os.environ.get('QEMU_RISCV32', ROOT / 'build/qemu-system-riscv32'))
CC = shutil.which(os.environ.get('RISCV32_CC', 'riscv32-esp-elf-gcc'))


@unittest.skipUnless(os.name == 'posix' and QEMU.exists() and CC,
                     'requires POSIX signals, native QEMU and RISCV32_CC')
class IcountSleepWakeup(unittest.TestCase):
    def test_delayed_host_wakes_at_first_guest_deadline(self):
        with tempfile.TemporaryDirectory() as directory:
            elf = Path(directory) / 'wakeup.elf'
            subprocess.run([CC, '-march=rv32imc_zicsr', '-mabi=ilp32',
                            '-nostdlib', '-Wl,-Ttext=0x80000000', '-Wl,--no-relax',
                            '-o', str(elf),
                            str(Path(__file__).with_name('icount-sleep-wakeup.S'))],
                           check=True, capture_output=True)
            command = [str(QEMU), '-M', 'virt', '-bios', 'none', '-kernel', str(elf),
                       '-icount', 'shift=0,sleep=on', '-nographic', '-monitor', 'none']
            process = subprocess.Popen(command, stdout=subprocess.PIPE,
                                       stderr=subprocess.PIPE)
            try:
                self.assertTrue(select.select([process.stdout], [], [], 10)[0],
                                'guest did not arm its timer')
                self.assertEqual(process.stdout.read(1), b'R')
                # Give the few remaining guest instructions time to reach WFI.
                time.sleep(.03)
                process.send_signal(signal.SIGSTOP)
                try:
                    time.sleep(1)
                finally:
                    process.send_signal(signal.SIGCONT)
                output, errors = process.communicate(timeout=10)
                self.assertEqual(process.returncode, 0, errors.decode())
                self.assertEqual(len(output), 4)
                ticks, = struct.unpack('<I', output)
                self.assertGreaterEqual(ticks, 5_000_000)
                self.assertLess(ticks, 6_000_000,
                                'host oversleep skipped the pending 500 ms timer')
            finally:
                if process.poll() is None:
                    process.send_signal(signal.SIGCONT)
                    process.kill()
                process.communicate()


if __name__ == '__main__':
    unittest.main()
