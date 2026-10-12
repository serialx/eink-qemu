"""Exercise stock-firmware boot peripherals without proprietary firmware.

Build qemu-system-xtensa first, then run:
    python3 -m unittest eink-qemu.tests.test_x4pro_boot
"""
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
class StockBootPeripherals(unittest.TestCase):
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
            str(QEMU), '-machine', 'x4pro', '-accel', 'qtest',
            '-display', 'none', '-monitor', 'none', '-serial', 'none',
            '-nic', 'none', '-qtest', 'stdio',
            '-chardev', f'pipe,id=qmp,path={monitor}',
            '-mon', 'chardev=qmp,mode=control',
            '-drive', f'file={flash},if=mtd,format=raw',
        ], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=self.err,
            text=True, bufsize=1)
        self.addCleanup(self.stop)
        self.assertTrue(select.select([self.qmp_out], [], [], 5)[0])
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

    def systimer_value(self):
        self.write(0x60023004, 1 << 30)
        return self.read(0x60023044) | (self.read(0x60023040) << 32)

    def test_systimer_polling_preserves_fractional_ticks(self):
        for _ in range(100):
            self.command('clock_step 32')
            self.systimer_value()
        self.assertEqual(self.systimer_value(), 51)
        self.command('clock_step 50')
        self.assertEqual(self.systimer_value(), 52)

    def test_systimer_stopped_time_is_excluded(self):
        self.command('clock_step 125')
        self.write(0x60023000, 0)
        self.command('clock_step 10000')
        self.assertEqual(self.systimer_value(), 2)
        self.write(0x60023000, 1 << 30)
        self.command('clock_step 125')
        self.assertEqual(self.systimer_value(), 4)

    def test_systimer_alarm_keeps_submicrosecond_precision(self):
        self.command('clock_step 32')
        self.write(0x60023020, 1)
        self.write(0x60023050, 1)
        self.write(0x60023000, (1 << 30) | (1 << 24))
        self.assertEqual(self.read(0x60023068), 0)
        self.command('clock_step 30')
        self.assertEqual(self.read(0x60023068), 0)
        self.command('clock_step 1')
        self.assertEqual(self.read(0x60023068), 1)
        self.assertEqual(self.systimer_value(), 1)

    def test_systimer_reset_restarts_counter(self):
        self.command('clock_step 1000000')
        self.assertEqual(self.systimer_value(), 16000)
        self.qmp('system_reset')
        self.assertEqual(self.systimer_value(), 0)
        self.command('clock_step 125')
        self.assertEqual(self.systimer_value(), 2)

    def test_cache_occupy_completes(self):
        reg = 0x600C4034
        self.assertEqual(self.read(reg), 2)
        for _ in range(2):
            self.write(reg, 1)
            self.assertEqual(self.read(reg), 2)
            self.assertEqual(self.read(reg), 2)

    def test_bluetooth_low_power_clock(self):
        div, source = 0x600C0028, 0x600C002C
        self.assertEqual(self.read(div), 255)
        self.assertEqual(self.read(source), 0x02001001)
        # The controller verifies clock selection and divider by reading back.
        for bit in [26, 24, 27, 25]:
            self.write(source, (self.read(source) & 0xFFF000) | (1 << bit))
            self.assertEqual(self.read(source), 0x1000 | (1 << bit))
        self.write(div, 40)
        self.assertEqual(self.read(div), 40)
        self.write(div, 0xFFFFFFFF)
        self.write(source, 0xFFFFFFFF)
        self.assertEqual(self.read(div), 0xFFF)
        self.assertEqual(self.read(source), 0x1FFFFFFF)

    def test_bluetooth_exchange_memory(self):
        for addr, value in [(0x3FC00000, 0xA55A0123),
                            (0x3FC00100, 0x06040200),
                            (0x3FC0FFFC, 0x89ABCDEF)]:
            self.write(addr, value)
            self.assertEqual(self.read(addr), value)
        # EMI table entries encode offsets into the exchange-memory bank.
        self.write(0x60031204, 0x1000 << 16)
        self.assertEqual((self.read(0x60031204) >> 18) << 2, 0x1000)

    def test_bluetooth_identity_and_reset_completion(self):
        control, version = 0x60031000, 0x60031004
        self.assertEqual(self.read(version), 0x09001B00)
        self.write(version, 0)
        self.assertEqual(self.read(version), 0x09001B00)
        self.write(control, (1 << 31) | 0x100)
        self.assertEqual(self.read(control), 0x100)
        self.qmp('system_reset')
        self.assertEqual(self.read(control), 0)
        self.assertEqual(self.read(version), 0x09001B00)

    def test_bluetooth_clock_sample(self):
        coarse, fine = 0x6003101C, 0x60031020

        def sample(expected_coarse, expected_fine):
            self.write(coarse, 1 << 31)
            self.assertEqual(self.read(coarse), expected_coarse)
            self.assertEqual(self.read(fine), expected_fine)

        sample(0, 624)
        self.command('clock_step 500')
        sample(0, 623)
        self.command('clock_step 312000')
        # Sampling latches both counters until the next request.
        self.assertEqual(self.read(coarse), 0)
        self.assertEqual(self.read(fine), 623)
        sample(1, 624)

    def test_flash_read_dummy_bus_width(self):
        spi = 0x60002000
        for opcode, width_bit, dummy in [(3, 0, 0), (0x0B, 0, 8),
                                         (0xBB, 1 << 23, 4),
                                         (0xEB, 1 << 24, 6)]:
            with self.subTest(opcode=opcode):
                self.write(spi + 0x04, 0x10000)
                self.write(spi + 0x08, width_bit)
                self.write(spi + 0x18, 0xD0000000 | (bool(dummy) << 29))
                self.write(spi + 0x1C, (23 << 26) | max(dummy - 1, 0))
                self.write(spi + 0x20, (7 << 28) | opcode)
                self.write(spi + 0x28, 63)
                self.write(spi, 1 << 18)
                self.assertEqual(self.read(spi + 0x58), 0x78563412)
                self.assertEqual(self.read(spi + 0x5C), 0xF0DEBC9A)

    def test_panel_hardware_spi_identity(self):
        gpio, spi = 0x60004000, 0x60024000
        pins = (1 << 13) | (1 << 14) | (1 << 18)
        self.write(gpio + 0x04, pins)
        self.write(gpio + 0x20, pins)
        self.write(gpio + 0x0C, (1 << 13) | (1 << 18))
        self.write(spi + 0x98, 0x70)
        self.write(spi + 0x1C, 7)
        self.write(spi, 1 << 24)
        self.command('clock_step 1000000')
        self.write(gpio + 0x08, 1 << 18)
        self.write(spi + 0x1C, 39)
        self.write(spi, 1 << 24)
        self.command('clock_step 1000000')
        self.assertEqual(self.read(spi + 0x98), 0xFF010000)
        self.assertEqual(self.read(spi + 0x9C) & 0xFF, 0xFF)

    def test_power_held_then_released(self):
        gpio_in = 0x6000403C
        self.assertEqual(self.read(gpio_in) & 8, 0)
        self.command('clock_step 1499000000')
        self.assertEqual(self.read(gpio_in) & 8, 0)
        self.command('clock_step 1000000')
        self.assertEqual(self.read(gpio_in) & 8, 8)

    def test_pending_interrupt_follows_routing(self):
        spi, matrix = 0x60024000, 0x600C2000
        self.write(spi + 0x44, 1 << 12)
        self.write(spi + 0x34, 1 << 12)
        self.assertEqual(self.cpu_interrupts() & 3, 0)
        self.write(matrix + 21 * 4, 0)
        self.assertEqual(self.cpu_interrupts() & 3, 1)
        self.write(matrix + 21 * 4, 1)
        self.assertEqual(self.cpu_interrupts() & 3, 2)
        self.write(matrix + 21 * 4, 6)
        self.assertEqual(self.cpu_interrupts() & 3, 0)

    def test_shared_interrupt_stays_asserted(self):
        matrix = 0x600C2000
        # Drive two interrupt-matrix inputs directly, independent of devices.
        path = '/machine/soc/intmatrix'
        for source in [21, 22]:
            self.write(matrix + source * 4, 0)
            self.command(f'set_irq_in {path} unnamed-gpio-in {source} 1')
        self.assertEqual(self.cpu_interrupts() & 1, 1)
        self.command(f'set_irq_in {path} unnamed-gpio-in 21 0')
        self.assertEqual(self.cpu_interrupts() & 1, 1)
        self.command(f'set_irq_in {path} unnamed-gpio-in 22 0')
        self.assertEqual(self.cpu_interrupts() & 1, 0)

    def test_panel_dma_descriptor_chain(self):
        gpio, spi, dma = 0x60004000, 0x60024000, 0x6003F000
        desc, payload = 0x3FC90000, 0x3FC90100
        pins = (1 << 13) | (1 << 14) | (1 << 18)
        self.write(gpio + 0x04, pins)
        self.write(gpio + 0x20, pins)
        self.write(gpio + 0x0C, (1 << 13) | (1 << 18))
        # The final command is beyond the PIO buffer's 64-byte limit.
        for offset in range(0, 80, 4):
            self.write(payload + offset, 0x70000000 if offset == 76 else 0)
        for offset in [0, 12]:
            self.write(desc + offset, (1 << 31) | (40 << 12) | 40 |
                       ((1 << 30) if offset else 0))
            self.write(desc + offset + 4, payload + (40 if offset else 0))
            self.write(desc + offset + 8, 0 if offset else desc + 12)
        self.write(dma + 0x60 + 0x48, 0)  # SPI2 TX on channel 0
        self.write(dma + 0x60 + 0x20, (desc & 0xFFFFF) | (1 << 21))
        self.write(spi + 0x30, 1 << 28)
        self.write(spi + 0x10, 1 << 27)  # MOSI data phase
        self.write(spi + 0x1C, 80 * 8 - 1)
        self.write(spi, 1 << 24)
        self.command('clock_step 1000000')
        self.assertEqual(self.read(dma + 0x68) & 0xF, 0xB)
        # Read back 80 bytes through RX DMA, including the identity and tail.
        self.write(gpio + 0x08, 1 << 18)
        self.write(desc, (1 << 31) | 80)
        self.write(desc + 4, payload)
        self.write(desc + 8, 0)
        self.write(dma + 0x48, 0)
        self.write(dma + 0x20, (desc & 0xFFFFF) | (1 << 22))
        self.write(spi + 0x30, 1 << 27)
        self.write(spi + 0x10, 1 << 28)  # MISO data phase
        self.write(spi, 1 << 24)
        self.command('clock_step 1000000')
        self.assertEqual(self.read(payload), 0xFF010000)
        self.assertEqual(self.read(payload + 76), 0xFFFFFFFF)
        self.assertEqual((self.read(desc) >> 12) & 0xFFF, 80)
        self.assertEqual(self.read(desc) >> 31, 0)

    def test_spi_inactive_dma_phase_preserves_descriptor_and_buffer(self):
        spi, dma, desc, payload = 0x60024000, 0x6003F000, 0x3FC90000, 0x3FC90100
        # A previous DMA transaction can leave a channel enabled and pointing
        # at memory the driver has since freed. The opposite phase must not
        # reuse it, even when descriptor owner checking is disabled.
        for rx in (True, False):
            with self.subTest(rx=rx):
                channel = dma if rx else dma + 0x60
                config = (1 << 31) | (4 << 12) | 4 | (1 << 30)
                self.write(desc, config)
                self.write(desc + 4, payload)
                self.write(desc + 8, 0)
                self.write(payload, 0x3FCE9724)
                self.write(channel + 0x48, 0)
                self.write(channel + 0x20, (desc & 0xFFFFF) | (1 << (22 if rx else 21)))
                self.write(spi + 0x30, 1 << (27 if rx else 28))
                self.write(spi + 0x10, 1 << (27 if rx else 28))
                self.write(spi + 0x1C, 7)
                self.write(spi, 1 << 24)
                self.command('clock_step 1000000')
                self.assertEqual(self.read(payload), 0x3FCE9724)
                self.assertEqual(self.read(desc), config)

    def test_panel_scan_directions(self):
        gpio, spi = 0x60004000, 0x60024000
        pins = (1 << 13) | (1 << 14) | (1 << 18)
        self.write(gpio + 0x04, pins)
        self.write(gpio + 0x20, pins)
        self.write(gpio + 0x0C, 1 << 13)

        def send(value, data=False):
            self.write(gpio + (0x08 if data else 0x0C), 1 << 18)
            self.write(spi + 0x98, value)
            self.write(spi + 0x1C, 7)
            self.write(spi, 1 << 24)
            self.command('clock_step 1000000')

        send(0x13)
        send(0x7F, True)  # One black pixel, all other RAM remains white.
        corners = [(0, 0), (0, 799), (479, 0), (479, 799)]
        for psr, expected in zip([0x1F, 0x1B, 0x17, 0x13], corners):
            with self.subTest(psr=psr):
                send(0)
                send(psr, True)
                send(0x12)
                self.command('clock_step 2000000000')
                screenshot = Path(self.directory.name) / 'panel.ppm'
                self.qmp('screendump', filename=str(screenshot), format='ppm')
                header, size, maximum, pixels = screenshot.read_bytes().split(b'\n', 3)
                self.assertEqual((header, size, maximum), (b'P6', b'480 800', b'255'))
                for x, y in corners:
                    shade = pixels[(y * 480 + x) * 3]
                    if (x, y) == expected:
                        self.assertLess(shade, 80)
                    else:
                        self.assertGreater(shade, 180)
