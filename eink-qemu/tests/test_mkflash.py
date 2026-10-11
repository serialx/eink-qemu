"""python3 -m unittest eink-qemu.tests.test_mkflash

Optional integration: set CROSSPOINT_RELEASE_APP and CROSSPOINT_REFERENCE_FLASH.
"""
import hashlib
from importlib import import_module
import os
from pathlib import Path
import struct
import tempfile
import unittest
mkflash = import_module('eink-qemu.mkflash')
mknvs = import_module('eink-qemu.mknvs')


class MkflashTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.table = mkflash.partition_table()

    def app(self, chip=5):
        data = bytearray(24)
        data[0] = 0xE9
        struct.pack_into('<H', data, 12, chip)
        return bytes(data) + b'app payload'

    def write(self, name, data):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
        return path

    def test_partition_reference(self):
        # SHA256 of CrossPoint 1.6.5 .pio/build/default/partitions.bin.
        self.assertEqual(hashlib.sha256(self.table).hexdigest(),
                         'bd0f7954aca2ef7d925ee21aaa1f3dc8822d1d6ce5cbbd26a135e5886bfff6ce')
        self.assertEqual(len(self.table), 0xC00)
        self.assertEqual(self.table[192:208], b'\xeb\xeb' + b'\xff' * 14)
        self.assertEqual(self.table[208:224], hashlib.md5(self.table[:192]).digest())

    def test_chip_detection(self):
        for chip, machine in ((5, 'x3'), (5, 'x4'), (9, 'x4pro')):
            self.assertEqual(mkflash.detect_chip(self.app(chip), machine), chip)
        for data in (b'', b'\xe9', b'X' * 24, self.app(0), self.app(261)):
            with self.assertRaises(ValueError):
                mkflash.detect_chip(data)
        with self.assertRaisesRegex(ValueError, 'conflicts'):
            mkflash.detect_chip(self.app(), 'x4pro')

    def test_release_composition(self):
        for chip, suffix in ((5, 'esp32c3'), (9, 'esp32s3')):
            app = self.app(chip)
            flash = mkflash.make_flash(self.write('app.bin', app))
            boot = (mkflash.BOOT_DIR / f'bootloader-{suffix}.bin').read_bytes()
            self.assertEqual(len(flash), mkflash.FLASH_SIZE)
            self.assertEqual(flash[:len(boot)], boot)
            self.assertEqual(flash[len(boot):0x8000], b'\xff' * (0x8000 - len(boot)))
            self.assertEqual(flash[0x8000:0x8C00], self.table)
            seed = mknvs.image(0x5000, 'cphw', {'dev_det': 2}) if chip == 5 else b'\xff' * 0x5000
            self.assertEqual(flash[0x9000:0xE000], seed)
            self.assertEqual(flash[0xE000:0x10000], b'\xff' * 0x2000)
            self.assertEqual(flash[0x10000:0x10000 + len(app)], app)
            self.assertEqual(flash[0x10000 + len(app):], b'\xff' * (mkflash.FLASH_SIZE - 0x10000 - len(app)))

    def test_x4_composition_does_not_force_x3_detection(self):
        app = self.app(5)
        flash = mkflash.make_flash(self.write('app.bin', app), 'x4')
        self.assertEqual(flash[0x9000:0xe000], b'\xff' * 0x5000)
        self.assertEqual(flash[0x10000:0x10000 + len(app)], app)
        with self.assertRaisesRegex(ValueError, 'conflicts'):
            mkflash.detect_chip(self.app(9), 'x4')

    def test_picco_composition_selects_ota_app_and_preserves_payload(self):
        app = self.app(9) + bytes(0x640000)
        flash = mkflash.make_flash(self.write('picco.bin', app), 'picco')
        table = flash[0x8000:0x8c00]
        self.assertEqual(mkflash.read_partitions(table), [
            (1, 2, 0x9000, 0xc000), (1, 0, 0x15000, 0x2000),
            (0, 0, 0x20000, 0xc0000), (0, 0x10, 0xe0000, 0xe70000),
            (1, 0x82, 0xf50000, 0xb0000),
        ])
        labels = [mkflash.ENTRY.unpack_from(table, pos)[5].rstrip(b'\0')
                  for pos in range(0, 5 * 32, 32)]
        self.assertEqual(labels, [b'nvs', b'otadata', b'safeboot', b'app', b'spiffs'])
        self.assertEqual(flash[0xe0000:0xe0000 + len(app)], app)
        self.assertEqual(flash[0x20000:0xe0000], b'\xff' * 0xc0000)
        self.assertEqual(flash[0x9000:0x15000], b'\xff' * 0xc000)
        self.assertEqual(flash[0xf50000:], b'\xff' * 0xb0000)
        # Valid ESP-IDF OTA record selects ota_0 even with a factory slot present.
        self.assertEqual(struct.unpack_from('<I', flash, 0x15000)[0], 1)
        self.assertEqual(flash[0x15004:0x15018], b'\xff' * 20)
        self.assertEqual(struct.unpack_from('<II', flash, 0x15018), (2, 0x4743989a))
        self.assertEqual(flash[0x15020:0x17000], b'\xff' * (0x2000 - 32))

    def test_picco_rejects_wrong_chip_oversize_and_wrong_build_layout(self):
        with self.assertRaisesRegex(ValueError, 'conflicts'):
            mkflash.make_flash(self.write('c3.bin', self.app()), 'picco')
        table = mkflash.partition_table(mkflash.BOOT_DIR / 'partitions-picco.csv')
        with self.assertRaisesRegex(ValueError, 'partition size'):
            mkflash.compose(self.app(9) + bytes(0xe70000), self.app(9), table, 'picco')
        with self.assertRaisesRegex(ValueError, '0xe0000'):
            mkflash.compose(self.app(9), self.app(9), self.table, 'picco')

    def test_picco_merged_dump_keeps_boot_selection_and_all_data(self):
        app = self.app(9)
        flash = bytearray(mkflash.make_flash(self.write('picco.bin', app), 'picco'))
        flash[0x15000:0x17000] = b'\xff' * 0x2000
        flash[0x20000:0x20000 + len(app)] = app
        flash[0xf50000:0xf50004] = b'FS!!'
        self.assertEqual(mkflash.make_flash(self.write('dump.bin', flash), 'picco'), flash)
        merged = flash[:0xf50004]
        expanded = mkflash.make_flash(self.write('merged.bin', merged), 'picco')
        self.assertEqual(expanded[:len(merged)], merged)
        self.assertEqual(expanded[len(merged):], b'\xff' * (mkflash.FLASH_SIZE - len(merged)))

    def test_pass_through(self):
        data = bytes(range(256)) * (mkflash.FLASH_SIZE // 256)
        self.assertEqual(mkflash.make_flash(self.write('flash.bin', data)), data)

    def test_short_merged_flash_preserves_layout(self):
        for chip in (5, 9):
            app = self.app(chip)
            merged = bytearray(b'\xff' * (0x10000 + len(app)))
            merged[:len(app)] = app
            merged[0x8000:0x8c00] = self.table
            merged[0x10000:] = app
            merged[0x9000:0x9004] = b'NVS!'
            flash = mkflash.make_flash(self.write('anything.bin', merged))
            self.assertEqual(flash[:len(merged)], merged)
            self.assertEqual(flash[len(merged):], b'\xff' * (mkflash.FLASH_SIZE - len(merged)))

    def test_invalid_merged_flash(self):
        merged = bytearray(b'\xff' * 0x10100)
        merged[:24] = self.app(9)[:24]
        merged[0x8000:0x8c00] = self.table
        with self.assertRaisesRegex(ValueError, 'application'):
            mkflash.make_flash(self.write('missing-app.bin', merged))
        merged[0x10000:0x10018] = self.app(5)[:24]
        with self.assertRaisesRegex(ValueError, 'application'):
            mkflash.make_flash(self.write('wrong-chip.bin', merged))
        with self.assertRaisesRegex(ValueError, 'truncated'):
            mkflash.make_flash(self.write('truncated.bin', merged[:0x8010]))
        merged[0x8010] ^= 1
        with self.assertRaisesRegex(ValueError, 'MD5'):
            mkflash.make_flash(self.write('bad-table.bin', merged))

    def test_build_directories(self):
        for nested in (False, True):
            with self.subTest(nested=nested):
                boot = 'bootloader/bootloader.bin' if nested else 'bootloader.bin'
                table = 'partition_table/partition-table.bin' if nested else 'partitions.bin'
                self.write(f'{nested}/{boot}', self.app())
                self.write(f'{nested}/{table}', self.table)
                self.write(f'{nested}/' + ('project.bin' if nested else 'firmware.bin'), self.app())
                flash = mkflash.make_flash(self.root / str(nested))
                self.assertEqual(flash, mkflash.compose(self.app(), self.app(), self.table))

    def test_invalid_inputs(self):
        with self.assertRaisesRegex(ValueError, 'app0'):
            mkflash.compose(self.app() + bytes(0x640000), self.app(), self.table)
        with self.assertRaisesRegex(ValueError, 'chips differ'):
            mkflash.compose(self.app(), self.app(9), self.table)
        with self.assertRaisesRegex(ValueError, 'bootloader overlaps'):
            mkflash.compose(self.app(), self.app() + bytes(0x8000), self.table)
        damaged = bytearray(self.table)
        damaged[208] ^= 1
        with self.assertRaisesRegex(ValueError, 'MD5'):
            mkflash.compose(self.app(), self.app(), damaged)
        with self.assertRaisesRegex(ValueError, 'bootloader.bin'):
            mkflash.make_flash(self.root)
        self.write('bootloader.bin', self.app())
        self.write('partitions.bin', self.table)
        self.write('one.bin', self.app())
        self.write('two.bin', self.app())
        with self.assertRaisesRegex(ValueError, 'exactly one app'):
            mkflash.make_flash(self.root)

    def test_official_release_when_available(self):
        app = Path(os.environ.get('CROSSPOINT_RELEASE_APP', 'crosspoint-1.6.5-x3-x4.bin'))
        reference = Path(os.environ.get('CROSSPOINT_REFERENCE_FLASH',
                                       'build-wasm-32limit/web-dist/flash.bin'))
        if not app.is_file() or not reference.is_file():
            self.skipTest('official release app or reference flash not supplied')
        self.assertEqual(mkflash.make_flash(app), reference.read_bytes())


if __name__ == '__main__':
    unittest.main()
