import importlib.util
from pathlib import Path
import struct
import subprocess
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("stage_linux", Path(__file__).parents[1] / "desktop/scripts/bridge/stage_linux.py")
staging = importlib.util.module_from_spec(spec)
spec.loader.exec_module(staging)


class LinuxBundleTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="bridge packaging ")
        self.addCleanup(self.temp.cleanup)
        root = Path(self.temp.name)
        self.source, self.output, self.repo = root / "QEMU install", root / "existing bundle", root / "repo"
        self.cli, self.helper = root / "CLI binary", root / "service binary"
        header = bytearray(64)
        header[:6] = b"\x7fELF\x02\x01"
        struct.pack_into("<H", header, 18, 183)
        self.binaries = [self.source / name for name in ["bin/qemu-system-riscv32", "bin/qemu-system-xtensa", "plugins/libheaptrace.so", "libexec/eink-qemu-bridge-helper"]] + [self.cli, self.helper]
        for path in self.binaries:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(header)
        for name in ["pc-bios/esp32c3-rom.bin", "pc-bios/esp32s3_rev0_rom.bin", "COPYING"]:
            path = self.repo / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(b"fixture data")
        self.output.mkdir()
        self.marker = self.output / "previous-success"
        self.marker.write_text("preserve me")
        self.readelf = patch.object(staging.subprocess, "run", return_value=subprocess.CompletedProcess([], 0,
            stdout="(NEEDED) Shared library: [libc.so.6]\nName: GLIBC_2.35", stderr="")).start()
        self.addCleanup(patch.stopall)

    def stage(self):
        staging.stage(self.source, self.output, self.repo, "aarch64-unknown-linux-gnu", self.cli, self.helper)

    def test_missing_payload_preserves_existing_bundle(self):
        self.binaries[1].unlink()
        with self.assertRaises(ValueError):
            self.stage()
        self.assertEqual(self.marker.read_text(), "preserve me")

    def test_wrong_architecture_or_symlink_preserves_existing_bundle(self):
        self.cli.write_bytes(b"not an ELF binary")
        with self.assertRaises(ValueError):
            self.stage()
        self.cli.unlink()
        self.cli.symlink_to(self.helper)
        with self.assertRaises(ValueError):
            self.stage()
        self.assertTrue(self.marker.exists())

    def test_newer_libc_or_build_search_path_is_not_shipped(self):
        for metadata in ["Name: GLIBC_2.36", "(RUNPATH) Library runpath: [/home/builder/lib]"]:
            self.readelf.return_value.stdout = metadata
            with self.assertRaises(ValueError):
                self.stage()
            self.assertTrue(self.marker.exists())

    def test_failed_publish_restores_old_bundle(self):
        original = Path.rename
        def rename(path, destination):
            if path.name.startswith(".existing bundle-stage-"):
                raise OSError("simulated publish failure")
            return original(path, destination)
        with patch.object(Path, "rename", rename), self.assertRaises(OSError):
            self.stage()
        self.assertEqual(self.marker.read_text(), "preserve me")

    def test_failed_restore_keeps_previous_bundle_backup(self):
        original = Path.rename
        def rename(path, destination):
            if path != self.output:
                raise OSError("simulated directory failure")
            return original(path, destination)
        with patch.object(Path, "rename", rename), self.assertRaises(OSError):
            self.stage()
        backups = list(self.output.parent.glob(".existing bundle-previous-*"))
        self.assertEqual(len(backups), 1)
        self.assertEqual((backups[0] / "previous-success").read_text(), "preserve me")

    def test_staging_paths_with_spaces_and_nonprivileged_modes(self):
        self.stage()
        self.assertFalse(self.marker.exists())
        self.assertEqual((self.output / "bridge/eink-qemu-bridge-helper").stat().st_mode & 0o7777, 0o755)
        self.assertTrue((self.output / "binaries/eink-qemu-aarch64-unknown-linux-gnu").exists())
        self.assertTrue((self.output / "qemu/bin/qemu-system-xtensa").exists())
        self.assertIn('"libc.so.6"', (self.output / "native-dependencies.json").read_text())
        self.assertFalse(list(self.output.parent.glob(".existing bundle-previous-*")))


if __name__ == "__main__":
    unittest.main()
