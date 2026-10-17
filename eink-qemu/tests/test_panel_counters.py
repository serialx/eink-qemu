"""The panels' "refreshes" and "refresh-ns" properties, without firmware.

Firmware-neutral metrics read these with qom-get: completed refreshes (DRF on
the UltraChip panels, master activation on the SSD1677) and their BUSY time,
counted since QEMU started.

    python3 -m unittest eink-qemu.tests.test_panel_counters
"""
import json
import os
from pathlib import Path
import select
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
RISCV = Path(os.environ.get('QEMU_RISCV32', ROOT / 'build/qemu-system-riscv32'))
XTENSA = Path(os.environ.get('QEMU_XTENSA', ROOT / 'build/qemu-system-xtensa'))
PANEL = '/machine/peripheral/panel'


class Qtest:
    def __init__(self, case, qemu, machine):
        self.case = case
        directory = tempfile.TemporaryDirectory()
        case.addCleanup(directory.cleanup)
        monitor = Path(directory.name) / 'qmp'
        for suffix in ['in', 'out']:
            os.mkfifo(f'{monitor}.{suffix}')
        self.qmp_in = os.fdopen(os.open(f'{monitor}.in', os.O_RDWR), 'w')
        self.qmp_out = os.fdopen(os.open(f'{monitor}.out', os.O_RDWR), 'rb', buffering=0)
        case.addCleanup(self.qmp_in.close)
        case.addCleanup(self.qmp_out.close)
        card = Path(directory.name) / 'sd.img'
        with card.open('wb') as out:
            out.truncate(64 * 1024 * 1024)
        self.err = tempfile.TemporaryFile()
        case.addCleanup(self.err.close)
        drive = ['-drive', f'file={card},if=sd,format=raw'] if qemu == RISCV else []
        self.proc = subprocess.Popen([
            str(qemu), '-L', str(ROOT / 'pc-bios'), '-machine', machine,
            '-accel', 'qtest', '-display', 'none', '-monitor', 'none',
            '-serial', 'none', '-nic', 'none', '-qtest', 'stdio', *drive,
            '-chardev', f'pipe,id=qmp,path={monitor}', '-mon', 'chardev=qmp,mode=control',
        ], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=self.err, text=True, bufsize=1)
        case.addCleanup(self.stop)
        case.assertTrue(select.select([self.qmp_out], [], [], 5)[0])
        case.assertIn('QMP', json.loads(self.qmp_out.readline()))
        self.qmp('qmp_capabilities')

    def stop(self):
        self.proc.kill()
        self.proc.wait()
        self.proc.stdin.close()
        self.proc.stdout.close()

    def qmp(self, command, **arguments):
        self.qmp_in.write(json.dumps({'execute': command, 'arguments': arguments}) + '\n')
        self.qmp_in.flush()
        while True:
            self.case.assertTrue(select.select([self.qmp_out], [], [], 5)[0])
            reply = json.loads(self.qmp_out.readline())
            if 'event' not in reply:
                self.case.assertIn('return', reply)
                return reply['return']

    def command(self, text):
        self.proc.stdin.write(text + '\n')
        self.proc.stdin.flush()
        self.case.assertTrue(select.select([self.proc.stdout], [], [], 5)[0], text)
        reply = self.proc.stdout.readline().strip().split()
        self.case.assertEqual(reply[:1], ['OK'], (text, reply))
        return reply[1:]

    def write(self, addr, value):
        self.command(f'writel {addr:#x} {value:#x}')

    def prop(self, name):
        return self.qmp('qom-get', path=PANEL, property=name)

    def counters(self):
        return self.prop('refreshes'), self.prop('refresh-ns')

    def step_until_idle(self, limit_ms=10000):
        elapsed = 0
        while self.prop('busy'):
            self.command('clock_step 1000000')
            elapsed += 1
            self.case.assertLess(elapsed, limit_ms)
        return elapsed

    # UltraChip panels: bit-banged command bytes on the panel's GPIO inputs.
    def pin(self, name, value):
        self.command(f'set_irq_in {PANEL} {name} 0 {value}')

    def uc_command(self, value):
        self.pin('dc', 0)
        for bit in range(7, -1, -1):
            self.pin('sclk', 0)
            self.pin('sda', (value >> bit) & 1)
            self.pin('sclk', 1)
        self.pin('sclk', 0)


