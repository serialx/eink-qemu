import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest.mock import patch
import zipfile

spec = importlib.util.spec_from_file_location("stage_dependency", Path(__file__).parents[1] / "desktop/scripts/bridge/stage_dependency.py")
dependency = importlib.util.module_from_spec(spec)
spec.loader.exec_module(dependency)


class DependencyTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.archive = self.root / "release.zip"
        with zipfile.ZipFile(self.archive, "w") as archive:
            archive.writestr("driver.bin", b"pinned bytes")
            archive.writestr("unused-installer.exe", b"do not ship")
        self.output = self.root / "bundle"
        self.output.mkdir()
        (self.output / "previous").write_text("keep")
        self.manifest = {"version": 1, "targets": {"test": {
            "archive": {"format": "zip", "sha256": hashlib.sha256(self.archive.read_bytes()).hexdigest()},
            "files": [{"member": "driver.bin", "path": "driver/payload", "size": 12,
                       "sha256": hashlib.sha256(b"pinned bytes").hexdigest(), "mode": "0644"}]}}}
        self.manifest_path = self.root / "manifest.json"
        self.addCleanup(patch.stopall)
        patch.object(dependency, "MANIFEST", self.manifest_path).start()

    def stage(self):
        self.manifest_path.write_text(json.dumps(self.manifest))
        dependency.stage("test", self.archive, self.output)

    def test_only_verified_payloads_replace_previous_bundle(self):
        self.stage()
        self.assertEqual((self.output / "driver/payload").read_bytes(), b"pinned bytes")
        self.assertFalse((self.output / "unused-installer.exe").exists())
        self.assertFalse((self.output / "previous").exists())
        # Windows chmod only controls the read-only flag, not POSIX mode bits.
        if os.name != "nt":
            self.assertEqual((self.output / "driver/payload").stat().st_mode & 0o7777, 0o644)

    def test_changed_archive_or_payload_preserves_previous_bundle(self):
        entry = self.manifest["targets"]["test"]
        for value in [entry["archive"], entry["files"][0]]:
            original = value["sha256"]
            value["sha256"] = "0" * 64
            with self.assertRaises(ValueError):
                self.stage()
            self.assertEqual((self.output / "previous").read_text(), "keep")
            value["sha256"] = original

    def test_links_and_unsafe_destination_modes_are_refused(self):
        entry = self.manifest["targets"]["test"]
        for field, value in [("path", "../outside"), ("mode", "4755")]:
            original = entry["files"][0][field]
            entry["files"][0][field] = value
            with self.assertRaises(ValueError):
                self.stage()
            entry["files"][0][field] = original
        self.archive = self.root / "release.tar.gz"
        with tarfile.open(self.archive, "w:gz") as archive:
            member = tarfile.TarInfo("driver.bin")
            member.type = tarfile.SYMTYPE
            member.linkname = "outside"
            archive.addfile(member)
        entry["archive"] = {"format": "tar.gz", "sha256": hashlib.sha256(self.archive.read_bytes()).hexdigest()}
        with self.assertRaises(ValueError):
            self.stage()
        self.assertEqual((self.output / "previous").read_text(), "keep")

    def test_failed_publish_restores_previous_bundle(self):
        original = Path.rename
        def rename(path, destination):
            if path.name.startswith(".bundle-stage-"):
                raise OSError("publish failed")
            return original(path, destination)
        with patch.object(Path, "rename", rename), self.assertRaises(OSError):
            self.stage()
        self.assertEqual((self.output / "previous").read_text(), "keep")


if __name__ == "__main__":
    unittest.main()
