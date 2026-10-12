"""Probe transport must retain coalesced lines and reject failed measurements."""
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]

class ProbeRunner(unittest.TestCase):
    def run_fake(self, greeting, response):
        with tempfile.TemporaryDirectory() as d:
            fake = Path(d)/'fake-qemu'
            fake.write_text('#!/usr/bin/env python3\nimport sys\n'
                + f'print({greeting!r}, flush=True)\n'
                + f'for line in sys.stdin: print({response!r}, flush=True)\n')
            fake.chmod(0o755)
            return subprocess.run([sys.executable,str(ROOT/'eink-qemu/x4cal/run_probe.py'),
                '--qemu',str(fake),'--flash',str(fake),'--sd',str(fake),
                '--log',str(Path(d)/'log'),'--timeout','2','info'],
                capture_output=True,text=True,timeout=5)

    def test_preserves_error_after_ready_in_same_read(self):
        r = self.run_fake('[CAL] READY\n[CAL] ERROR coalesced', '[CAL] DONE info')
        self.assertNotEqual(r.returncode,0)
        self.assertIn('ERROR coalesced',r.stderr)

    def test_rejects_error_counts_on_completion(self):
        r = self.run_fake('[CAL] READY','[CAL] DONE info errors=1')
        self.assertNotEqual(r.returncode,0)
        self.assertIn('errors=1',r.stderr)

    def test_accepts_clean_completion(self):
        self.assertEqual(self.run_fake('[CAL] READY','[CAL] DONE info errors=0').returncode,0)
