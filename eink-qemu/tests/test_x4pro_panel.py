"""X4 Pro panel selection and protocol regressions, without firmware blobs."""
import json
import os
import re
import select
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
QEMU = Path(os.environ.get('QEMU_XTENSA', ROOT / 'build/qemu-system-xtensa'))

PRELUDE = r'''
#include <assert.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <math.h>
typedef struct { bool cs; } SSIPeripheral;
typedef int QemuConsole;
typedef int Object;
typedef int *qemu_irq;
typedef struct { int64_t deadline; bool pending; } QEMUTimer;
typedef struct Uc8179State Uc8179State;
#define UC8179(p) ((Uc8179State *)(p))
#define QEMU_CLOCK_VIRTUAL 0
#define MIN(a,b) ((a) < (b) ? (a) : (b))
#define MAX(a,b) ((a) > (b) ? (a) : (b))
#define DIV_ROUND_UP(a,b) (((a) + (b) - 1) / (b))
#define ARRAY_SIZE(a) (sizeof(a) / sizeof((a)[0]))
#define qemu_log_mask(...) ((void)0)
#define trace_uc8179_refresh(...) ((void)0)
#define trace_uc8179_plane_write(...) ((void)0)
static int64_t now_ns = 500123;
static int64_t qemu_clock_get_ns(int clock) { return now_ns; }
static int64_t qemu_clock_get_us(int clock) { return now_ns / 1000; }
static int64_t qemu_clock_get_ms(int clock) { return now_ns / 1000000; }
static void qemu_set_irq(qemu_irq irq, int level) { if (irq) *irq = level; }
static void timer_del(QEMUTimer *t) { t->pending = false; }
static void timer_mod(QEMUTimer *t, int64_t deadline) {
    t->deadline = deadline; t->pending = true;
}
'''

