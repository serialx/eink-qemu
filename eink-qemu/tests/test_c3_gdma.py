"""C3 SPI DMA: FIFO readiness, receive EOF and all three interrupt banks."""
import importlib
import unittest

base = importlib.import_module('eink-qemu.tests.test_gpio_reset')
DMA, SPI, RAM = 0x6003f000, 0x60024000, 0x3fc90000


@unittest.skipUnless(base.QEMU.exists(), 'build qemu-system-riscv32 first')
class C3Gdma(unittest.TestCase):
    setUp = base.GPIOReset.setUp
    stop = base.GPIOReset.stop
    qmp = base.GPIOReset.qmp
    command = base.GPIOReset.command
    read = base.GPIOReset.read
    write = base.GPIOReset.write

    def descriptor(self, channel, rx, data, *, capacity=None, owner=True):
        desc = RAM + channel * 0x100 + (0x80 if rx else 0)
        buf = desc + 0x1000
        self.command(f'write {buf:#x} {len(data)} 0x{data.hex()}')
        flags = (int(owner) << 31) | (capacity or len(data))
        if not rx:
            flags |= (1 << 30) | (len(data) << 12)
        self.write(desc, flags)
        self.write(desc + 4, buf)
        self.write(desc + 8, 0)
        reg = DMA + 0x70 + channel * 0xc0 + (0 if rx else 0x60)
        self.write(reg + 4, 1 << 12)  # Check descriptor ownership.
        self.write(reg + 0x30, 0)  # SPI2
        self.write(reg + 0x10, desc & 0xfffff)
        return reg, desc, buf

    def test_spi_dma_on_every_channel(self):
        # CMD0 (with CRC), one response-delay byte, then the idle response.
        tx = bytes.fromhex('400000000095ffff')
        self.write(0x60004024, 1 << 12)  # Select SD, leave panel deselected.
        for channel in range(3):
            with self.subTest(channel=channel):
                out, txdesc, _ = self.descriptor(channel, False, tx)
                inp, rxdesc, rxbuf = self.descriptor(channel, True, bytes(32))
                irq = DMA + channel * 0x10
                self.write(irq + 0xc, 0xffffffff)
                # IN_DONE must map to RX, independently of OUT_DONE.
                self.write(irq + 8, 1)
                self.assertEqual(self.read(irq + 8), 1)
                self.assertEqual(self.read(out + 8) & 2, 2)
                self.write(out + 0x10, (txdesc & 0xfffff) | (1 << 21))
                self.assertEqual(self.read(out + 8) & 2, 0)
                self.assertEqual(self.read(irq), 0)  # Not completed yet.
                self.write(inp + 0x10, (rxdesc & 0xfffff) | (1 << 22))
                self.write(SPI + 0x30, (1 << 28) | (1 << 27))
                self.write(SPI + 0x10, (1 << 27) | (1 << 28))
                self.write(SPI + 0x1c, len(tx) * 8 - 1)
                self.write(SPI, 1 << 24)
                self.command('clock_step 1000000')
                self.assertEqual(self.read(SPI) & (1 << 24), 0)
                self.assertEqual(self.read(irq) & 0x11b, 0x11b)
                self.assertEqual(self.read(irq + 4), 1)
                self.assertEqual(self.read(out + 8) & 2, 2)
                # RX EOF comes from SPI even with a partially filled buffer.
                self.assertEqual(self.read(rxdesc), (1 << 30) | (len(tx) << 12) | 32)
                received = bytes.fromhex(self.command(f'read {rxbuf:#x} 8')[0][2:])
                self.assertEqual(received, b'\xff' * 7 + b'\x01')
                self.write(irq + 0xc, 1)
                self.assertEqual(self.read(irq + 4), 0)
                self.assertEqual(self.read(irq) & 0x11b, 0x11a)
                self.write(irq + 0xc, 0xffffffff)
                self.write(irq + 8, 0)

    def test_invalid_tx_owner_signals_error_before_spi_start(self):
        out, desc, _ = self.descriptor(0, False, b'abc', owner=False)
        self.write(out + 0x10, (desc & 0xfffff) | (1 << 21))
        self.assertEqual(self.read(out + 8) & 2, 2)
        self.assertEqual(self.read(DMA) & (1 << 6), 1 << 6)

    def test_tx_fifo_stop_reset_and_restart(self):
        out, desc, _ = self.descriptor(0, False, b'abc')
        start = (desc & 0xfffff) | (1 << 21)
        for _ in range(2):
            self.write(out + 0x10, start)
            self.assertEqual(self.read(out + 8) & 2, 0)
            self.write(out + 0x10, self.read(out + 0x10) | (1 << 20))
            self.assertEqual(self.read(out + 8) & 2, 2)
            self.assertEqual(self.read(out + 0x10) & (1 << 21), 0)
        self.write(out + 0x10, start)
        self.write(out, 1)
        self.write(out, 0)
        self.assertEqual(self.read(out + 8) & 2, 2)
        self.write(out + 0x10, start)
        self.qmp('system_reset')
        self.assertEqual(self.read(out + 8) & 2, 2)

    def test_memory_copy_drains_tx_fifo(self):
        data = b'copy through GDMA'
        out, txdesc, _ = self.descriptor(0, False, data)
        inp, rxdesc, rxbuf = self.descriptor(0, True, bytes(len(data)))
        self.write(inp, 1 << 4)  # Memory-to-memory mode.
        self.write(out + 0x10, (txdesc & 0xfffff) | (1 << 21))
        self.assertEqual(self.read(out + 8) & 2, 0)
        self.write(inp + 0x10, (rxdesc & 0xfffff) | (1 << 22))
        received = bytes.fromhex(self.command(f'read {rxbuf:#x} {len(data)}')[0][2:])
        self.assertEqual(received, data)
        self.assertEqual(self.read(out + 8) & 2, 2)


if __name__ == '__main__':
    unittest.main()
