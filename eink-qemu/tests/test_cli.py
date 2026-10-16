"""The command-line tool and the QEMU hooks it uses.

Build QEMU and the tool first (cargo build -p eink-qemu-cli in
eink-qemu/desktop), then run:
    python3 -m unittest eink-qemu.tests.test_cli
EINK_QEMU_TEST_FIRMWARE=<CrossPoint X3 build dir or .bin> adds a full run;
EINK_QEMU_TEST_STOCK_FLASH=<a stock Xteink X4 Pro 16 MiB dump> adds a run of
firmware eink-qemu has no CrossPoint knowledge for.
"""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
CLI = Path(os.environ.get('EINK_QEMU_CLI', ROOT / 'eink-qemu/desktop/target/debug/eink-qemu'))
RISCV = ROOT / 'build/qemu-system-riscv32'
XTENSA = ROOT / 'build/qemu-system-xtensa'
FIRMWARE = os.environ.get('EINK_QEMU_TEST_FIRMWARE')
STOCK = os.environ.get('EINK_QEMU_TEST_STOCK_FLASH')


def app_image(chip, data=bytes(range(1, 17))):
    """A minimal valid ESP app: one data segment and its checksum."""
    image = bytearray(24)
    image[0], image[1] = 0xE9, 1
    image[12:14] = chip.to_bytes(2, 'little')
    image += (0x3FC80000).to_bytes(4, 'little') + len(data).to_bytes(4, 'little') + data
    checksum = 0xEF
    for b in data:
        checksum ^= b
    image += bytes(63 - len(image)) + bytes([checksum])
    return bytes(image)


def cli(*args, **kwargs):
    return subprocess.run([str(CLI), *args], capture_output=True, text=True, **kwargs)


def records(stdout):
    return [json.loads(line) for line in stdout.splitlines() if line.startswith('{')]


def qmp(binary, machine, commands):
    script = '{"execute":"qmp_capabilities"}\n' + ''.join(json.dumps(c) + '\n' for c in commands) + '{"execute":"quit"}\n'
    out = subprocess.run([str(binary), '-machine', machine, '-display', 'none', '-serial', 'null',
                          '-object', 'guest-clock,id=clock', '-qmp', 'stdio'],
                         input=script, capture_output=True, text=True, timeout=30).stdout
    return [json.loads(line) for line in out.splitlines() if '"return"' in line or '"error"' in line]


