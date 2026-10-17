"""ESP32-C3 deep sleep, reset reasons and the X3's Power hold, without firmware.

QEMU_RISCV32=build/native/qemu-system-riscv32 python3 -m unittest eink-qemu.tests.test_c3_sleep
"""
import importlib
import unittest

base = importlib.import_module('eink-qemu.tests.test_gpio_reset')

RTC = 0x60008000
GPIO_IN = 0x6000403C
OPTIONS0, SLP_TIMER0, SLP_TIMER1, STATE0 = 0x00, 0x04, 0x08, 0x18
RESET_STATE, WAKEUP_STATE, DIG_PWC = 0x38, 0x3c, 0x88
WAKEUP_CAUSE, GPIO_WAKEUP = 0xf8, 0x110
WAKE_GPIO, WAKE_TIMER = 4, 8


@unittest.skipUnless(base.QEMU.exists(), 'build qemu-system-riscv32 first')
class C3DeepSleep(unittest.TestCase):
    setUp = base.GPIOReset.setUp
    stop = base.GPIOReset.stop
    qmp = base.GPIOReset.qmp
    command = base.GPIOReset.command
    read = base.GPIOReset.read
    write = base.GPIOReset.write

    def release_boot_hold(self):
        # The cold power-on holds Power (GPIO3) for 1.5 s.
        self.assertEqual(self.read(GPIO_IN) & 8, 0)
        self.command('clock_step 1600000000')
        self.assertEqual(self.read(GPIO_IN) & 8, 8)

    def deep_sleep(self, ena):
        self.write(RTC + DIG_PWC, 1 << 31)
        self.write(RTC + WAKEUP_STATE, ena << 15)
        self.write(RTC + STATE0, 1 << 31)

    def status(self):
        return self.qmp('query-status')['status']

    def power(self, down):
        self.qmp('qom-set', path='/machine/x3-keys', property='power', value=down)

    def test_power_pad_wake_holds_power(self):
        self.release_boot_hold()
        self.write(RTC + GPIO_WAKEUP, (1 << 28) | (4 << 14))   # pad 3 (Power), low
        self.deep_sleep(WAKE_GPIO)
        self.assertEqual(self.status(), 'suspended')
        self.power(True)
        self.assertEqual(self.status(), 'running')
        self.assertEqual(self.read(RTC + RESET_STATE), 5)
        self.assertEqual(self.read(RTC + WAKEUP_CAUSE), WAKE_GPIO)
        self.assertEqual(self.read(RTC + GPIO_WAKEUP) & 0x3f, 1 << 3)
        self.power(False)
        self.assertEqual(self.read(GPIO_IN) & 8, 0)
        self.command('clock_step 1500000000')
        self.assertEqual(self.read(GPIO_IN) & 8, 8)

    def test_an_early_software_reset_ends_the_cold_boot_hold(self):
        self.assertEqual(self.read(GPIO_IN) & 8, 0)     # held since power-on
        self.command('clock_step 100000000')
        self.write(RTC + OPTIONS0, 1 << 31)             # SW_SYS_RST within the hold
        self.qmp('query-status')
        self.assertEqual(self.read(RTC + RESET_STATE), 3)
        self.assertEqual(self.read(GPIO_IN) & 8, 8)

    def test_software_reset_boots_with_power_released(self):
        self.release_boot_hold()
        self.write(RTC + OPTIONS0, 1 << 31)     # SW_SYS_RST
        self.qmp('query-status')
        self.assertEqual(self.read(RTC + RESET_STATE), 3)
        self.assertEqual(self.read(GPIO_IN) & 8, 8)

    def test_timer_wake_then_external_reset(self):
        self.release_boot_hold()
        self.write(RTC + 0x0c, 1 << 31)         # TIME_UPDATE
        now = self.read(RTC + 0x10)
        self.write(RTC + SLP_TIMER0, now + 13600)
        self.write(RTC + SLP_TIMER1, 1 << 16)
        self.deep_sleep(WAKE_TIMER)
        # Deep sleep on a timer keeps the VM running; the RTC still says so.
        self.assertEqual(self.status(), 'running')
        self.assertIs(self.qmp('qom-get', path='/machine/rtccntl', property='deep-sleep'), True)
        self.command('clock_step 200000000')
        self.qmp('query-status')
        self.assertIs(self.qmp('qom-get', path='/machine/rtccntl', property='deep-sleep'), False)
        self.assertEqual(self.read(RTC + RESET_STATE), 5)
        self.assertEqual(self.read(RTC + WAKEUP_CAUSE), WAKE_TIMER)
        self.assertEqual(self.read(GPIO_IN) & 8, 8)     # no hand on Power
        self.qmp('system_reset')
        self.assertEqual(self.read(RTC + RESET_STATE), 1)
        self.assertEqual(self.read(RTC + WAKEUP_CAUSE), 0)
        self.assertEqual(self.read(GPIO_IN) & 8, 0)     # a power-on holds it again

    def test_deep_sleep_without_wake_source_is_off_until_reset(self):
        self.release_boot_hold()
        self.deep_sleep(0)
        self.assertEqual(self.status(), 'suspended')
        self.power(True)
        self.power(False)
        self.assertEqual(self.status(), 'suspended')
        self.qmp('system_reset')
        self.assertEqual(self.status(), 'running')
        self.assertEqual(self.read(RTC + RESET_STATE), 1)
        self.assertEqual(self.read(GPIO_IN) & 8, 0)

    def test_light_sleep_without_wake_source_returns(self):
        self.write(RTC + WAKEUP_STATE, 0)
        self.write(RTC + STATE0, 1 << 31)
        self.assertEqual(self.read(RTC + 0x44) & 1, 1)
        self.assertEqual(self.status(), 'running')


if __name__ == '__main__':
    unittest.main()
