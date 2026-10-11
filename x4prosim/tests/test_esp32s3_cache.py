"""ESP32-S3 cache enable and core bus gates are separate registers."""
import unittest

from x4prosim.tests import test_x4pro_boot as boot


@unittest.skipUnless(boot.QEMU.exists(), 'build qemu-system-xtensa first')
class CacheControl(unittest.TestCase):
    setUp = boot.StockBootPeripherals.setUp
    stop = boot.StockBootPeripherals.stop
    qmp = boot.StockBootPeripherals.qmp
    command = boot.StockBootPeripherals.command
    read = boot.StockBootPeripherals.read
    write = boot.StockBootPeripherals.write

    def test_enabling_core_bus_preserves_cache_enable(self):
        # IDF opens both core buses after enabling the cache. SPI rejects
        # PSRAM transactions when the separate CTRL.ENABLE bit reads zero.
        for ctrl in [0x600c4000, 0x600c4060]:
            with self.subTest(ctrl=hex(ctrl)):
                self.write(ctrl, 1)
                self.write(ctrl + 4, 0)
                self.assertEqual(self.read(ctrl) & 1, 1)
                self.assertEqual(self.read(ctrl + 4), 0)
                for gates in [1, 2, 3]:
                    self.write(ctrl + 4, gates)
                    self.assertEqual(self.read(ctrl + 4), gates)
                    self.assertEqual(self.read(ctrl) & 1, 1)
                self.write(ctrl, 0)
                self.assertEqual(self.read(ctrl) & 1, 0)
                self.assertEqual(self.read(ctrl + 4), 3)

    def test_reset_clears_enable_and_shuts_core_buses(self):
        for ctrl in [0x600c4000, 0x600c4060]:
            self.write(ctrl, 1)
            self.write(ctrl + 4, 0)
        self.qmp('system_reset')
        for ctrl in [0x600c4000, 0x600c4060]:
            self.assertEqual(self.read(ctrl) & 1, 0)
            self.assertEqual(self.read(ctrl + 4), 3)
