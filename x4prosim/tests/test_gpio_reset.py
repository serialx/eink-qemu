"""ESP32 GPIO reset releases every SPI chip select, without firmware blobs.

QEMU_RISCV32=build/native/qemu-system-riscv32 python3 -m unittest x4prosim.tests.test_gpio_reset
"""
import json
import os
from pathlib import Path
import select
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
QEMU = Path(os.environ.get('QEMU_RISCV32', ROOT / 'build/qemu-system-riscv32'))


@unittest.skipUnless(QEMU.exists(), 'build qemu-system-riscv32 first')
class GPIOReset(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.err = tempfile.TemporaryFile()
        self.addCleanup(self.err.close)
        monitor = Path(self.directory.name) / 'qmp'
        for suffix in ['in', 'out']:
            os.mkfifo(f'{monitor}.{suffix}')
        self.qmp_in = os.fdopen(os.open(f'{monitor}.in', os.O_RDWR), 'w')
        self.qmp_out = os.fdopen(os.open(f'{monitor}.out', os.O_RDWR), 'rb', buffering=0)
        self.addCleanup(self.qmp_in.close)
        self.addCleanup(self.qmp_out.close)
        card = Path(self.directory.name) / 'sd.img'
        with card.open('wb') as out:
            out.truncate(64 * 1024 * 1024)
        self.proc = subprocess.Popen([
            str(QEMU), '-L', str(ROOT / 'pc-bios'), '-machine', 'x3',
            '-accel', 'qtest', '-display', 'none', '-monitor', 'none',
            '-serial', 'none', '-nic', 'none', '-qtest', 'stdio',
            '-drive', f'file={card},if=sd,format=raw',
            '-chardev', f'pipe,id=qmp,path={monitor}', '-mon', 'chardev=qmp,mode=control',
        ], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=self.err,
            text=True, bufsize=1)
        self.addCleanup(self.stop)
        if not select.select([self.qmp_out], [], [], 3)[0]:
            self.err.seek(0)
            self.fail(self.err.read().decode())
        self.assertIn('QMP', json.loads(self.qmp_out.readline()))
        self.qmp('qmp_capabilities')

    def stop(self):
        self.proc.kill()
        self.proc.wait()
        self.proc.stdin.close()
        self.proc.stdout.close()

    def qmp(self, command, **arguments):
        self.qmp_in.write(json.dumps(dict(execute=command, arguments=arguments)) + '\n')
        self.qmp_in.flush()
        result = None
        reset_done = command != "system_reset"
        while True:
            self.assertTrue(select.select([self.qmp_out], [], [], 5)[0])
            reply = json.loads(self.qmp_out.readline())
            if reply.get('event') == 'RESET':
                reset_done = True
            elif 'event' not in reply:
                self.assertIn('return', reply)
                result = reply['return']
            if result is not None and reset_done:
                return result

    def command(self, command):
        self.proc.stdin.write(command + '\n')
        self.proc.stdin.flush()
        self.assertTrue(select.select([self.proc.stdout], [], [], 5)[0], command)
        response = self.proc.stdout.readline().strip()
        self.assertTrue(response.startswith('OK'), (command, response))
        return response.split()[1:]

    def write(self, addr, value):
        self.command(f'writel {addr:#x} {value:#x}')

    def read(self, addr):
        return int(self.command(f'readl {addr:#x}')[0], 0)

    def transfer(self, value):
        spi = 0x60024000
        self.write(spi + 0x10, (1 << 27) | (1 << 28))
        self.write(spi + 0x98, value)
        self.write(spi + 0x1c, 7)
        self.write(spi, 1 << 24)
        self.command('clock_step 1000000')
        return self.read(spi + 0x98) & 0xff

    def sd_command(self, command, argument=0):
        for byte in [0x40 | command, *argument.to_bytes(4, 'big'), 0x95]:
            self.transfer(byte)
        self.assertEqual(self.transfer(0xff), 0xff)
        return self.transfer(0xff)

    def test_reset_releases_unconfigured_sd_chip_select(self):
        gpio = 0x60004000

        for _ in range(2):
            # TRMNL only drives the panel CS. The unused SD CS must float high.
            self.write(gpio + 0x24, (1 << 21) | (1 << 4) | (1 << 5))
            self.write(gpio + 0x08, (1 << 21) | (1 << 4) | (1 << 5))
            self.assertEqual(self.transfer(0xff), 0)
            # Explicitly selecting SD still enables normal SPI responses.
            self.write(gpio + 0x24, 1 << 12)
            self.assertEqual(self.transfer(0xff), 0xff)
            self.assertEqual(self.sd_command(0), 1)  # CMD0 R1: idle
            self.qmp('system_reset')

    def test_selected_sd_block_roundtrip(self):
        gpio = 0x60004000
        self.write(gpio + 0x24, 1 << 12)
        self.assertEqual(self.sd_command(0), 1)
        self.assertEqual(self.sd_command(1), 0)
        self.assertEqual(self.sd_command(24), 0)
        data = bytes((i * 37 + 11) & 255 for i in range(512))
        for byte in [0xfe, *data, 0xff, 0xff]:
            self.transfer(byte)
        self.assertEqual(self.transfer(0xff), 5)  # data accepted
        self.assertEqual(self.transfer(0xff), 0xff)
        self.assertEqual(self.sd_command(17), 0)
        self.assertEqual(self.transfer(0xff), 0xff)
        self.assertEqual(self.transfer(0xff), 0xfe)
        self.assertEqual(bytes(self.transfer(0xff) for _ in range(512)), data)