CASES = r'''
static void command(Uc8179State *s, uint8_t cmd, const uint8_t *bytes, int n)
{
    uc8179_set_dc(s, 0, 0);
    uc8179_transfer(&s->parent_obj, cmd);
    uc8179_set_dc(s, 0, 1);
    for (int i = 0; i < n; i++) uc8179_transfer(&s->parent_obj, bytes[i]);
}
static void finish(Uc8179State *s)
{
    now_ns = s->busy_timer.deadline;
    uc8179_run_frames(s, now_ns / 1e9);
    uc8179_busy_done(s);
}
static void identity(void)
{
    Uc8179State *s = make(true);
    const uint8_t id[] = {0, 15, 0x68, 1, 0};
    command(s, 0x70, NULL, 0);
    for (int i = 0; i < 5; i++) {
        assert(uc8179_transfer(&s->parent_obj, 0xff) == id[i]);
    }
    /* Restarting the command restarts the response. */
    command(s, 0x70, NULL, 0);
    assert(uc8179_transfer(&s->parent_obj, 0xff) == 0);
    drop(s);
}
static void timing(void)
{
    Uc8179State *s = make(true);
    int busy = 1; s->busy_n = &busy;
    command(s, 0x04, NULL, 0);
    assert(busy == 0);
    assert(s->busy_timer.deadline == now_ns + 40000000);
    finish(s); assert(busy == 1);
    command(s, 0x00, (uint8_t[]){0x17,0x4d}, 2);
    command(s, 0x12, NULL, 0);
    assert(s->busy_timer.deadline == now_ns + 1328000000);
    finish(s);
    command(s, 0x91, NULL, 0);
    command(s, 0x90, (uint8_t[]){0,0,3,31,0,120,2,87,1}, 9);
    command(s, 0x12, NULL, 0);
    assert(s->busy_timer.deadline == now_ns + 483000000);
    finish(s);
    command(s, 0x92, NULL, 0);
    command(s, 0x00, (uint8_t[]){0x37,0x4d}, 2);
    uint8_t gray[49] = {1,2,3,1,1,1,1};
    for (int r = 0; r < 5; r++) command(s, 0x20+r, gray, sizeof(gray));
    command(s, 0x12, NULL, 0);
    assert(s->busy_timer.deadline == now_ns + 228998000);
    finish(s);
    uint8_t settle[42] = {1,6,1,6,6,1,1,1,2,4,0,0,1,1};
    for (int r = 0; r < 5; r++) command(s, 0x20+r, settle, sizeof(settle));
    command(s, 0x12, NULL, 0);
    assert(s->busy_timer.deadline == now_ns + 558992000);
    finish(s); drop(s);
}
static void lut(void)
{
    Uc8179State *s = make(true);
    command(s, 0x00, (uint8_t[]){0x37}, 1);
    /* Skip empty groups; repeat each pair and then the entire group. */
    uint8_t seq[] = {0,0,0,0,0,0,0, 2,0x41,0x81,2,0xc1,2,1,
                    1,0x41,0x81,0,0,1,0, 1,0,0,0x81,0x41,0,1,
                    255,255};
    const int expected[] = {-1,1,-1,1,0,0,0,-1,1,-1,1,0,0,0,-1,1,1,-1};
    command(s, 0x22, seq, sizeof(seq));
    command(s, 0x12, NULL, 0);
    assert(s->anim_n == 18);
    for (int i = 0; i < 18; i++) assert(s->frames[i][1] == expected[i]);
    finish(s);
    /* A shorter replacement must clear previous groups. */
    command(s, 0x22, (uint8_t[]){1,0x81,0,0,0,1,0}, 7);
    command(s, 0x20, (uint8_t[]){1,0x43,0,0,0,1,0}, 7);
    command(s, 0x12, NULL, 0);
    assert(s->anim_n == 3); /* longest row, including VCOM */
    assert(s->frames[0][1] == 2 && s->frames[1][1] == 1);
    finish(s); drop(s);
}
static void addressing(void)
{
    Uc8179State *s = make(true);
    s->animate = false;
    command(s, 0x00, (uint8_t[]){0x17}, 1);
    command(s, 0x61, (uint8_t[]){3,32,2,88}, 4);
    uint8_t *plane = malloc(H_ADDR*WB);
    memset(plane, 0xff, H_ADDR*WB);
    plane[120*WB] = 0x7f;
    plane[599*WB+99] = 0xfe;
    command(s, 0x13, plane, H_ADDR*WB);
    command(s, 0x12, NULL, 0);
    assert(s->ink[0].p == 0 && s->ink[1].p == 1);
    assert(s->ink[W*H-1].p == 0);
    finish(s);
    /* Like the firmware, synchronize OLD after a full refresh. */
    command(s, 0x10, plane, H_ADDR*WB);
    /* Only this window is written/refreshed; the far corner stays black. */
    command(s, 0x91, NULL, 0);
    command(s, 0x90, (uint8_t[]){0,0,0,7,0,120,0,120,1}, 9);
    command(s, 0x13, (uint8_t[]){0xff}, 1);
    assert(s->ram[PLANE_NEW][120][0] == 0xff);
    assert(s->ram[PLANE_NEW][599][99] == 0xfe);
    command(s, 0x12, NULL, 0);
    assert(s->ink[0].p == 1 && s->ink[W*H-1].p == 0);
    finish(s);
    /* An inverted or out-of-range window must not alias visible RAM. */
    command(s, 0x90, (uint8_t[]){3,255,0,7,255,255,0,120,1}, 9);
    command(s, 0x13, (uint8_t[]){0}, 1);
    command(s, 0x12, NULL, 0);
    assert(s->ink[0].p == 1);
    free(plane); drop(s);
}
static void reset(void)
{
    Uc8179State *s = make(true);
    int busy = 1; s->busy_n = &busy;
    command(s, 0x12, NULL, 0);
    assert(s->busy_timer.pending && s->anim_timer.pending);
    uc8179_set_rst(s, 0, 0);
    assert(!s->busy_timer.pending && !s->anim_timer.pending && busy == 1);
    command(s, 0x04, NULL, 0); /* commands in reset are ignored */
    assert(!s->busy_timer.pending);
    uc8179_set_rst(s, 0, 1);
    command(s, 0x04, NULL, 0);
    assert(s->busy_timer.deadline == now_ns + 40000000);
    finish(s); drop(s);
}
static void orientation(void)
{
    for (int ud = 0; ud < 2; ud++) {
        for (int shl = 0; shl < 2; shl++) {
            Uc8179State *s = make(true);
            s->animate = false;
            s->psr = 0x13 | (ud ? PSR_UD : 0) | (shl ? PSR_SHL : 0);
            s->ram[PLANE_NEW][120][0] = 0x7f;
            command(s, 0x12, NULL, 0);
            int black = (ud ? H - 1 : 0) * W + (shl ? 0 : W - 1);
            for (int i = 0; i < W*H; i++) assert(s->ink[i].p == (i == black ? 0 : 1));
            finish(s); drop(s);
        }
    }
    Uc8179State *s = make(true);
    s->ghosting = true; s->animate = false; s->drift = 0; s->bloom = 100;
    s->psr = 0x17;
    command(s, 0x91, NULL, 0);
    command(s, 0x90, (uint8_t[]){0,0,0,7,0,120,0,120,1}, 9);
    command(s, 0x13, (uint8_t[]){0}, 1);
    Uc8179Ink *before = malloc(W*H*sizeof(*before));
    memcpy(before, s->ink, W*H*sizeof(*before));
    command(s, 0x12, NULL, 0);
    assert(s->ink[0].p < before[0].p);
    assert(!memcmp(before+8, s->ink+8, (W*H-8)*sizeof(*before)));
    finish(s); free(before); drop(s);
}
static void turbo(void)
{
    Uc8179State *s = make(true);
    s->busy_ms = 1; s->frame_us = 1;
    command(s, 0x12, NULL, 0);
    assert(s->busy_timer.deadline == now_ns + 1000000);
    finish(s);
    assert(!s->anim_timer.pending && s->anim_f == s->anim_n);
    for (int i = 0; i < W*H; i++) assert(isfinite(s->ink[i].p));
    drop(s);
}
static void long_lut(void)
{
    Uc8179State *s = make(true);
    s->frame_us = UINT32_MAX;
    command(s, 0x00, (uint8_t[]){0x37}, 1);
    uint8_t row[56]; memset(row, 0xff, sizeof(row));
    command(s, 0x22, row, sizeof(row));
    command(s, 0x12, NULL, 0);
    assert(s->anim_n == MAX_FRAMES);
    assert(s->busy_timer.deadline > now_ns && s->anim_timer.deadline > now_ns);
    assert(s->anim_frame_us <= (uint64_t)(INT64_MAX - now_ns) / 1000 / MAX_FRAMES);
    drop(s);
}
int main(int argc, char **argv)
{
    assert(argc == 2);
    if (!strcmp(argv[1], "identity")) identity();
    else if (!strcmp(argv[1], "timing")) timing();
    else if (!strcmp(argv[1], "lut")) lut();
    else if (!strcmp(argv[1], "addressing")) addressing();
    else if (!strcmp(argv[1], "reset")) reset();
    else if (!strcmp(argv[1], "orientation")) orientation();
    else if (!strcmp(argv[1], "turbo")) turbo();
    else if (!strcmp(argv[1], "long_lut")) long_lut();
    else abort();
    return 0;
}
'''


