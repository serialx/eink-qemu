"""Picco board wiring and peripheral transactions without firmware blobs."""
import os
import json
from pathlib import Path
import re
import select
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
QEMU = Path(os.environ.get('QEMU_XTENSA', ROOT / 'build/qemu-system-xtensa'))


@unittest.skipUnless(QEMU.exists(), 'build qemu-system-xtensa first')
class PiccoBoard(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        flash = Path(self.directory.name) / 'flash.bin'
        data = bytearray(b'\xff' * 0x1000000)
        data[0x10000:0x10008] = bytes.fromhex('123456789abcdef0')
        flash.write_bytes(data)
        self.err = tempfile.TemporaryFile()
        self.addCleanup(self.err.close)
        monitor = Path(self.directory.name) / 'qmp'
        for suffix in ['in', 'out']:
            os.mkfifo(f'{monitor}.{suffix}')
        self.qmp_in = os.fdopen(os.open(f'{monitor}.in', os.O_RDWR), 'w')
        # Keep select() and readline() on the same buffer: RESET events and
        # their command reply can arrive together in a single pipe write.
        self.qmp_out = os.fdopen(os.open(f'{monitor}.out', os.O_RDWR), 'rb', buffering=0)
        self.addCleanup(self.qmp_in.close)
        self.addCleanup(self.qmp_out.close)
        self.proc = subprocess.Popen([
            str(QEMU), '-machine', 'picco', '-accel', 'qtest',
            '-display', 'none', '-monitor', 'none', '-serial', 'none',
            '-nic', 'none', '-qtest', 'stdio',
            '-chardev', f'pipe,id=qmp,path={monitor}',
            '-mon', 'chardev=qmp,mode=control',
            '-drive', f'file={flash},if=mtd,format=raw',
        ], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=self.err,
            text=True, bufsize=1)
        self.addCleanup(self.stop)
        if not select.select([self.qmp_out], [], [], 5)[0]:
            self.err.seek(0)
            self.fail(self.err.read().decode())
        self.assertIn('QMP', json.loads(self.qmp_out.readline()))
        self.qmp('qmp_capabilities')

    def qmp(self, command, **arguments):
        self.qmp_in.write(json.dumps({'execute': command,
                                     'arguments': arguments}) + '\n')
        self.qmp_in.flush()
        while True:
            self.assertTrue(select.select([self.qmp_out], [], [], 5)[0])
            reply = json.loads(self.qmp_out.readline())
            if 'event' not in reply:
                self.assertIn('return', reply)
                return reply['return']

    def cpu_interrupts(self):
        regs = self.qmp('human-monitor-command',
                        **{'command-line': 'info registers'})
        return int(re.search(r'INTERRUPT=([0-9a-f]+)', regs)[1], 16)

    def stop(self):
        self.proc.terminate()
        try:
            self.proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.proc.kill()
            self.proc.wait()
        self.proc.stdin.close()
        self.proc.stdout.close()

    def command(self, command):
        self.proc.stdin.write(command + '\n')
        self.proc.stdin.flush()
        self.assertTrue(select.select([self.proc.stdout], [], [], 5)[0], command)
        response = self.proc.stdout.readline().strip()
        self.assertTrue(response.startswith('OK'), (command, response))
        return response.split()[1:]

    def read(self, addr):
        return int(self.command(f'readl {addr:#x}')[0], 0)

    def write(self, addr, value):
        self.command(f'writel {addr:#x} {value:#x}')

    def test_board_identity_and_keys(self):
        self.assertEqual(self.qmp('qom-get', path='/machine', property='type'), 'picco-machine')
        self.assertEqual(self.read(0x6000403c) & (1 << 15), 0)
        pins = (1 << 21) | (1 << 9)
        self.assertEqual(self.read(0x6000403c) & pins, pins)
        self.qmp('qom-set', path='/machine/picco-keys', property='up', value=True)
        self.assertEqual(self.read(0x6000403c) & (1 << 21), 0)
        self.qmp('qom-set', path='/machine/picco-keys', property='up', value=False)
        self.assertNotEqual(self.read(0x6000403c) & (1 << 21), 0)

    def i2c(self, addr, data, count=0):
        base = 0x60013000
        self.write(base + 0x18, 3 << 12)
        self.write(base + 0x18, 0)
        self.write(base + 0x24, 0xffffffff)
        for byte in [addr << 1, *data]:
            self.write(base + 0x1c, byte)
        cmds = [6 << 11, (1 << 11) | (1 << 8) | (len(data) + 1)]
        if count:
            self.write(base + 0x1c, (addr << 1) | 1)
            cmds += [6 << 11, (1 << 11) | (1 << 8) | 1,
                     (3 << 11) | (1 << 10) | count]
        cmds += [2 << 11]
        for index, cmd in enumerate(cmds):
            self.write(base + 0x58 + index * 4, cmd)
        self.write(base + 4, 1 << 5)
        self.assertEqual(self.read(base + 0x20) & ((1 << 10) | (1 << 7)), 1 << 7)
        return bytes(self.read(base + 0x1c) for _ in range(count))

    def test_power_and_motion_devices(self):
        self.assertEqual(self.i2c(0x03, [0x0b], 1), b'\x00')
        self.assertEqual(self.i2c(0x03, [0x08], 1)[0] & 2, 2)
        self.i2c(0x03, [0x04, 0xaa])
        self.assertEqual(self.i2c(0x03, [0x04], 1), b'\xaa')
        self.i2c(0x03, [0x02, 0x80])
        self.assertEqual(self.i2c(0x03, [0x04], 1), b'\xa3')
        self.i2c(0x03, [0x0b, 0xff])
        self.assertEqual(self.i2c(0x03, [0x0b], 1), b'\x00')
        self.assertEqual(self.i2c(0x6a, [0], 1), b'\x05')
        self.assertEqual(self.i2c(0x51, [0], 1), b'\x00')

    def test_display_id_on_shared_gpio47(self):
        gpio = 0x60004000
        low = (1 << 16) | (1 << 17) | (1 << 18)
        high = (1 << 15) | (1 << 16)
        self.write(gpio + 4, low)
        self.write(gpio + 0x10, 0)
        self.write(gpio + 0x20, low)
        self.write(gpio + 0x30, high)
        self.write(gpio + 0x0c, (1 << 17) | (1 << 18))
        for bit in range(7, -1, -1):
            self.write(gpio + (0x14 if (0x2e >> bit) & 1 else 0x18), 1 << 15)
            self.write(gpio + 0x14, 1 << 16)
            self.write(gpio + 0x18, 1 << 16)
        self.write(gpio + 0x34, 1 << 15)  # turn shared SDA around for reading
        self.write(gpio + 8, 1 << 17)
        result = bytearray()
        for _ in range(10):
            value = 0
            for bit in range(8):
                value = (value << 1) | ((self.read(gpio + 0x40) >> 15) & 1)
                self.write(gpio + 0x14, 1 << 16)
                self.write(gpio + 0x18, 1 << 16)
            result.append(value)
        self.assertEqual(result, bytes.fromhex('ca fe 00 16 77 00 08 01 00 4e'))
