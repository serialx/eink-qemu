"""Run with python3 -m unittest eink-qemu.tests.test_panel_ink.

Compile the actual panel functions on a small glass, with clock/timer stubs.
Compare X3's ink state against X4 Pro and exercise both X3 LUT encodings.
"""
import os
from pathlib import Path
import re
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
X3 = (ROOT / 'hw/display/uc8279.c').read_text()
X4 = (ROOT / 'hw/display/uc8179.c').read_text()


def function(source, name):
    start = source.rfind('\nstatic ', 0, source.index(name + '(')) + 1
    end = source.index('\n}', start) + 2
    return source[start:end]


def defaults(source):
    return {name: (field, value) for name, field, value in re.findall(
        r'DEFINE_PROP_\w+\("([^"]+)", \w+,\s*(\w+), (\w+)\)', source)}


PRELUDE = r'''
#include <assert.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <math.h>
#define W 8
#define H 4
#define RAM_W 8
#define RAM_H 4
#define RAM_WB 1
#define LUT_ROWS 5
#define LUT_LEN 64
#define MAX_FRAMES 8192
#define SHADES 1024
#define PSR_REG 0x20
#define CDI_N2OCP 0x08
#define MIN(a,b) ((a) < (b) ? (a) : (b))
#define MAX(a,b) ((a) > (b) ? (a) : (b))
#define DIV_ROUND_UP(a,b) (((a) + (b) - 1) / (b))
#define qemu_log_mask(...) ((void)0)
#define QEMU_CLOCK_VIRTUAL 0
#define PLANE_OLD 0
#define PLANE_NEW 1
typedef int SSIPeripheral;
typedef int QemuConsole;
typedef int qemu_irq;
typedef struct QEMUTimer { int64_t deadline; bool pending; } QEMUTimer;
static int64_t now_ns;
static int64_t qemu_clock_get_ns(int clock) { return now_ns; }
static void timer_del(QEMUTimer *t) { t->pending = false; }
static void timer_mod(QEMUTimer *t, int64_t ns) {
    assert(ns > now_ns);
    t->deadline = ns;
    t->pending = true;
}
'''

