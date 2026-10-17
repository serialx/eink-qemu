"""Peripheral selection and direction-specific GDMA START bits."""
import hashlib
import importlib
import unittest

boot = importlib.import_module('eink-qemu.tests.test_x4pro_boot')
DMA, SPI, SHA, AES = 0x6003f000, 0x60024000, 0x6003b000, 0x6003a000
RAM = 0x3fc90000


class GDMAPeripheral(boot.StockBootHarness):
    def channel(self, index, peripheral, data, *, rx=False, start=True):
        reg = DMA + index * 0xc0 + (0 if rx else 0x60)
        desc = RAM + index * 0x100 + (0x80 if rx else 0)
        buf = RAM + 0x1000 + index * 0x100 + (0x80 if rx else 0)
        self.command(f'write {buf:#x} {len(data)} 0x{data.hex()}')
        self.write(desc, (1 << 31) | (1 << 30) | len(data) |
                   (0 if rx else len(data) << 12))
        self.write(desc + 4, buf)
        self.write(desc + 8, 0)
        self.write(reg + 0x48, peripheral)
        self.write(reg + 0x20, (desc & 0xfffff) |
                   ((1 << (22 if rx else 21)) if start else 0))
        return reg, desc, buf

    def sha(self):
        self.write(SHA, 2)  # SHA-256, one padded block
        self.write(SHA + 0xc, 1)
        self.write(SHA + 0x1c, 1)

    def test_started_spi_and_sha_use_their_own_channels(self):
        spi, _, _ = self.channel(0, 0, bytes(64))
        padded = b'abc\x80' + bytes(52) + (24).to_bytes(8, 'big')
        sha, _, _ = self.channel(1, 7, padded)
        self.sha()
        self.assertEqual(self.read(sha + 8) & 0xb, 0xb)
        self.assertEqual(self.read(spi + 8), 0)
        self.assertTrue(self.read(spi + 0x20) & (1 << 21))
        digest = b''.join(self.read(SHA + 0x40 + i).to_bytes(4, 'little')
                          for i in range(0, 32, 4))
        self.assertEqual(digest, hashlib.sha256(b'abc').digest())
        self.write(SPI + 0x30, 1 << 28)
        self.write(SPI + 0x10, 1 << 27)
        self.write(SPI + 0x1c, 511)
        self.write(SPI, 1 << 24)
        self.command('clock_step 1000000')
        self.assertEqual(self.read(spi + 8) & 0xb, 0xb)
        self.assertFalse(self.read(spi + 0x20) & (1 << 21))

    def test_configured_but_unstarted_channel_is_not_consumed(self):
        sha, _, _ = self.channel(0, 7, bytes(64), start=False)
        self.sha()
        self.assertEqual(self.read(sha + 8), 0)
        self.assertEqual(self.read(sha + 0x30), 0)

    def test_aes_uses_input_start_bit_and_its_own_channels(self):
        other, _, _ = self.channel(0, 0, bytes(16))
        other_rx, _, _ = self.channel(0, 0, bytes(16), rx=True)
        out, _, _ = self.channel(1, 6, bytes(16))
        inp, desc, buf = self.channel(1, 6, bytes(16), rx=True)
        self.write(AES + 0x90, 1)
        self.write(AES + 0x98, 1)
        self.write(AES + 0x48, 1)
        self.assertEqual(self.read(out + 8) & 0xb, 0xb)
        self.assertEqual(self.read(inp + 8) & 3, 3)
        self.assertEqual(self.read(other + 8), 0)
        self.assertEqual(self.read(other_rx + 8), 0)
        self.assertEqual((self.read(desc) >> 12) & 0xfff, 16)
        encrypted = bytes.fromhex(self.command(f'read {buf:#x} 16')[0][2:])
        self.assertEqual(encrypted.hex(), '66e94bd4ef8a2c3b884cfa59ca342b2e')

    def test_input_stop_bit_does_not_start_aes(self):
        out, _, _ = self.channel(0, 6, bytes(16))
        inp, desc, _ = self.channel(0, 6, bytes(16), rx=True, start=False)
        self.write(inp + 0x20, (desc & 0xfffff) | (1 << 21))
        self.write(AES + 0x90, 1)
        self.write(AES + 0x98, 1)
        self.write(AES + 0x48, 1)
        self.assertEqual(self.read(inp + 8), 0)
        self.assertEqual(self.read(out + 8), 0)


if __name__ == '__main__':
    unittest.main()
