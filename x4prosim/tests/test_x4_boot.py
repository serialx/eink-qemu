"""Xteink X4 board wiring and SSD1677 protocol, without firmware blobs.

QEMU_RISCV32=build/native/qemu-system-riscv32 python3 -m unittest x4prosim.tests.test_x4_boot
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
class X4Panel(unittest.TestCase):
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
            str(QEMU), '-L', str(ROOT / 'pc-bios'), '-machine', 'x4',
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

    def test_ssd1677_pixels_and_busy_polarity(self):
        gpio, spi = 0x60004000, 0x60024000
        pins = (1 << 21) | (1 << 4) | (1 << 5)
        self.write(gpio + 0x04, pins)
        self.write(gpio + 0x20, pins)
        self.write(gpio + 0x0c, 1 << 21)
        self.write(spi + 0x10, 1 << 27)

        def send(value, data=False):
            self.write(gpio + (0x08 if data else 0x0c), 1 << 4)
            self.write(spi + 0x98, value)
            self.write(spi + 0x1c, 7)
            self.write(spi, 1 << 24)
            self.command('clock_step 10000')

        def cmd(value, *data):
            send(value)
            for byte in data:
                send(byte, True)

        self.assertEqual(self.read(gpio + 0x3c) & (1 << 6), 0)
        cmd(0x12)  # SSD1677 SWRESET, not UC8253 display refresh.
        self.assertNotEqual(self.read(gpio + 0x3c) & (1 << 6), 0)
        self.command('clock_step 3000000')
        self.assertEqual(self.read(gpio + 0x3c) & (1 << 6), 0)
        # TRMNL's bb_epaper uses pixel-addressed RAM windows and X decrement.
        cmd(0x11, 2)
        cmd(0x44, 0x1f, 3, 0, 0)
        cmd(0x45, 0, 0, 0xdf, 1)
        cmd(0x4e, 0x1f, 3)
        cmd(0x4f, 0, 0)
        cmd(0x24, 0xfe)
        cmd(0x22, 0xf7)
        cmd(0x20)
        self.command('clock_step 400000000')
        shot = Path(self.directory.name) / 'panel.ppm'
        self.qmp('screendump', filename=str(shot), format='ppm', device='panel')
        header, size, maximum, pixels = shot.read_bytes().split(b'\n', 3)
        self.assertEqual((header, size, maximum), (b'P6', b'480 800', b'255'))
        self.assertEqual(pixels[(792 * 480) * 3], 0x10)
        self.assertEqual(pixels[:3], b'\xf0' * 3)
        self.assertEqual(sum(v == 0x10 for v in pixels), 3)

        # Exercise both address directions, byte boundaries, and row wrapping.
        for entry in range(4):
            with self.subTest(entry=entry):
                xinc, yinc = entry & 1, entry & 2
                xs, xe = (0, 15) if xinc else (799, 784)
                ys, ye = (0, 1) if yinc else (479, 478)
                cmd(0x46, 0xff)  # erase the BW plane
                self.command('clock_step 6000000')
                cmd(0x11, entry)
                cmd(0x44, xs & 255, xs >> 8, xe & 255, xe >> 8)
                cmd(0x45, ys & 255, ys >> 8, ye & 255, ye >> 8)
                cmd(0x4e, xs & 255, xs >> 8)
                cmd(0x4f, ys & 255, ys >> 8)
                cmd(0x24, 0x7f, 0xfe, 0xbf, 0xfd)
                cmd(0x20)
                self.command('clock_step 400000000')
                self.qmp('screendump', filename=str(shot), format='ppm', device='panel')
                pixels = shot.read_bytes().split(b'\n', 3)[3]
                expected = {(ys, xs), (ys, xs + (15 if xinc else -15)),
                            (ye, xs + (1 if xinc else -1)),
                            (ye, xs + (14 if xinc else -14))}
                actual = {(i % 480, i // 480) for i in range(480 * 800)
                          if pixels[i * 3] == 0x10}
                self.assertEqual(actual, expected)
