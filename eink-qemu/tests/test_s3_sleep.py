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

    def test_periodic_alarm_preserves_phase_across_sleep(self):
        self.write(0x60023034, (1 << 30) | 16000)
        self.write(0x60023050, 1)
        self.write(0x60023000, (1 << 30) | (1 << 24))
        self.command('clock_step 250000')
        self.sleep_until(self.rtc() + 13600)
        self.command('clock_step 100000000')
        self.assertEqual(self.systimer_value(), 4000)
        self.command('clock_step 749999')
        self.assertEqual(self.read(0x60023068), 0)
        self.command('clock_step 1')
        self.assertEqual(self.read(0x60023068), 1)
        self.assertEqual(self.systimer_value(), 16000)

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


GPIO_IN = 0x6000403C
RTCIO = 0x60008400


@unittest.skipUnless(boot.QEMU.exists(), 'build Xtensa QEMU first')
class S3DeepSleepWake(unittest.TestCase):
    """EXT0/EXT1 wake on the RTC pads, a chip with no wake source powered off,
    reset reasons, and when the X4 Pro holds Power at boot."""
    setUp = boot.StockBootPeripherals.setUp
    command = boot.StockBootPeripherals.command
    read = boot.StockBootPeripherals.read
    write = boot.StockBootPeripherals.write
    stop = boot.StockBootPeripherals.stop
    qmp = boot.StockBootPeripherals.qmp
    rtc = S3Sleep.rtc
    sleep_until = S3Sleep.sleep_until
    base = 0x60008000

    def release_boot_hold(self):
        # The cold power-on holds Power (GPIO3) for 1.5 s.
        self.command('clock_step 1600000000')
        self.assertEqual(self.read(GPIO_IN) & 8, 8)

    def deep_sleep(self, ena):
        self.write(self.base + 0x90, 1 << 31)
        self.write(self.base + 0x3c, ena << 15)
        self.write(self.base + 0x18, 1 << 31)

    def status(self):
        return self.qmp('query-status')['status']

    def key(self, name, down):
        self.qmp('qom-set', path='/machine/x4pro-keys', property=name, value=down)

    def test_ext0_wakes_deep_sleep_on_power_and_holds_it(self):
        self.release_boot_hold()
        self.write(RTCIO + 0xdc, 3 << 27)   # EXT0 on RTC IO 3, the Power key
        self.write(self.base + 0x64, 0)     # when low
        self.deep_sleep(1)
        self.assertEqual(self.status(), 'suspended')
        self.command('clock_step 1000000000')
        self.assertEqual(self.status(), 'suspended')
        self.key('power', True)
        self.assertEqual(self.status(), 'running')
        self.assertEqual(self.read(self.base + 0x38) & 63, 5)
        self.assertEqual(self.read(self.base + 0x130), 1)
        # The hand that woke it is still on Power while the firmware checks.
        self.key('power', False)
        self.assertEqual(self.read(GPIO_IN) & 8, 0)
        self.command('clock_step 1500000000')
        self.assertEqual(self.read(GPIO_IN) & 8, 8)

    def test_pad_wake_by_another_key_boots_with_power_released(self):
        self.release_boot_hold()
        self.write(RTCIO + 0xdc, 7 << 27)   # EXT0 on Down
        self.deep_sleep(1)
        self.assertEqual(self.status(), 'suspended')
        self.key('down', True)
        self.assertEqual(self.status(), 'running')
        self.assertEqual(self.read(self.base + 0x130), 1)
        self.assertEqual(self.read(GPIO_IN) & 8, 8)

    def test_ext1_all_low_needs_every_selected_pad(self):
        self.release_boot_hold()
        mask = (1 << 3) | (1 << 7)          # Power and Down
        self.write(self.base + 0xe0, mask)
        self.write(self.base + 0x64, 0)     # all low
        self.deep_sleep(2)
        self.assertEqual(self.status(), 'suspended')
        self.key('down', True)
        self.assertEqual(self.status(), 'suspended')
        self.key('power', True)
        self.assertEqual(self.status(), 'running')
        self.assertEqual(self.read(self.base + 0x38) & 63, 5)
        self.assertEqual(self.read(self.base + 0x130), 2)
        self.assertEqual(self.read(self.base + 0xe4), mask)

    def test_ext1_any_high_light_sleep_and_status_clear(self):
        self.write(self.base + 0xe0, 1 << 5)        # RTC IO 5 floats high
        self.write(self.base + 0x64, 1 << 31)       # any high
        self.write(self.base + 0x3c, 2 << 15)
        self.write(self.base + 0x18, 1 << 31)       # light sleep
        self.assertEqual(self.read(self.base + 0x44) & 1, 1)
        self.assertEqual(self.read(self.base + 0x130), 2)
        self.assertEqual(self.read(self.base + 0xe4), 1 << 5)
        self.write(self.base + 0xe0, (1 << 22) | (1 << 5))
        self.assertEqual(self.read(self.base + 0xe4), 0)
        self.assertEqual(self.read(self.base + 0xe0), 1 << 5)

    def test_rtcio_window_keeps_registers_and_reports_pads(self):
        self.write(RTCIO + 0x84, 0x12345678)
        self.assertEqual(self.read(RTCIO + 0x84), 0x12345678)
        self.assertEqual((self.read(RTCIO + 0x24) >> 10) & 8, 0)    # held at power-on
        self.release_boot_hold()
        self.assertEqual((self.read(RTCIO + 0x24) >> 10) & 8, 8)
        self.key('power', True)
        self.assertEqual((self.read(RTCIO + 0x24) >> 10) & 8, 0)

    def test_deep_sleep_without_wake_source_is_off_until_reset(self):
        self.release_boot_hold()
        self.deep_sleep(0)
        self.assertEqual(self.status(), 'suspended')
        self.key('power', True)
        self.key('power', False)
        self.assertEqual(self.status(), 'suspended')
        self.qmp('system_reset')
        self.assertEqual(self.status(), 'running')
        self.assertEqual(self.read(self.base + 0x38) & 63, 1)
        self.assertEqual(self.read(GPIO_IN) & 8, 0)     # a cold power-on holds Power

    def test_light_sleep_without_wake_source_returns(self):
        self.write(self.base + 0x3c, 0)
        self.write(self.base + 0x18, 1 << 31)
        self.assertEqual(self.read(self.base + 0x44) & 1, 1)
        self.assertEqual(self.status(), 'running')

    def test_external_reset_after_a_wake_is_a_power_on(self):
        self.sleep_until(13600, deep=True)
        self.command('clock_step 100000000')
        self.qmp('query-status')
        self.assertEqual(self.read(self.base + 0x38) & 63, 5)
        self.assertEqual(self.read(self.base + 0x130), 8)
        self.qmp('system_reset')
        self.assertEqual(self.read(self.base + 0x38) & 63, 1)
        self.assertEqual(self.read(self.base + 0x130), 0)

    def test_software_reset_and_timer_wake_boot_with_power_released(self):
        self.release_boot_hold()
        self.write(self.base, 1 << 31)      # OPTIONS0 SW_SYS_RESET
        self.qmp('query-status')
        self.assertEqual(self.read(self.base + 0x38) & 63, 3)
        self.assertEqual(self.read(GPIO_IN) & 8, 8)
        self.sleep_until(self.rtc() + 13600, deep=True)
        self.command('clock_step 200000000')
        self.qmp('query-status')
        self.assertEqual(self.read(self.base + 0x38) & 63, 5)
        self.assertEqual(self.read(self.base + 0x130), 8)
        self.assertEqual(self.read(GPIO_IN) & 8, 8)