@unittest.skipUnless(CLI.exists(), 'build the tool: cargo build -p eink-qemu-cli in eink-qemu/desktop')
class Cli(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.addCleanup(self.dir.cleanup)
        self.tmp = Path(self.dir.name)

    def test_flash_matches_mkflash(self):
        sys.path.insert(0, str(ROOT))
        from importlib import import_module
        mkflash = import_module('eink-qemu.mkflash')
        for chip, machine in [(5, None), (5, 'x4'), (9, None), (9, 'picco')]:
            with self.subTest(chip=chip, machine=machine):
                app = self.tmp / f'app-{chip}-{machine}.bin'
                app.write_bytes(app_image(chip))
                out = self.tmp / 'flash.bin'
                args = ['flash', str(app), str(out)] + (['--machine', machine] if machine else [])
                result = cli(*args)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(out.read_bytes(), mkflash.make_flash(app, machine))

    def test_flash_partitions_and_crosspoint_seed(self):
        sys.path.insert(0, str(ROOT))
        from importlib import import_module
        mkflash = import_module('eink-qemu.mkflash')
        table = self.tmp / 'partitions.csv'
        table.write_text('nvs, data, nvs, , 24K\nphy_init, data, phy, , 4K\nfactory, app, factory, , 1M\n')
        cases = [(app_image(9), table, None), (app_image(5, b'cphw\0dev_det\0\0\0\0'), None, 'NVS: CrossPoint'),
                 (app_image(5, b'cphw\0dev_det\0\0\0\0'), table, 'NVS: CrossPoint'), (app_image(5), None, None)]
        for i, (data, partitions, note) in enumerate(cases):
            with self.subTest(case=i):
                app = self.tmp / f'app{i}.bin'
                app.write_bytes(data)
                out = self.tmp / 'flash.bin'
                result = cli('flash', str(app), str(out), *(['--partitions', str(partitions)] if partitions else []))
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(out.read_bytes(), mkflash.make_flash(app, None, partitions))
                self.assertEqual(note is not None, 'NVS: CrossPoint' in result.stdout, result.stdout)
                self.assertIn('partitions.csv' if partitions else 'Arduino', result.stdout)

    def test_bad_steps_are_usage_errors(self):
        result = cli('run', str(self.tmp / 'missing.bin'), 'ready', 'wait-logg:x')
        self.assertEqual(result.returncode, 2)
        error = records(result.stdout)[0]
        self.assertEqual(error['stage'], 'usage')
        self.assertIn('wait-log', error['error'])
        result = cli('run', str(self.tmp / 'missing.bin'), 'repeat:2', 'press:down')
        self.assertEqual(result.returncode, 2)
        self.assertIn('no matching `end`', records(result.stdout)[0]['error'])

    def test_bad_limits_and_names_are_usage_errors(self):
        app = self.tmp / 'x3.bin'
        app.write_bytes(app_image(5))
        self.assertEqual(cli('run', str(app), '--timeout', 'NaN', 'ready').returncode, 2)
        self.assertEqual(cli('run', str(app), 'wait:1e300').returncode, 2)
        self.assertEqual(cli('run', str(app), '--crash-pattern', '[', 'ready').returncode, 2)
        result = cli('compare', str(app), str(app), '--names', '..,x', '--out', str(self.tmp / 'c'), 'ready')
        self.assertEqual(result.returncode, 2)
        self.assertFalse((self.tmp / 'card.img').exists())

    def test_truncated_firmware_is_rejected(self):
        app = self.tmp / 'short.bin'
        app.write_bytes(app_image(5)[:40])
        result = cli('run', str(app), '--out', str(self.tmp / 'run'), 'ready')
        self.assertEqual(result.returncode, 3)
        self.assertIn('not a complete ESP application image', records(result.stdout)[0]['error'])

    def test_missing_firmware_is_a_setup_error(self):
        result = cli('run', str(self.tmp / 'missing.bin'), '--out', str(self.tmp / 'run'), 'ready')
        self.assertEqual(result.returncode, 3)
        self.assertIn('does not exist', records(result.stdout)[0]['error'])

    def test_wrong_key_fails_before_boot(self):
        app = self.tmp / 'x3.bin'
        app.write_bytes(app_image(5))
        result = cli('run', str(app), '--out', str(self.tmp / 'run'), '--qemu', str(RISCV.parent), 'press:home')
        self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
        self.assertIn('back, confirm, left, right, up, down, power', records(result.stdout)[-1]['error'])

    def test_sd_round_trip(self):
        card = self.tmp / 'card.img'
        book = self.tmp / 'book.txt'
        book.write_text('hello')
        self.assertEqual(cli('sd', 'new', str(card)).returncode, 0)
        result = cli('sd', 'put', str(card), str(book), '--to', '/books')
        self.assertEqual(result.returncode, 0, result.stdout)
        listing = json.loads(cli('sd', 'ls', str(card), '/books').stdout)
        self.assertEqual([e['name'] for e in listing['entries']], ['book.txt'])
        back = self.tmp / 'back.txt'
        self.assertEqual(cli('sd', 'get', str(card), '/books/book.txt', str(back)).returncode, 0)
        self.assertEqual(back.read_text(), 'hello')

    @unittest.skipUnless(FIRMWARE, 'set EINK_QEMU_TEST_FIRMWARE to a CrossPoint X3 build or .bin')
    @unittest.skipUnless(RISCV.exists(), 'build qemu-system-riscv32 first')
    def test_run_crosspoint(self):
        out = self.tmp / 'run'
        result = cli('run', FIRMWARE, '--out', str(out), 'ready', 'shot:home', 'heap:home', 'mark:reboot', 'reset', 'ready', timeout=300)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        steps = [r for r in records(result.stdout) if r['type'] == 'step']
        self.assertEqual([s['step'] for s in steps], ['ready', 'shot', 'heap', 'mark', 'reset', 'ready'])
        self.assertEqual(steps[0]['how'], 'home')
        self.assertGreater(steps[3]['screen_updates'], 0)
        self.assertGreaterEqual(steps[3]['panel_refreshes'], 3)
        metrics = records(result.stdout)[-1]['metrics']
        self.assertIn('crosspoint', metrics)
        self.assertGreater(metrics['panel_refreshes'], steps[3]['panel_refreshes'])
        steps.pop(3)
        self.assertTrue((out / 'shots/home.png').exists())
        self.assertGreater(steps[2]['dram_free'], 0)
        # ready after a reset waits for the new boot.
        self.assertGreater(steps[4]['ms'], 3000)
        self.assertIn('PASS', (out / 'report.html').read_text())
        self.assertTrue(list((out / 'frames').glob('*.png')))

    @unittest.skipUnless(STOCK, 'set EINK_QEMU_TEST_STOCK_FLASH to a stock X4 Pro flash dump')
    @unittest.skipUnless(XTENSA.exists(), 'build qemu-system-xtensa first')
    def test_run_stock_firmware(self):
        out = self.tmp / 'stock'
        result = cli('run', STOCK, '--machine', 'x4pro', '--power-hold', '3000', '--out', str(out),
                     'ready', 'shot:boot', 'heap:boot', timeout=300)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        steps = [r for r in records(result.stdout) if r['type'] == 'step']
        self.assertEqual(steps[0]['how'], 'screen-quiet')
        self.assertGreater(steps[2]['free'], 0)
        end = records(result.stdout)[-1]
        self.assertNotIn('crosspoint', end['metrics'])
        self.assertGreater(end['metrics']['panel_refreshes'], 0)


class QemuHooks(unittest.TestCase):
    @unittest.skipUnless(RISCV.exists(), 'build qemu-system-riscv32 first')
    def test_guest_clock_and_panel_busy_x3(self):
        replies = qmp(RISCV, 'x3', [
            {'execute': 'qom-get', 'arguments': {'path': '/objects/clock', 'property': 'ns'}},
            {'execute': 'stop'},
            {'execute': 'qom-get', 'arguments': {'path': '/objects/clock', 'property': 'ns'}},
            {'execute': 'qom-get', 'arguments': {'path': '/objects/clock', 'property': 'ns'}},
            {'execute': 'qom-get', 'arguments': {'path': '/machine/peripheral/panel', 'property': 'busy'}},
        ])
        values = [r.get('return') for r in replies]
        self.assertIsInstance(values[1], int)
        self.assertEqual(values[3], values[4], 'the guest clock stops while the VM is stopped')
        self.assertIs(values[5], False)

    @unittest.skipUnless(XTENSA.exists(), 'build qemu-system-xtensa first')
    def test_panel_busy_x4pro_and_picco(self):
        for machine in ['x4pro', 'picco']:
            with self.subTest(machine=machine):
                replies = qmp(XTENSA, machine, [
                    {'execute': 'qom-get', 'arguments': {'path': '/machine/peripheral/panel', 'property': 'busy'}},
                ])
                self.assertIs(replies[1].get('return'), False)

    def test_panel_counters(self):
        for binary, machine in [(RISCV, 'x3'), (RISCV, 'x4'), (XTENSA, 'x4pro'), (XTENSA, 'picco')]:
            if not binary.exists():
                continue
            with self.subTest(machine=machine):
                replies = qmp(binary, machine, [
                    {'execute': 'qom-get', 'arguments': {'path': '/machine/peripheral/panel', 'property': 'refreshes'}},
                    {'execute': 'qom-get', 'arguments': {'path': '/machine/peripheral/panel', 'property': 'refresh-ns'}},
                ])
                self.assertEqual([r.get('return') for r in replies[1:3]], [0, 0])


if __name__ == '__main__':
    unittest.main()
