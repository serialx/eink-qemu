"""python3 -m unittest eink-qemu.tests.test_mksd

Requires dosfstools and mtools; no firmware or QEMU build needed.
"""
import hashlib
import os
from pathlib import Path
import shutil
import struct
import subprocess
import sys
import tempfile
import unittest


BUILDER = Path(__file__).resolve().parents[1] / 'mksd.py'
PARTITION_OFFSET = 1024 * 1024
EPOCH = 315532800  # 1980-01-01 00:00:00 UTC


@unittest.skipUnless(all(shutil.which(tool) for tool in
                         ('mkfs.vfat', 'mcopy', 'mmd', 'fsck.fat')),
                     'requires dosfstools and mtools')
class MksdTest(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)

    def command(self, args, env):
        result = subprocess.run(args, env=env, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0,
                         f'{args}\n{result.stdout}\n{result.stderr}')

    def build(self, name, source=None, *, timezone, epoch):
        image = self.root / name
        args = [sys.executable, str(BUILDER), str(image), '64']
        if source is not None:
            args.append(str(source))
        args += ['--cluster-size', '512']
        self.command(args, dict(os.environ, TZ=timezone, SOURCE_DATE_EPOCH=str(epoch)))
        return image

    def digest(self, image):
        digest = hashlib.sha256()
        with image.open('rb') as stream:
            for block in iter(lambda: stream.read(1024 * 1024), b''):
                digest.update(block)
        return digest.hexdigest()

    def check_volume_id(self, image):
        with image.open('rb') as stream:
            stream.seek(PARTITION_OFFSET)
            boot = stream.read(512)
            self.assertEqual(struct.unpack_from('<I', boot, 67)[0], 0x45494E4B)
            backup_sector = struct.unpack_from('<H', boot, 50)[0]
            stream.seek(PARTITION_OFFSET + backup_sector * 512)
            self.assertEqual(stream.read(512), boot)

    def test_empty_images_are_identical(self):
        empty = self.root / 'empty'
        empty.mkdir()
        first = self.build('blank.img', timezone='UTC+12', epoch=1700000000)
        second = self.build('empty.img', empty, timezone='UTC-9', epoch=1800000000)
        self.assertEqual(self.digest(first), self.digest(second))
        self.check_volume_id(first)

    def test_populated_images_are_identical_and_readable(self):
        payloads = {
            '.hidden': b'hidden\n',
            'empty.txt': b'',
            'long filename one.txt': b'first alias\n',
            'long filename two.txt': b'second alias\n',
            'books/nested/payload.bin': bytes(range(256)) * 64,
        }
        # Grow root and nested directories beyond one 512-byte cluster, and
        # exercise short-name collisions whose assignment depends on copy order.
        for index in range(24):
            for directory in ('', 'books/'):
                payloads[f'{directory}chapter number {index:02}.txt'] = str(index).encode()

        sources = []
        source_times = []
        for name, entries, epoch in (
                ('first', list(payloads.items()), 1700000000),
                ('second', list(reversed(payloads.items())), 1800000000)):
            source = self.root / name
            for relative, data in entries:
                path = source / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(data)
            (source / 'books/empty directory').mkdir()
            paths = [source, *source.rglob('*')]
            for path in paths:
                os.utime(path, (epoch, epoch))
            sources.append(source)
            source_times.append({path: path.stat().st_mtime_ns for path in paths})

        first = self.build('first.img', sources[0], timezone='UTC+12', epoch=1700000000)
        second = self.build('second.img', sources[1], timezone='UTC-9', epoch=1800000000)
        self.assertEqual(self.digest(first), self.digest(second))
        self.check_volume_id(first)
        for timestamps in source_times:
            self.assertEqual({path: path.stat().st_mtime_ns for path in timestamps}, timestamps)

        # Validate the filesystem and contents with tools independent of the
        # builder, including fixed modification times after extraction.
        env = dict(os.environ, TZ='UTC')
        partition = self.root / 'partition.img'
        with first.open('rb') as source, partition.open('wb') as target:
            source.seek(PARTITION_OFFSET)
            shutil.copyfileobj(source, target)
        self.command(['fsck.fat', '-n', str(partition)], env)
        extracted = self.root / 'extracted'
        extracted.mkdir()
        self.command(['mcopy', '-s', '-m', '-i', f'{first}@@{PARTITION_OFFSET}',
                      '::*', str(extracted)], env)
        actual = {str(path.relative_to(extracted)): path.read_bytes()
                  for path in extracted.rglob('*') if path.is_file()}
        self.assertEqual(actual, payloads)
        self.assertTrue((extracted / 'books/empty directory').is_dir())
        for path in extracted.rglob('*'):
            self.assertEqual(path.stat().st_mtime, EPOCH, str(path))


if __name__ == '__main__':
    unittest.main()