class PanelProtocol(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.directory = tempfile.TemporaryDirectory()
        cls.addClassCleanup(cls.directory.cleanup)
        source = (ROOT / 'hw/display/uc8179.c').read_text()
        core = source[source.index('#define W '):
                      source.index('static bool uc8179_update_display')]
        init_start = source.index('static void uc8279_68_init(')
        init = source[init_start:source.index('\n}', init_start)+2]
        setup = '\nstatic Uc8179State *make(bool variant) {\n'
        setup += 'Uc8179State *s = calloc(1, sizeof(*s));\n'
        for field, value in re.findall(
                r'DEFINE_PROP_\w+\("[^"]+", Uc8179State,\s*(\w+), (\w+)\)', source):
            setup += f's->{field} = {value};\n'
        setup += '''
if (variant) uc8279_68_init((Object *)s);
s->ink = calloc(W*H, sizeof(*s->ink));
s->frames = calloc(MAX_FRAMES, sizeof(*s->frames));
for (int i = 0; i < W*H; i++) s->ink[i].p = 1;
memset(s->ram, 0xff, sizeof(s->ram));
uc8179_reset_regs(s);
return s;
}
static void drop(Uc8179State *s) { free(s->ink); free(s->frames); free(s); }
'''
        path = Path(cls.directory.name) / 'protocol.c'
        cls.executable = Path(cls.directory.name) / 'protocol'
        path.write_text(PRELUDE + core + init + setup + CASES)
        result = subprocess.run([os.environ.get('CC', 'cc'), '-std=c11', '-O1',
                                 '-g', '-fsanitize=address,undefined',
                                 '-fno-omit-frame-pointer', str(path), '-lm',
                                 '-o', str(cls.executable)], capture_output=True, text=True)
        if result.returncode:
            raise AssertionError(result.stderr)

    def check_case(self, name):
        result = subprocess.run([str(self.executable), name], capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_measured_identity(self):
        self.check_case('identity')

    def test_long_lut_keeps_virtual_deadlines_representable(self):
        self.check_case('long_lut')

    def test_measured_waits_and_complete_lut_banks(self):
        self.check_case('timing')

    def test_lut_phase_order_repeats_and_short_replacement(self):
        self.check_case('lut')

    def test_visible_gate_offset_and_partial_window(self):
        self.check_case('addressing')

    def test_reset_cancels_busy_and_animation(self):
        self.check_case('reset')

    def test_axes_and_partial_window_with_ghosting(self):
        self.check_case('orientation')

    def test_turbo_finishes_animation_with_busy(self):
        self.check_case('turbo')


@unittest.skipUnless(QEMU.exists(), 'build qemu-system-xtensa first')
class PanelSelection(unittest.TestCase):
    def launch(self, machine, extra=()):
        commands = [{'execute': 'qmp_capabilities'}]
        for prop in ('type', 'frame-us', 'pon-ms'):
            commands.append({'execute': 'qom-get', 'arguments': {
                'path': '/machine/peripheral/panel', 'property': prop}})
        commands.append({'execute': 'quit'})
        result = subprocess.run([
            str(QEMU), '-L', str(ROOT / 'pc-bios'), '-machine', machine,
            '-S', '-display', 'none', '-serial', 'none', '-monitor', 'none',
            '-nic', 'none', '-qmp', 'stdio', *extra,
        ], input=''.join(json.dumps(c) + '\n' for c in commands),
            text=True, capture_output=True, timeout=15)
        return result

    def properties(self, machine, extra=()):
        result = self.launch(machine, extra)
        self.assertEqual(result.returncode, 0, result.stderr)
        replies = [json.loads(line) for line in result.stdout.splitlines()]
        self.assertFalse([r for r in replies if 'error' in r], result.stdout)
        return [r['return'] for r in replies if 'return' in r][1:4]

    def test_measured_profile(self):
        self.assertEqual(self.properties('x4pro,panel=uc8279-68'),
                         ['uc8279-68', 18333, 40])

    def test_default_profile(self):
        self.assertEqual(self.properties('x4pro'), ['uc8179', 25000, 2])
        self.assertEqual(self.properties('x4pro,panel=uc8179'),
                         ['uc8179', 25000, 2])

    def test_global_override_wins(self):
        self.assertEqual(self.properties('x4pro,panel=uc8279-68', (
            '-global', 'uc8179.frame-us=25000',
            '-global', 'uc8179.pon-ms=0',
        )), ['uc8279-68', 25000, 0])

    def test_invalid_profile(self):
        result = self.launch('x4pro,panel=unknown')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('uc8179 or uc8279-68', result.stderr)


@unittest.skipUnless(QEMU.exists(), 'build qemu-system-xtensa first')
class PanelGPIO(unittest.TestCase):
    def test_realized_identity_mtp_header_and_busy_edges(self):
        with tempfile.TemporaryFile() as err:
            proc = subprocess.Popen([
                str(QEMU), '-L', str(ROOT / 'pc-bios'), '-machine', 'x4pro,panel=uc8279-68',
                '-accel', 'qtest', '-display', 'none', '-serial', 'none',
                '-monitor', 'none', '-nic', 'none', '-qtest', 'stdio',
            ], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=err,
                text=True, bufsize=1)
            try:
                def command(text):
                    proc.stdin.write(text + '\n'); proc.stdin.flush()
                    self.assertTrue(select.select([proc.stdout], [], [], 5)[0], text)
                    reply = proc.stdout.readline().strip().split()
                    if not reply:
                        err.seek(0)
                        self.fail(f'{text}: QEMU closed qtest: {err.read().decode()}')
                    self.assertEqual(reply[0], 'OK', reply)
                    return reply[1:]

                def pin(name, value):
                    command(f'set_irq_in /machine/peripheral/panel {name} 0 {value}')

                def send(value):
                    for bit in range(7, -1, -1):
                        pin('sclk', 0); pin('sda', (value >> bit) & 1); pin('sclk', 1)
                    pin('sclk', 0)

                def receive():
                    byte = 0
                    for _ in range(8):
                        value = int(command('readl 0x6000403c')[0], 0)
                        byte = byte * 2 + ((value >> 11) & 1)
                        pin('sclk', 1); pin('sclk', 0)
                    return byte

                def read_register(reg, count):
                    pin('dc', 0); send(reg); pin('dc', 1)
                    return bytes(receive() for _ in range(count))

                pin('rst', 1); pin('ssi-gpio-cs', 0)
                self.assertEqual(read_register(0x70, 5), bytes.fromhex('00 0f 68 01 00'))
                # RMTP has one dummy byte before address zero, as in the
                # hardware probe. Compare the bytes the firmware records.
                self.assertEqual(read_register(0xa2, 49)[1:], bytes.fromhex(
                    'a5 a5 1f 20 25 25 3c 00 97 02 02 03 20 02 58 00 '
                    '00 00 00 00 00 00 00 0f 00 00 68 01 00 00 00 00 '
                    '00 00 00 00 00 00 00 00 05 0a 0f 14 50 5f 64 7f'))
                pin('dc', 0); send(0x04)
                self.assertEqual(read_register(0x71, 1), b'\x12')
                command('clock_step 39999999')
                self.assertEqual(read_register(0x71, 1), b'\x12')
                command('clock_step 1')
                self.assertEqual(read_register(0x71, 1), b'\x13')
            finally:
                proc.terminate(); proc.wait(timeout=5)
                proc.stdin.close(); proc.stdout.close()


if __name__ == '__main__':
    unittest.main()
