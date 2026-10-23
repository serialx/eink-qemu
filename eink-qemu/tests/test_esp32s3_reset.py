"""CPU-only ESP32-S3 resets must preserve the other core and peripherals."""
from importlib import import_module
import re
import unittest

boot = import_module('eink-qemu.tests.test_x4pro_boot')
guest = import_module('eink-qemu.tests.test_xtensa_cost')


class CpuReset(boot.StockBootHarness):
    extra_qemu_args = [
        '-device', 'loader,addr=0x40374000,cpu-num=0',
        '-device', 'loader,addr=0x40374100,cpu-num=1',
    ]

    def pcs(self):
        registers = self.qmp('human-monitor-command',
                             **{'command-line': 'info registers -a'})
        return [int(pc, 16) for pc in re.findall(r'\bPC=([0-9a-f]+)', registers)]

    def test_cpu_reset_preserves_other_core_and_shared_devices(self):
        for mask, cores in [(1 << 5, [0]), (1 << 4, [1]),
                            ((1 << 5) | (1 << 4), [0, 1])]:
            with self.subTest(cores=cores):
                self.qmp('system_reset')
                self.assertEqual(self.pcs(), [0x40374000, 0x40374100])
                # The running firmware has released Core 1, enabled caches,
                # and mapped flash. None of this belongs to a CPU-only reset.
                values = {0x600c0000: 2, 0x600c0010: 2,
                          0x600c0004: 0x40374100,
                          0x600c4000: 1, 0x600c4060: 1,
                          0x600c5000: 1}
                for addr, value in values.items():
                    self.write(addr, value)
                self.command('clock_step 1000000')
                timer = self.systimer_value()
                self.write(0x60008000, self.read(0x60008000) | mask)
                expected = [0x40374000, 0x40374100]
                for core in cores:
                    expected[core] = 0x40000400
                self.assertEqual(self.pcs(), expected)
                for addr, value in values.items():
                    self.assertEqual(self.read(addr), value, hex(addr))
                self.assertEqual(self.systimer_value(), timer)
                causes = self.read(0x60008038)
                for core in (0, 1):
                    self.assertEqual((causes >> (6 * core)) & 0x3f,
                                     12 if core in cores else 1)

    def test_full_reset_still_resets_shared_devices(self):
        for digital in (False, True):
            with self.subTest(digital=digital):
                self.write(0x600c0000, 2)
                self.write(0x600c4000, 1)
                self.command('clock_step 1000000')
                if digital:
                    self.write(0x60008000, 1 << 31)
                    self.qmp('query-status')  # wait for the deferred reset
                else:
                    self.qmp('system_reset')
                self.assertEqual(self.read(0x600c0000), 4)
                self.assertEqual(self.read(0x600c4000), 0)
                self.assertEqual(self.systimer_value(), 0)


class NoReboot(boot.StockBootHarness):
    extra_qemu_args = ['-no-reboot']

    def test_only_pro_cpu_reset_exits(self):
        self.write(0x60008000, 1 << 4)
        self.qmp('query-status')
        self.assertIsNone(self.proc.poll())
        self.write(0x60008000, 1 << 5)
        self.assertEqual(self.proc.wait(timeout=5), 0)


@unittest.skipUnless(guest.QEMU.exists() and guest.CC.exists(),
                     'requires Xtensa QEMU and compiler')
class SelfReset(unittest.TestCase):
    execute = guest.XtensaCosts.execute

    def test_guest_self_reset_reenters_reset_vector_and_keeps_clock(self):
        # -kernel installs a reset-vector trampoline back to _start. Keep a
        # marker outside the ELF so the second entry can check retained state.
        setup = [
            'movi a2, 0x3fc92000', 'l32i a3, a2, 0',
            'bnez a3, reset_returned',
            'movi a3, 1', 's32i a3, a2, 0',
            'movi a2, 0x600c0010', 'movi a3, 2', 's32i a3, a2, 0',
            'movi a2, 0x60008000', 'movi a3, 32', 's32i a3, a2, 0',
            'memw', 'self_reset_failed: j self_reset_failed',
            'reset_returned:',
            'movi a2, 0x600c0010', 'l32i a3, a2, 0',
            'bnei a3, 2, self_reset_failed',
        ]
        self.assertEqual(self.execute([], setup=setup), [])
