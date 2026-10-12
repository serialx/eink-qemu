"""SDMMC virtual command/data deadlines and DMA ownership, on a disposable card."""
import importlib
import os
from pathlib import Path
import tempfile
import unittest

boot = importlib.import_module('eink-qemu.tests.test_x4pro_boot')


@unittest.skipUnless(boot.QEMU.exists(), 'build Xtensa QEMU first')
class SdmmcTiming(unittest.TestCase):
    command = boot.StockBootPeripherals.command
    read = boot.StockBootPeripherals.read
    write = boot.StockBootPeripherals.write
    stop = boot.StockBootPeripherals.stop
    qmp = boot.StockBootPeripherals.qmp
    base = 0x60028000

    def setUp(self):
        self.card = tempfile.NamedTemporaryFile()
        self.addCleanup(self.card.close)
        self.card.write(bytes(range(256)) * 2)
        self.card.truncate(32 * 1024 * 1024)
        self.card.flush()
        self.extra_qemu_args = ['-drive', f'file={self.card.name},if=sd,format=raw',
            '-global', 'dwc_sdmmc.timing=true', '-global', 'dwc_sdmmc.read-latency-us=0',
            '-global', 'dwc_sdmmc.write-latency-us=0']
        boot.StockBootPeripherals.setUp(self)
        self.w(0x800, (1 << 23) | (3 << 13) | (3 << 17) | (1 << 9))
        self.w(0x10, 2)  # slot 1, PLL160/4 = 40 MHz
        self.w(0x0c, 4)  # slot 1 uses divider 1

    def w(self, reg, value): self.write(self.base + reg, value)
    def r(self, reg): return self.read(self.base + reg)

    def issue(self, cmd, arg=0, flags=0x40):
        self.w(0x44, 0xffffffff)
        self.w(0x28, arg)
        self.w(0x2c, (1 << 31) | (1 << 16) | flags | cmd)

    def init_card(self):
        for cmd, arg, flags in [(0,0,0),(8,0x1aa,0x40),(55,0,0x40),
                                (41,0x40ff8000,0x40),(2,0,0xc0),(3,0,0x40)]:
            self.issue(cmd,arg,flags);self.command('clock_step 1000000')
            self.assertFalse(self.r(0x44) & 0x100, (cmd,self.r(0x44)))
        rca = self.r(0x30) & 0xffff0000
        self.issue(7,rca);self.command('clock_step 1000000')
        self.issue(16,512);self.command('clock_step 1000000')
        self.w(0, 0x30)
        self.w(0x1c,512);self.w(0x20,512)
        self.write(0x3fc90000,0x8000000c)
        self.write(0x3fc90004,512)
        self.write(0x3fc90008,0x3fc91000)
        self.write(0x3fc9000c,0)
        self.w(0x88,0x3fc90000)

    def test_clock_register_readback(self):
        for reg,value in [(8,0x12345678),(0x10,0x30003),(0x18,0x30003),(0x800,0x876543)]:
            self.w(reg,value);self.assertEqual(self.r(reg),value)

    def test_command_deadline_and_overlap(self):
        self.issue(0,flags=0)
        self.assertTrue(self.r(0x2c) & (1 << 31))
        self.assertFalse(self.r(0x44) & 4)
        self.w(0x2c,(1<<31)|8)
        self.assertTrue(self.r(0x44) & (1<<12))
        self.assertEqual(self.r(0x2c)&63,0)
        self.command('clock_step 1199');self.assertFalse(self.r(0x44)&4)
        self.command('clock_step 1');self.assertTrue(self.r(0x44)&4)

    def test_read_dma_waits_for_wire_deadline(self):
        self.init_card();self.issue(17,flags=0x240)
        self.command('clock_step 2400')
        self.assertEqual(self.r(0x44)&0x10c,4)
        self.assertEqual(self.read(0x3fc91000),0)
        self.assertTrue(self.read(0x3fc90000)&(1<<31))
        self.command('clock_step 102849')
        self.assertFalse(self.r(0x44)&8)
        self.command('clock_step 1')
        self.assertTrue(self.r(0x44)&8)
        self.assertEqual(self.read(0x3fc91000),0x03020100)
        self.assertFalse(self.read(0x3fc90000)&(1<<31))
        self.assertTrue(self.r(0x8c)&2)

    def test_stale_response_error_does_not_suppress_next_data(self):
        self.init_card()
        self.issue(63)  # unsupported command, response timeout
        self.command('clock_step 2400')
        self.assertTrue(self.r(0x44) & 0x100)
        # Intentionally leave the previous sticky error uncleared.
        self.w(0x28, 0)
        self.w(0x2c, (1 << 31) | (1 << 16) | 0x240 | 17)
        self.command('clock_step 105250')
        self.assertTrue(self.r(0x44) & 8)
        self.assertEqual(self.read(0x3fc91000), 0x03020100)

    def test_reset_cancels_pending_dma(self):
        self.init_card();self.issue(17,flags=0x240)
        self.command('clock_step 2400');self.w(0,1)
        self.command('clock_step 1000000')
        self.assertEqual(self.r(0x44),0)
        self.assertEqual(self.read(0x3fc91000),0)
        self.assertTrue(self.read(0x3fc90000)&(1<<31))

    def test_disabled_clock_waits_until_enabled(self):
        self.w(0x10,0);self.issue(0,flags=0)
        self.command('clock_step 1000000');self.assertFalse(self.r(0x44)&4)
        self.w(0x10,2)
        self.command('clock_step 1200');self.assertTrue(self.r(0x44)&4)

    def test_divider_and_bus_width_scale_data_time(self):
        self.init_card()
        # 20 MHz, four data lines: 512*8/4 + 18 data clocks.
        self.w(8,1<<8);self.w(0x18,2)
        self.issue(17,flags=0x240)
        self.command('clock_step 4800')
        self.command('clock_step 52099');self.assertFalse(self.r(0x44)&8)
        self.command('clock_step 1');self.assertTrue(self.r(0x44)&8)
        self.assertEqual(self.read(0x3fc91000),0x03020100)

    def test_write_waits_for_data_deadline(self):
        self.init_card();self.write(0x3fc91000,0x5aa55aa5)
        self.issue(24,512,0x640)
        self.command('clock_step 2400')
        self.assertTrue(self.read(0x3fc90000)&(1<<31))
        self.command('clock_step 102850')
        self.assertTrue(self.r(0x44)&8);self.assertTrue(self.r(0x8c)&1)
        # Read the sector back through the controller, not the host cache.
        self.w(0x20,512);self.w(0x88,0x3fc90000)
        self.write(0x3fc90000,0x8000000c);self.write(0x3fc91000,0)
        self.issue(17,512,0x240);self.command('clock_step 105250')
        self.assertEqual(self.read(0x3fc91000),0x5aa55aa5)

    def test_unowned_descriptor_waits_for_poll_demand(self):
        self.init_card();self.write(0x3fc90000,0x0c)
        self.issue(17,flags=0x240);self.command('clock_step 105250')
        self.assertTrue(self.r(0x8c)&(1<<4));self.assertFalse(self.r(0x44)&8)
        self.write(0x3fc90000,0x8000000c);self.w(0x84,1)
        self.command('clock_step 102850')
        self.assertTrue(self.r(0x44)&8)
        self.assertEqual(self.read(0x3fc91000),0x03020100)

    def test_dma_reset_cancels_pending_transfer(self):
        self.init_card();self.issue(17,flags=0x240)
        self.command('clock_step 2400');self.w(0x80,1)
        self.command('clock_step 1000000')
        self.assertFalse(self.r(0x44)&8);self.assertEqual(self.r(0x8c),0)
        self.assertEqual(self.read(0x3fc91000),0)