class PanelCounters(unittest.TestCase):
    def check_ultrachip(self, qemu, machine, rst_cancels=True):
        q = Qtest(self, qemu, machine)
        q.pin('rst', 0)
        q.pin('rst', 1)
        q.pin('ssi-gpio-cs', 0)
        self.assertEqual(q.counters(), (0, 0))
        q.uc_command(0x04)              # PON: BUSY, but not a refresh
        q.step_until_idle()
        self.assertEqual(q.counters(), (0, 0))
        q.uc_command(0x12)              # DRF
        self.assertTrue(q.prop('busy'))
        self.assertEqual(q.counters(), (0, 0))
        ms = q.step_until_idle()
        refreshes, ns = q.counters()
        self.assertEqual(refreshes, 1)
        self.assertGreater(ns, 0)
        self.assertLessEqual(ns, ms * 1000000)
        self.assertGreater(ns, (ms - 1) * 1000000)
        # A refresh cut short by RST is not counted (the UC8253 model keeps
        # BUSY through RST, so its refresh completes); counts survive a reset.
        q.uc_command(0x12)
        q.pin('rst', 0)
        q.pin('rst', 1)
        q.step_until_idle()
        q.qmp('system_reset')
        self.assertEqual(q.counters(), (1, ns) if rst_cancels else (2, 2 * ns))

    @unittest.skipUnless(XTENSA.exists(), 'build qemu-system-xtensa first')
    def test_uc8279_68_on_x4pro(self):
        self.check_ultrachip(XTENSA, 'x4pro,panel=uc8279-68')

    @unittest.skipUnless(XTENSA.exists(), 'build qemu-system-xtensa first')
    def test_uc8179_on_x4pro(self):
        self.check_ultrachip(XTENSA, 'x4pro,panel=uc8179')

    @unittest.skipUnless(RISCV.exists(), 'build qemu-system-riscv32 first')
    def test_uc8253_on_x3(self):
        self.check_ultrachip(RISCV, 'x3', rst_cancels=False)

    @unittest.skipUnless(RISCV.exists(), 'build qemu-system-riscv32 first')
    def test_ssd1677_on_x4(self):
        q = Qtest(self, RISCV, 'x4')
        gpio, spi = 0x60004000, 0x60024000
        pins = (1 << 21) | (1 << 4) | (1 << 5)
        q.write(gpio + 0x04, pins)
        q.write(gpio + 0x20, pins)
        q.write(gpio + 0x0c, 1 << 21)
        q.write(spi + 0x10, 1 << 27)

        def send(value, data=False):
            q.write(gpio + (0x08 if data else 0x0c), 1 << 4)    # DC: low a command, high data
            q.write(spi + 0x98, value)
            q.write(spi + 0x1c, 7)
            q.write(spi, 1 << 24)
            q.command('clock_step 10000')

        send(0x12)                          # SWRESET: BUSY, not a refresh
        q.command('clock_step 3000000')
        self.assertEqual(q.counters(), (0, 0))
        send(0x20)                          # master activation
        self.assertEqual(q.counters(), (0, 0))
        q.command('clock_step 400000000')
        refreshes, ns = q.counters()
        self.assertEqual(refreshes, 1)
        # "busy-ms" is 300, on a millisecond deadline.
        self.assertGreater(ns, 299000000)
        self.assertLessEqual(ns, 300000000)
        # Update control 2 decides what an activation does: switching the
        # clock and analog supply on or off, or loading a LUT, is no refresh.
        for control in (0xc0, 0x03, 0x91):
            send(0x22)
            send(control, data=True)
            send(0x20)
            q.command('clock_step 400000000')
            self.assertEqual(q.counters(), (1, ns), hex(control))
        send(0x22)
        send(0xf7, data=True)               # a full update, display stage included
        send(0x20)
        q.command('clock_step 400000000')
        self.assertEqual(q.counters()[0], 2)


if __name__ == '__main__':
    unittest.main()
