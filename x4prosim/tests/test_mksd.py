"""python3 -m unittest discover -s x4prosim/tests -p test_mksd.py"""
from pathlib import Path
import shutil
import struct
import subprocess
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().parents[1] / 'mksd.py'


@unittest.skipUnless(all(shutil.which(tool) for tool in ('mkfs.vfat', 'mcopy', 'mtype')),
                     'requires dosfstools and mtools')
class MksdTest(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        self.image = self.root / 'sd.img'

    def create(self, *args):
        return subprocess.run([sys.executable, str(SCRIPT), str(self.image),
                               *map(str, args)], capture_output=True, text=True)

    def check_geometry(self, cluster_bytes):
        with self.image.open('rb') as source:
            mbr = source.read(512)
            self.assertEqual(mbr[510:512], b'\x55\xaa')
            self.assertEqual(mbr[450], 0x0c)
            start, sectors = struct.unpack_from('<II', mbr, 454)
            self.assertEqual(start, 2048)
            self.assertEqual((start + sectors) * 512, self.image.stat().st_size)
            source.seek(start * 512)
            boot = source.read(512)
        bps = struct.unpack_from('<H', boot, 11)[0]
        self.assertEqual(bps, 512)
        self.assertEqual(bps * boot[13], cluster_bytes)
        reserved = struct.unpack_from('<H', boot, 14)[0]
        fat_sectors = struct.unpack_from('<I', boot, 36)[0]
        total = struct.unpack_from('<I', boot, 32)[0]
        clusters = (total - reserved - boot[16] * fat_sectors) // boot[13]
        self.assertGreaterEqual(clusters, 65525, 'too few clusters for FAT32')
        self.assertLessEqual(total, sectors)

    def test_default_matches_stock_x3_cluster_geometry(self):
        result = self.create()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.check_geometry(32768)

    def test_undersized_default_does_not_truncate_existing_image(self):
        self.image.write_bytes(b'existing image')
        result = self.create(2049)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('2050', result.stderr)
        self.assertEqual(self.image.read_bytes(), b'existing image')

    def test_default_geometry_copies_hidden_and_nested_files(self):
        tree = self.root / 'tree'
        (tree / '.fonts').mkdir(parents=True)
        payload = b'font bytes\x00\xff' * 4000
        (tree / '.fonts/font.cpfont').write_bytes(payload)
        result = self.create(2050, tree)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.check_geometry(32768)
        data = subprocess.check_output(['mtype', '-i', f'{self.image}@@1048576',
                                        '::/.fonts/font.cpfont'])
        self.assertEqual(data, payload)

    def test_compact_image_requires_explicit_cluster_override(self):
        result = self.create(64, '--cluster-size', 512)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.check_geometry(512)


if __name__ == '__main__':
    unittest.main()
