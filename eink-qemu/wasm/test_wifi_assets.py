import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import zlib

spec = importlib.util.spec_from_file_location('wifi_assets', Path(__file__).with_name('prepare-wifi-assets.py'))
assets = importlib.util.module_from_spec(spec)
spec.loader.exec_module(assets)


class AssetsTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.destination = Path(self.temp.name)
        self.font = b'font payload for integrity verification'
        self.entry = {'name': 'Alef_12.cpfont', 'size': len(self.font), 'crc32': zlib.crc32(self.font)}
        self.catalog = {'version': 1, 'baseUrl': 'https://fonts.example/', 'families': [
            {'name': 'Alef', 'files': [self.entry]}, {'name': 'Other', 'files': []}]}
        self.calls = []

    def download(self, url):
        self.calls.append(url)
        if url == assets.CATALOG:
            return json.dumps(self.catalog).encode()
        if url.endswith('.cpfont'):
            return self.font
        return b'Font license notice'

    def prepare(self):
        with patch.object(assets, 'download', self.download):
            assets.prepare(self.destination, families=['Alef'])

    def test_catalog_routes_licenses_and_cache(self):
        self.prepare()
        catalog = json.loads((self.destination / 'fonts.json').read_text())
        self.assertEqual([f['name'] for f in catalog['families']], ['Alef'])
        self.assertEqual(catalog['baseUrl'], 'https://fonts.example/')
        self.assertEqual((self.destination / 'licenses/Alef.txt').read_bytes(), b'Font license notice')
        index = json.loads((self.destination / 'index.json').read_text())
        self.assertEqual(index['urls']['https://fonts.example/Alef_12.cpfont'], 'Alef_12.cpfont')
        self.assertEqual(len(index['sources']['Alef_12.cpfont']['sha256']), 64)
        self.calls.clear()
        self.prepare()
        self.assertEqual(self.calls, [assets.CATALOG])

    def test_corrupt_cache_is_replaced(self):
        self.prepare()
        (self.destination / 'Alef_12.cpfont').write_bytes(b'corrupt')
        self.prepare()
        self.assertEqual((self.destination / 'Alef_12.cpfont').read_bytes(), self.font)

    def test_bad_download_never_publishes_catalog(self):
        self.entry['crc32'] ^= 1
        with self.assertRaisesRegex(ValueError, 'CRC mismatch'):
            self.prepare()
        self.assertFalse((self.destination / 'index.json').exists())
        self.assertFalse((self.destination / 'Alef_12.cpfont').exists())

    def test_unsafe_filename_is_rejected(self):
        self.entry['name'] = '../outside.cpfont'
        with self.assertRaisesRegex(ValueError, 'Unsafe font filename'):
            self.prepare()

    def test_packaging_prunes_unselected_fonts_and_notices(self):
        package = self.destination / 'web' / 'wifi-assets'
        cache = self.destination / 'cache'
        self.catalog['families'].append({'name': 'Literata', 'files': [
            {**self.entry, 'name': 'Literata_12.cpfont'}]})
        with patch.object(assets, 'download', self.download):
            assets.prepare(cache, families=['Alef', 'Literata'])
            assets.copy_bundle(cache, package)
            self.assertTrue((package / 'Literata_12.cpfont').exists())
            assets.prepare(cache, families=['Alef'])
            assets.copy_bundle(cache, package)
        self.assertTrue((cache / 'Literata_12.cpfont').exists(), 'keep cache for reuse')
        self.assertEqual(sorted(p.name for p in package.glob('*.cpfont')), ['Alef_12.cpfont'])
        self.assertEqual(sorted(p.name for p in (package / 'licenses').iterdir()), ['Alef.txt'])

    def test_bad_cache_preserves_previous_package(self):
        self.prepare()
        with tempfile.TemporaryDirectory() as output:
            package = Path(output) / 'wifi-assets'
            assets.copy_bundle(self.destination, package)
            (self.destination / self.entry['name']).write_bytes(b'corrupt')
            with self.assertRaisesRegex(ValueError, 'integrity check failed'):
                assets.copy_bundle(self.destination, package)
            self.assertEqual((package / self.entry['name']).read_bytes(), self.font)

    def test_packaging_rejects_overlapping_directories(self):
        with self.assertRaisesRegex(ValueError, 'must not overlap'):
            assets.copy_bundle(self.destination, self.destination)


if __name__ == '__main__':
    unittest.main()
