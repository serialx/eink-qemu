"""I2C ACK-error termination against the real ESP32-S3 controller model."""
import unittest

from x4prosim.tests import test_x4pro_boot as boot


@unittest.skipUnless(boot.QEMU.exists(), 'build qemu-system-xtensa first')
class I2CNack(unittest.TestCase):
    # Reuse the existing board's QMP/qtest transport without inheriting its tests.
    setUp = boot.StockBootPeripherals.setUp
    stop = boot.StockBootPeripherals.stop
    qmp = boot.StockBootPeripherals.qmp
    command = boot.StockBootPeripherals.command
    read = boot.StockBootPeripherals.read
    write = boot.StockBootPeripherals.write

    def test_unexpected_ack_stops_bus_and_signals_completion(self):
        base = 0x60013000
        nack, complete = 1 << 10, 1 << 7
        self.write(base + 0x28, nack | complete)

        def start(address, expected_nack=False):
            self.write(base + 0x18, (1 << 12) | (1 << 13))
            self.write(base + 0x18, 0)
            self.write(base + 0x1c, address << 1)
            self.write(base + 0x58, 6 << 11)  # START
            self.write(base + 0x5c, (1 << 11) | (1 << 8) |
                       (int(expected_nack) << 9) | 1)  # checked address WRITE
            self.write(base + 0x60, 2 << 11)  # STOP
            self.write(base + 4, 1 << 5)

        # Missing slave fails ACK=0; an existing RTC fails expected ACK=1.
        for address, expected_nack in [(0x7e, False), (0x51, True)]:
            with self.subTest(address=address, expected_nack=expected_nack):
                start(address, expected_nack)
                self.assertEqual(self.read(base + 0x20) & (nack | complete),
                                 nack | complete)
                self.assertEqual(self.read(base + 0x2c), nack | complete)
                self.assertEqual(self.read(base + 8) & (1 << 4), 0)
                # Auto-STOP terminates the failed WRITE before the programmed
                # STOP command executes, while still raising STOP detection.
                self.assertEqual(self.read(base + 0x60) & (1 << 31), 0)
                self.write(base + 0x24, nack)
                self.assertEqual(self.read(base + 0x2c), complete)
                self.write(base + 0x24, complete)
                self.assertEqual(self.read(base + 0x2c), 0)

        # A subsequent successful probe must have only its own completion.
        start(0x51)
        self.assertEqual(self.read(base + 0x2c), complete)
        self.assertNotEqual(self.read(base + 0x60) & (1 << 31), 0)
