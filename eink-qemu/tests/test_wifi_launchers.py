"""Check native network selection without changing the host's networking."""
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class WifiLauncher(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="wifi's test, ")
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        for relative in ('eink-qemu/run.sh', 'eink-qemu/turbo-args.sh'):
            target = self.root / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(ROOT / relative, target)
        (self.root / 'bin').mkdir()
        recorder = self.root / 'bin/qemu-system-riscv32'
        recorder.write_text('#!/usr/bin/env python3\nimport json,sys\nprint(json.dumps(sys.argv[1:]))\n')
        recorder.chmod(0o755)
        helper = self.root / 'bin/qemu-bridge-helper'
        shutil.copyfile(recorder, helper)
        helper.chmod(0o755)
        uname = self.root / 'bin/uname'
        uname.write_text('#!/bin/sh\nprintf "%s\\n" "$TEST_HOST"\n')
        uname.chmod(0o755)
        self.flash, self.sd = self.root / 'flash.bin', self.root / 'sd.img'
        self.flash.write_bytes(bytes(12) + bytes([5]))
        self.sd.touch()

    def launch(self, host, **variables):
        env = {k: v for k, v in os.environ.items() if not k.startswith('X4')}
        env.update(TEST_HOST=host, PATH=f"{self.root / 'bin'}{os.pathsep}{env['PATH']}", **variables)
        return subprocess.run(['sh', str(self.root / 'eink-qemu/run.sh'), str(self.flash),
                               str(self.sd), '-name', 'extra argument'],
                              env=env, capture_output=True, text=True, timeout=10)

    def nic(self, host, **variables):
        result = self.launch(host, **variables)
        self.assertEqual(result.returncode, 0, result.stderr)
        args = json.loads(result.stdout)
        self.assertEqual(args[-2:], ['-name', 'extra argument'])
        self.assertEqual(args.count('-nic'), 1)
        return args[args.index('-nic') + 1]

    def test_modes_and_overrides_on_each_native_platform(self):
        nat = 'user,model=esp32_wifi,hostfwd=tcp::8080-:80'
        for host, backend, interface in [
            ('Linux', 'bridge,br=', 'br0'), ('Darwin', 'vmnet-bridged,ifname=', 'en0'),
            ('MINGW64_NT', 'tap,ifname=', 'eink-qemu'),
            ('MSYS_NT', 'tap,ifname=', 'eink-qemu'), ('CYGWIN_NT', 'tap,ifname=', 'eink-qemu'),
        ]:
            with self.subTest(host=host):
                self.assertEqual(self.nic(host), nat)
                self.assertEqual(self.nic(host, X4BR='unused', X4WIFI_MODE='nat'), nat)
                bridge = self.nic(host, X4WIFI_MODE='bridge')
                self.assertTrue(bridge.startswith(f'{backend}{interface},model=esp32_wifi'), bridge)
                for variables in ({'X4BR': 'LAN,2'}, {'X4BR': 'LAN,2', 'X4WIFI_MODE': 'bridge'}):
                    self.assertTrue(self.nic(host, **variables).startswith(f'{backend}LAN,,2,model=esp32_wifi'))
                self.assertEqual(self.nic(host, X4WIFI_MODE='bridge', X4NET='-nic none'), 'none')
                result = self.launch(host, X4NET='')
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertNotIn('-nic', json.loads(result.stdout))
                result = self.launch(host, X4WIFI_MODE='typo')
                self.assertEqual(result.returncode, 2)
                self.assertIn('X4WIFI_MODE must be nat or bridge', result.stderr)
        self.assertEqual(self.nic('Darwin', X4BR='LAN adapter'), 'vmnet-bridged,ifname=LAN adapter,model=esp32_wifi')
        self.assertEqual(self.nic('MINGW64_NT', X4BR='LAN adapter'), 'tap,ifname=LAN adapter,model=esp32_wifi')

    def test_linux_helper_path_survives_qemu_shell_interpretation(self):
        nic = self.nic('Linux', X4WIFI_MODE='bridge')
        helper = nic.split(',helper=', 1)[1].replace(',,', ',')
        self.assertEqual(shlex.split(helper), [str(self.root / 'bin/qemu-bridge-helper')])
        result = subprocess.run(['sh', '-c', helper + ' --use-vnet --fd=3 --br=br0'],
                                capture_output=True, text=True, check=True)
        self.assertEqual(json.loads(result.stdout), ['--use-vnet', '--fd=3', '--br=br0'])
