"""Keyboard and QOM presses must decode in both raw and calibrated ADC bands."""
import importlib
import unittest

base = importlib.import_module('eink-qemu.tests.test_gpio_reset')
x4 = importlib.import_module('eink-qemu.tests.test_x4_boot')
ADC = 0x60040000


def calibrated_mv(raw, code):
    # ESP-IDF/esp-hal ESP32-C3 ADC1 11 dB curve, following line calibration.
    # Independent of the key model: firmware consumes these calibrated values.
    coeffs = (-1.4912262772850453, -0.0228549975564099,
              0.0000356391935717, -0.0000000179964582,
              0.0000000000042046)
    value = raw * (1370 * 65536 // code) // 65536
    error = 0
    for coeff in reversed(coeffs):
        error = error * value + int(coeff * (1 << 52))
    error = (abs(error) // (1 << 52)) * (-1 if error < 0 else 1)
    return value - error if value else 0


@unittest.skipUnless(base.QEMU.exists(), 'build qemu-system-riscv32 first')
class X3Keys(unittest.TestCase):
    setUp = base.GPIOReset.setUp
    stop = base.GPIOReset.stop
    qmp = base.GPIOReset.qmp
    command = base.GPIOReset.command
    read = base.GPIOReset.read
    write = base.GPIOReset.write

    def sample(self, channel):
        config = (1 << 31) | (channel << 25) | (3 << 23)
        self.write(ADC + 0x20, config)
        self.write(ADC + 0x20, config | (1 << 29))
        return self.read(ADC + 0x2c) & 0xfff

    def test_keys_in_raw_and_calibrated_bands(self):
        # ADC1_CAL_VOL_ATTEN3 spans BLOCK2 words 6 and 7.
        efuse = 0x60008800
        field = (self.read(efuse + 0x74) >> 26) | ((self.read(efuse + 0x78) & 15) << 6)
        code = 2000 + (-(field & 511) if field & 512 else field)
        cases = [
            ('back', 'esc', 1, (3101, 3899), (2400, 2700)),
            ('confirm', 'ret', 1, (2091, 3100), (1800, 2150)),
            ('left', 'left', 1, (751, 2090), (1000, 1250)),
            ('right', 'right', 1, (0, 750), (0, 100)),
            ('up', 'up', 2, (1121, 3899), (1500, 1800)),
            ('down', 'down', 2, (0, 1120), (0, 100)),
            ('back', 'backspace', 1, (3101, 3899), (2400, 2700)),
        ]
        for route in ('qom', 'keyboard'):
            for key, qcode, channel, raw_band, mv_band in cases:
                with self.subTest(route=route, key=qcode):
                    def send(down):
                        if route == 'qom':
                            self.qmp('qom-set', path='/machine/x3-keys',
                                     property=key, value=down)
                        else:
                            self.qmp('input-send-event', events=[{
                                'type': 'key', 'data': {'down': down,
                                    'key': {'type': 'qcode', 'data': qcode}}}])
                    send(True)
                    try:
                        raw = self.sample(channel)
                        self.assertTrue(raw_band[0] <= raw <= raw_band[1], raw)
                        mv = calibrated_mv(raw, code)
                        self.assertTrue(mv_band[0] <= mv <= mv_band[1], mv)
                        self.assertEqual(self.sample(3 - channel), 4095)
                    finally:
                        send(False)
                    self.assertEqual(self.sample(channel), 4095)
                    self.assertGreater(calibrated_mv(self.sample(channel), code), 2700)


class X4Keys(X3Keys):
    setUp = x4.X4Panel.setUp


if __name__ == '__main__':
    unittest.main()