CASES = r'''
static void compare(Uc8279State *a, Uc8279State *b)
{
    for (int i = 0; i < W * H; i++) {
        assert(a->ink[i].p == b->ink[i].p);
        assert(a->ink[i].qf == b->ink[i].qf);
        assert(a->ink[i].qs == b->ink[i].qs);
        assert(a->ink[i].t_drive == b->ink[i].t_drive);
    }
}

static void parity(void)
{
    Uc8279State a = {0}, b = {0};
    init(&a); init(&b);
    a.ghosting = b.ghosting = true;
    /* Thin edges, held pixels, mixed gray positions, prior charge. */
    for (int i = 0; i < W * H; i++) {
        a.ink[i].p = (i % 9) / 8.0f;
        a.ink[i].qf = (i % 5 - 2) * 0.01f;
        a.ink[i].qs = (i % 7 - 3) * 0.1f;
        b.ink[i] = a.ink[i];
        a.cls[i / W][i % W] = b.cls[i / W][i % W] = i % 4;
    }
    for (int partial = 0; partial < 2; partial++) {
        a.partial = b.partial = partial;
        int n = uc8279_otp_frames(&a);
        assert(n == ref_otp_frames(&b));
        assert(memcmp(a.frames, b.frames, n * sizeof(*a.frames)) == 0);
        for (int f = 0; f < n; f++) {
            uc8279_run_frame(&a, f);
            ref_run_frame(&b, f);
            compare(&a, &b);
        }
        uc8279_relax(&a, 10.0, true);
        ref_relax(&b, 10.0, true);
        compare(&a, &b);
    }
    uc8279_relax(&a, 1810.0, false);
    ref_relax(&b, 1810.0, false);
    compare(&a, &b);
    fini(&a); fini(&b);
}

static void lut_order(void)
{
    Uc8279State s = {0};
    init(&s);
    /* Six-byte groups repeat all four phases; empty groups don't terminate. */
    const uint8_t six[] = { 0,0,0,0,0,0, 0x60,1,2,0,0,2, 0x80,1,0,0,0,1 };
    const int six_expected[] = {1,2,2,1,2,2,2};
    s.uc8253 = true;
    memcpy(s.lut[2], six, sizeof(six)); s.lut_n[2] = sizeof(six);
    assert(uc8279_row_frames(&s, 2) == 7);
    for (int f = 0; f < 7; f++) {
        assert(uc8279_lut_level(&s, 2, f) == six_expected[f]);
    }
    assert(uc8279_lut_level(&s, 2, 7) == 0);
    /* Seven-byte groups repeat phase pairs, then repeat the whole group. */
    const uint8_t seven[] = {
        0,0,0,0,0,0,0, 2,0x41,0x81,0x02,0xc1,2,1,
        1,0x41,0x81,0,0,1,0, 1,0,0,0x81,0x41,0,1,
        255,255 /* incomplete trailing group */
    };
    const int seven_expected[] = {1,2,1,2,0,0,3, 1,2,1,2,0,0,3, 1,2, 2,1};
    s.uc8253 = false;
    memcpy(s.lut[2], seven, sizeof(seven)); s.lut_n[2] = sizeof(seven);
    assert(uc8279_row_frames(&s, 2) == 18);
    for (int f = 0; f < 18; f++) {
        assert(uc8279_lut_level(&s, 2, f) == seven_expected[f]);
    }
    assert(uc8279_lut_level(&s, 2, 18) == 0);
    /* VCOM contributes to drive, and the longest row determines timing. */
    memset(s.lut_n, 0, sizeof(s.lut_n));
    s.uc8253 = true;
    memcpy(s.lut[0], (uint8_t[]){0x40,3,0,0,0,1}, 6); s.lut_n[0] = 6;
    memcpy(s.lut[2], (uint8_t[]){0x80,1,0,0,0,1}, 6); s.lut_n[2] = 6;
    int rows[5];
    assert(uc8279_lut_frames(&s, rows) == 3);
    assert(s.frames[0][1] == 2 && s.frames[1][1] == 1);
    /* Bound ink playback without shortening the controller's BUSY interval. */
    memcpy(s.lut[2], (uint8_t[]){0x40,255,255,255,255,255}, 6);
    assert(uc8279_lut_frames(&s, rows) == 260100);
    assert(s.anim_n == MAX_FRAMES);
    fini(&s);
}

static void clear_and_animate(void)
{
    Uc8279State a = {0}, b = {0};
    init(&a); init(&b);
    a.ghosting = b.ghosting = true;
    a.uc8253 = b.uc8253 = true;
    a.psr = b.psr = PSR_REG;
    a.animate = true; b.animate = false;
    /* A balanced black-then-white waveform must erase the previous image. */
    for (int r = 0; r < 5; r++) {
        memcpy(a.lut[r], (uint8_t[]){r ? 0x60 : 0,13,13,0,0,1}, 6);
        memcpy(b.lut[r], a.lut[r], 6);
        a.lut_n[r] = b.lut_n[r] = 6;
    }
    for (int i = 0; i < W * H; i++) {
        a.ink[i].p = b.ink[i].p = (i % 3) / 2.0f;
    }
    memset(a.ram[PLANE_NEW], 0xff, sizeof(a.ram[PLANE_NEW]));
    memcpy(b.ram, a.ram, sizeof(a.ram));
    a.cdi = b.cdi = CDI_N2OCP;
    int rows[5];
    assert(uc8279_refresh(&a, rows) == 26);
    assert(uc8279_refresh(&b, rows) == 26);
    assert(a.anim_f == 0 && b.anim_f == 26);
    assert(memcmp(a.ram[0], a.ram[1], sizeof(a.ram[0])) == 0);
    while (a.anim_timer.pending) {
        now_ns = a.anim_timer.deadline;
        uc8279_run_frames(&a, uc8279_now());
    }
    assert(a.anim_f == 26);
    compare(&a, &b);
    for (int i = 0; i < W * H; i++) { assert(a.ink[i].p > 0.999f); }
    /* A controller reset keeps optical history. */
    Uc8279Ink saved = a.ink[0];
    uc8279_reset_regs(&a);
    assert(memcmp(&saved, &a.ink[0], sizeof(saved)) == 0);
    /* Existing zero-period turbo must be finite and synchronous. */
    a.frame_us = 0;
    assert(uc8279_refresh(&a, rows) == 60);
    assert(a.anim_f == a.anim_n && !a.anim_timer.pending);
    assert(isfinite(a.ink[0].p));
    /* Non-turbo calibrated BUSY and PLL scaling remain intact. */
    a.frame_us = 12850; a.refresh_overhead_us = 138000; a.pll = 9;
    assert(uc8279_refresh_us(&a, 19) == 382150);
    assert(uc8279_refresh_us(&a, 27) == 484950);
    assert(uc8279_refresh_us(&a, 7) == 227950);
    assert(uc8279_refresh_us(&a, 62) == 934700);
    a.pll = 10;
    assert(uc8279_frame_us(&a) == 11423);
    a.busy_ms = 1;
    assert(uc8279_refresh_us(&a, 62) == 1000);
    fini(&a); fini(&b);
}

static void clean_ink(void)
{
    Uc8279State a = {0}, b = {0};
    init(&a); init(&b);
    assert(!a.ghosting && !b.ghosting);
    /* Same target, different ink/charge history and neighboring classes. */
    for (int i = 0; i < W * H; i++) {
        a.ink[i].p = (i % 9) / 8.0f;
        b.ink[i].p = 1 - a.ink[i].p;
        a.ink[i].qf = 0.2f; b.ink[i].qs = -0.3f;
        a.cls[i / W][i % W] = b.cls[i / W][i % W] = i % 4;
    }
    /* Saturate black, then paint four evenly spaced LUT gray levels. */
    for (int f = 0; f < 12; f++) {
        for (int c = 0; c < 4; c++) {
            a.frames[f][c] = b.frames[f][c] =
                f < 6 ? -1 : (f - 6 < c * 2 ? 1 : 0);
        }
        uc8279_run_frame(&a, f);
        ref_run_frame(&b, f);
    }
    for (int i = 0; i < W * H; i++) {
        float target = (i % 4) / 3.0f;
        assert(fabsf(a.ink[i].p - target) < 1e-6f);
        assert(a.ink[i].p == b.ink[i].p);
    }
    float before = a.ink[0].p;
    uc8279_relax(&a, 100000.0, true);
    ref_relax(&b, 100000.0, true);
    assert(a.ink[0].p == before && b.ink[0].p == before);
    a.anim_n = a.anim_f = 12;
    uc8279_run_frames(&a, -1);
    assert(!a.drift_timer.pending);
    /* Fast OTP refreshes must leave held gray pixels untouched. */
    a.partial = b.partial = true;
    a.ink[0].p = b.ink[0].p = 1.0f / 3;
    int n = uc8279_otp_frames(&a);
    assert(n == ref_otp_frames(&b));
    for (int f = 0; f < n; f++) {
        uc8279_run_frame(&a, f);
        ref_run_frame(&b, f);
    }
    assert(a.ink[0].p == 1.0f / 3 && b.ink[0].p == 1.0f / 3);
    fini(&a); fini(&b);
}

int main(void)
{
    parity(); lut_order(); clear_and_animate(); clean_ink();
    puts("X4 Pro parity, LUT order, clearing, animation, turbo, timing and clean ink: PASS");
}
'''


