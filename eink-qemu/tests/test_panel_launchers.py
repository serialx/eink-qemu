"""Exercise launchers with a recording executable instead of a firmware VM."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class PanelLaunchers(unittest.TestCase):
    def test_native_and_node_preserve_machine_routing(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for relative in ('eink-qemu/run.sh', 'eink-qemu/turbo-args.sh',
                             'eink-qemu/wasm/run-node.sh'):
                target = root / relative
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(ROOT / relative, target)
            (root / 'build').mkdir()
            recorder = root / 'build/qemu-system-xtensa'
            recorder.write_text('#!/usr/bin/env python3\nimport json,sys\nprint(json.dumps(sys.argv[1:]))\n')
            recorder.chmod(0o755)
            shutil.copyfile(recorder, root / 'build/qemu-system-riscv32')
            (root / 'build/qemu-system-riscv32').chmod(0o755)
            flash, sd = root / 'flash.bin', root / 'sd.img'
            sd.touch()
            for launcher in ('run.sh', 'wasm/run-node.sh'):
                for machine, chip in (('x4pro', 9), ('x3', 5)):
                    flash.write_bytes(bytes(12) + bytes([chip]))
                    for panel in ('', 'uc8179', 'uc8279-68'):
                        with self.subTest(launcher=launcher, machine=machine, panel=panel):
                            env = {k: v for k, v in os.environ.items()
                                   if not k.startswith('X4')}
                            env.update(NODE=str(recorder), X4PANEL=panel,
                                       X4TURBO='1', TURBO='1')
                            result = subprocess.run(['bash', str(root / 'eink-qemu' / launcher),
                                                     str(flash), str(sd)], env=env,
                                                    capture_output=True, text=True, check=True)
                            args = json.loads(result.stdout)
                            expected = f'{machine},panel={panel}' if panel and machine == 'x4pro' else machine
                            self.assertEqual(args[args.index('-machine') + 1], expected)
                            self.assertIn('shift=0,sleep=off', args)
                            if machine == 'x4pro':
                                self.assertIn('uc8179.pon-ms=1', args)
                                self.assertIn('uc8179.pof-ms=1', args)
