"""Execute actual Xtensa loads/stores, measuring the SoC system timer."""
import test_xtensa_cost
import unittest


@unittest.skipUnless(test_xtensa_cost.QEMU.exists() and test_xtensa_cost.CC.exists(), "requires Xtensa QEMU and compiler")
class PSRAMTiming(unittest.TestCase):
    ZERO_PIPELINE = [f'driver=esp32s3.icache,property={prop},value=0' for prop in
                     ['psram-read-tail-ns', 'psram-read-cycles', 'psram-write-cycles']]
    execute = test_xtensa_cost.XtensaCosts.execute
    def run_memory(self, size, stride=32, store=False, enabled=True, single=False,
                   ctrl=9, padding=0, access=None):
        setup = ['movi a3, 0x15', 'wsr a3, ATOMCTL',
                 'movi a3, -1', 'wsr a3, SCOMPARE1',
                 'movi a2, 0x600c5000', 'movi a3, 0x8000',
                 's32i a3, a2, 0', 'movi a2, 0x600c4000',
                 f'movi a3, {ctrl}', 's32i a3, a2, 0',
                 'movi a6, 0x3c000000', f'movi a15, {size - 1}',
                 'movi a13, 0', 'movi a14, 0x3c000000']
        op = '\n'.join(['add a6, a14, a13',
                         access or ('s32i a11, a6, 0' if store else 'l32i a11, a6, 0'),
                         f'addi a13, a13, {stride}', 'and a13, a13, a15'] + ['nop'] * padding)
        return self.execute([op, op], setup=setup, machine='x4pro', single=single,
                            iterations=8192, globals=self.ZERO_PIPELINE + [
                                'driver=esp32s3.icache,property=psram-read-ns,value=' + ('400' if enabled else '0'),
                                'driver=esp32s3.icache,property=psram-write-ns,value=400'])

    def test_capacity_misses_leave_warm_hits_unchanged(self):
        for size in [4096, 65536]:
            off = self.run_memory(size, enabled=False)
            on = self.run_memory(size)
            misses = [size // 32, 0] if size == 4096 else [8192, 8192]
            for a, b, n in zip(on, off, misses):
                self.assertAlmostEqual(a - b, n * 400 * .016, delta=5)

    def test_stores_pay_for_dirty_eviction(self):
        clean = self.run_memory(65536)
        dirty = self.run_memory(65536, store=True)
        self.assertGreater(dirty[1], clean[1] * 1.3)

    def test_single_instruction_blocks_preserve_stalls(self):
        normal = self.run_memory(65536)
        single = self.run_memory(65536, single=True)
        for a, b in zip(normal, single):
            self.assertAlmostEqual(a, b, delta=5)

    def test_64k_mode_retains_a_64k_working_set(self):
        on = self.run_memory(65536, ctrl=13)
        off = self.run_memory(65536, ctrl=13, enabled=False)
        self.assertAlmostEqual(on[0] - off[0], 2048 * 400 * .016, delta=5)
        self.assertAlmostEqual(on[1], off[1], delta=5)

    def test_vector_loads_and_atomics_receive_miss_costs(self):
        for access in ['ee.vld.128.ip q0, a6, 16', 's32c1i a11, a6, 0']:
            with self.subTest(access=access):
                on = self.run_memory(65536, access=access)
                off = self.run_memory(65536, access=access, enabled=False)
                self.assertAlmostEqual(on[1] - off[1], 8192 * 400 * .016, delta=5)

    def test_writeback_overlaps_computation(self):
        stalls = []
        for padding in [0, 150]:
            on = self.run_memory(65536, store=True, padding=padding)
            off = self.run_memory(65536, store=True, padding=padding, enabled=False)
            stalls.append(on[1] - off[1])
        self.assertGreater(stalls[0], stalls[1] * 1.3)

    def test_physical_aliases_and_invalidation(self):
        setup = ['movi a2, 0x600c5000', 'movi a3, 0x8000',
                 's32i a3, a2, 0', 's32i a3, a2, 4',
                 'movi a2, 0x600c4000', 'movi a3, 9', 's32i a3, a2, 0',
                 'movi a6, 0x3c000000', 'movi a13, 0x3c010000',
                 'movi a14, 0x600c4028', 'movi a15, 1',
                 's32i a6, a14, 4', 's32i a15, a14, 8']
        snippets = ['l32i a11, a6, 0', 'l32i a11, a13, 0',
                    's32i a15, a14, 0\nl32i a11, a13, 0']
        measurements = []
        for ns in [0, 400]:
            measurements.append(self.execute(snippets, setup=setup, machine='x4pro',
                iterations=100, globals=self.ZERO_PIPELINE + [f'driver=esp32s3.icache,property=psram-read-ns,value={ns}']))
        delta = [a - b for a, b in zip(measurements[1], measurements[0])]
        self.assertAlmostEqual(delta[0], 400 * .016, delta=2)
        self.assertAlmostEqual(delta[1], 0, delta=2)
        self.assertAlmostEqual(delta[2], 100 * 400 * .016, delta=3)

    def test_invalidate_all_rom_encoding(self):
        setup = ['movi a2, 0x600c5000', 'movi a3, 0x8000',
                 's32i a3, a2, 0', 'movi a2, 0x600c4000',
                 'movi a3, 9', 's32i a3, a2, 0',
                 'movi a6, 0x3c000000', 'l32i a11, a6, 0',
                 'movi a13, 0x600c4028', 'movi a14, 0',
                 's32i a14, a13, 4', 's32i a14, a13, 8', 'movi a15, 1']
        op = 's32i a15, a13, 0\nl32i a11, a6, 0'
        times = [self.execute([op], setup=setup, machine='x4pro', iterations=100,
                 globals=self.ZERO_PIPELINE + [f'driver=esp32s3.icache,property=psram-read-ns,value={ns}'])[0]
                 for ns in [0, 400]]
        self.assertAlmostEqual(times[1] - times[0], 100 * 400 * .016, delta=3)

    def test_clean_and_writeback_all_include_unmapped_tags(self):
        times = []
        for command in [0, 2, 4]:
            setup = ['movi a2, 0x600c5000', 'movi a3, 0x8000',
                     's32i a3, a2, 0', 'movi a2, 0x600c4000',
                     'movi a3, 9', 's32i a3, a2, 0']
            # Four dirty lines in one set. Hide their physical page before
            # issuing the ROM all-cache command; remapping must not hide tags.
            for offset in [0, 8192, 16384, 24576]:
                setup += [f'movi a6, {0x3c000000 + offset}', 's32i a3, a6, 0']
            setup += ['movi a2, 0x600c5000', 'movi a3, 0x8001',
                      's32i a3, a2, 0', 'movi a13, 0x600c4028',
                      'movi a14, 0', 's32i a14, a13, 4', 's32i a14, a13, 8',
                      f'movi a15, {command}', 's32i a15, a13, 0',
                      'movi a3, 0x8000', 's32i a3, a2, 0',
                      'movi a6, 0x3c000000']
            op = '\n'.join(['l32i a11, a6, 0', 'movi a13, 0x3c008000',
                             'l32i a11, a13, 0', 'movi a13, 0x3c00a000',
                             'l32i a11, a13, 0'])
            times.append(self.execute([op], setup=setup, machine='x4pro', iterations=1,
                globals=self.ZERO_PIPELINE + [
                    'driver=esp32s3.icache,property=psram-read-ns,value=400',
                    'driver=esp32s3.icache,property=psram-write-ns,value=10000'])[0])
        self.assertGreater(times[0] - times[1], 140)
        self.assertAlmostEqual(times[1], times[2], delta=2)
        # Exactly two new lines refill. Clean/writeback must retain the hit.
        off = self.execute([op], setup=setup, machine='x4pro', iterations=1,
            globals=['driver=esp32s3.icache,property=psram-read-ns,value=0'])[0]
        self.assertAlmostEqual(times[2] - off, 2 * 400 * .016, delta=2)