class PanelInkTest(unittest.TestCase):
    def test_defaults_match_x4pro(self):
        for name in ('ghosting', 'swing-frames', 'rail-soft', 'otp-fast-frames',
                     'otp-full-frames', 'otp-hold-drive', 'remnant-fast',
                     'remnant-fast-ms', 'remnant-slow', 'remnant-slow-ms',
                     'bloom', 'drift', 'drift-s', 'animate', 'frame-us'):
            self.assertEqual(defaults(X3)[name], defaults(X4)[name], name)

    def test_actual_c_functions(self):
        ink = X3[X3.index('typedef struct Uc8279Ink'):X3.index('enum { PLANE_OLD')]
        state = X3[X3.index('struct Uc8279State {'):X3.index('static const uint8_t uc8279_ver')]
        source = PRELUDE + ink + 'typedef struct Uc8279State Uc8279State;\n' + state
        names = ('row_frames', 'lut_level', 'lut_frames', 'pll_hz', 'frame_us',
                 'refresh_us', 'now', 'otp_frames', 'relax', 'run_frame',
                 'run_frames', 'refresh', 'set_window', 'reset_regs')
        source += function(X3, 'ink_move') + '\n'
        source += '\n'.join(function(X3, 'uc8279_' + name) for name in names)
        source += '\ntypedef Uc8279State Uc8179State;\ntypedef Uc8279Ink Uc8179Ink;\n'
        source += function(X4, 'ink_move').replace('ink_move', 'ref_ink_move') + '\n'
        for name in ('otp_frames', 'relax', 'run_frame'):
            source += function(X4, 'uc8179_' + name).replace('uc8179_', 'ref_').replace('ink_move', 'ref_ink_move') + '\n'
        source += '\nstatic void init(Uc8279State *s) {\n'
        for field, value in defaults(X3).values():
            source += f'    s->{field} = {value};\n'
        source += '''
    s->ink = calloc(W * H, sizeof(*s->ink));
    s->frames = calloc(MAX_FRAMES, sizeof(*s->frames));
    s->anim_frame_us = s->frame_us;
    uc8279_reset_regs(s);
}
static void fini(Uc8279State *s) { free(s->ink); free(s->frames); }
'''
        source += CASES
        with tempfile.TemporaryDirectory(prefix='panel-ink-') as tmp:
            c = Path(tmp) / 'test.c'
            binary = Path(tmp) / 'test'
            c.write_text(source)
            subprocess.run([os.environ.get('CC', 'cc'), '-std=c11', '-O1',
                            '-g', '-fsanitize=address,undefined',
                            '-fno-omit-frame-pointer', str(c), '-lm', '-o', str(binary)], check=True)
            subprocess.run([str(binary)], check=True)


if __name__ == '__main__':
    unittest.main()
