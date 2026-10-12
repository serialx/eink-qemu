"""RTC deadlines, clock calibration and digital timer suspension."""
import importlib
import unittest
boot = importlib.import_module('eink-qemu.tests.test_x4pro_boot')

@unittest.skipUnless(boot.QEMU.exists(), 'build Xtensa QEMU first')
class S3Sleep(unittest.TestCase):
    setUp = boot.StockBootPeripherals.setUp
    command = boot.StockBootPeripherals.command
    read = boot.StockBootPeripherals.read
    write = boot.StockBootPeripherals.write
    stop = boot.StockBootPeripherals.stop
    qmp = boot.StockBootPeripherals.qmp
    systimer_value = boot.StockBootPeripherals.systimer_value
    base = 0x60008000

    def rtc(self):
        self.write(self.base + 0xc, 1 << 31)
        return self.read(self.base + 0x10) | (self.read(self.base + 0x14) << 32)

    def sleep_until(self, ticks, deep=False):
        self.write(self.base + 0x90, (1 << 31) if deep else 0)
        self.write(self.base + 4, ticks)
        self.write(self.base + 8, 1 << 16)
        self.write(self.base + 0x3c, 8 << 15)
        self.write(self.base + 0x18, 1 << 31)

    def test_rtc_agrees_with_calibration(self):
        self.write(0x6001f068, (1 << 31) | (1024 << 16))
        cycles = self.read(0x6001f06c) >> 7
        self.command('clock_step 1000000000')
        self.assertAlmostEqual(self.rtc() * cycles / 1024, 40000000, delta=150)

    def test_reserved_calibration_selector_and_zero_period_are_bounded(self):
        self.write(0x6001f068, (1 << 31) | (1024 << 16) | (3 << 13))
        self.assertTrue(self.read(0x6001f068) & (1 << 15))
        self.command('clock_step 32')
        self.write(0x60023034, 1 << 30)  # periodic, zero ticks
        self.write(0x60023000, (1 << 30) | (1 << 24))
        self.command('clock_step 1000000')
        self.assertGreater(self.systimer_value(), 0)

    def test_sleep_preserves_rtc_and_stops_systimer(self):
        self.command('clock_step 1000000000')
        before = self.rtc()
        self.sleep_until(before + 13600)
        sys_before = self.systimer_value()
        self.command('clock_step 99999999')
        self.assertEqual(self.read(self.base + 0x44), 0)
        self.assertEqual(self.systimer_value(), sys_before)
        self.command('clock_step 1')
        self.assertEqual(self.read(self.base + 0x44), 1)
        self.assertEqual(self.read(self.base + 0x130), 8)
        self.assertEqual(self.rtc(), before + 13600)
        self.command('clock_step 1000')
        self.assertEqual(self.systimer_value(), sys_before + 16)

    def test_mux_preserves_counter_and_reserved_selector_is_safe(self):
        self.command('clock_step 1000000000')
        before = self.rtc()
        self.write(self.base + 0x74, 1 << 30)
        self.assertEqual(self.rtc(), before)
        self.command('clock_step 1000000000')
        self.assertEqual(self.rtc(), before + 32000)
        self.write(self.base + 0x74, 3 << 30)
        self.command('clock_step 1000000')
        self.assertGreaterEqual(self.rtc(), before + 32000)

    def test_deep_sleep_wakes_with_retained_memory_and_reset_cause(self):
        self.write(self.base + 0x50, 0x827968)
        self.write(0x50000000, 0x12345678)
        self.sleep_until(13600, deep=True)
        self.command('clock_step 100000000')
        # Process the pending system reset through a monitor round trip.
        self.qmp('query-status')
        self.assertEqual(self.read(self.base + 0x38) & 63, 5)
        self.assertEqual(self.read(self.base + 0x130), 8)
        self.assertEqual(self.read(self.base + 0x50), 0x827968)
        self.assertEqual(self.read(0x50000000), 0x12345678)
