# SPDX-License-Identifier: GPL-2.0-or-later
"""Compile real UC8279/UC8179 ink and display code with clock stubs.

Run: python3 tests/unit/test_uc8279_drift.py
No firmware, QEMU build or display server is required.
"""
import importlib.util
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location(
    'panel_ink', ROOT / 'x4prosim/tests/test_panel_ink.py')
helpers = importlib.util.module_from_spec(spec)
spec.loader.exec_module(helpers)

DISPLAY = r'''
#define H_ADDR H
#define WB (W / 8)
#define OTP_SIZE 4096
#define PSR_UD 8
#define PSR_SHL 4
#define LUT_GROUPS (LUT_LEN / 6)
typedef struct DisplaySurface { uint32_t pixels[W * H]; } DisplaySurface;
static DisplaySurface surface;
static DisplaySurface *qemu_console_surface(QemuConsole *c) { return &surface; }
static void *surface_data(DisplaySurface *s) { return s->pixels; }
static int surface_stride(DisplaySurface *s) { return W * 4; }
static int surface_width(DisplaySurface *s) { return W; }
static int surface_height(DisplaySurface *s) { return H; }
static uint32_t rgb_to_pixel32(int r, int g, int b) {
    return (r << 16) | (g << 8) | b;
}
static void qemu_console_update(QemuConsole *c, int x, int y, int w, int h) {}
'''

CASES = r'''
static void init(PanelState *s)
{
    s->ink = calloc(W * H, sizeof(*s->ink));
    s->frames = calloc(MAX_FRAMES, sizeof(*s->frames));
    s->swing = 6; s->rail_soft = 100;
    s->remnant_fast_ms = 1000; s->remnant_slow_ms = 30000;
    /* A large drift makes cadence failures obvious on the small test glass. */
    s->drift = 500; s->drift_s = 10;
    s->frame_us = 25000;
    FRAME_PERIOD
    for (int i = 0; i < W * H; i++) {
        s->ink[i].p = (i % 9) / 8.0f;
        s->ink[i].qf = 0.1f; s->ink[i].qs = 0.2f;
    }
    for (int i = 0; i <= SHADES; i++) { s->shade[i] = i * 255 / SHADES; }
    s->drift_deadline = 1000000000;
    timer_mod(&s->drift_timer, s->drift_deadline);
}

static void present(PanelState *s)
{
    PanelInk before[W * H];
    memcpy(before, s->ink, sizeof(before));
    double t = s->t_drift;
    panel_update_display(s);
    assert(memcmp(before, s->ink, sizeof(before)) == 0);
    assert(t == s->t_drift);
}

static void run_until(PanelState *s, int64_t until)
{
    while (s->drift_timer.pending && s->drift_timer.deadline <= until) {
        now_ns = s->drift_timer.deadline;
        /* QEMU clears pending/expiry before calling the callback. */
        s->drift_timer.pending = false;
        s->drift_timer.deadline = -1;
        panel_drift_tick(s);
    }
    now_ns = until;
}

int main(void)
{
    PanelState a = {0}, b = {0};
    init(&a); init(&b);
    for (int tick = 1; tick <= 1200; tick++) {
        run_until(&a, tick * 10000000LL);
        present(&a);
    }
    now_ns = 0;
    run_until(&b, 12000000000LL);
    present(&b);
    assert(memcmp(a.ink, b.ink, W * H * sizeof(*a.ink)) == 0);
    assert(a.t_drift == 12 && b.t_drift == 12);
    assert(a.ink[0].qf == 0.1f && a.ink[0].qs == 0.2f);
    uint32_t saved[W * H];
    memcpy(saved, surface.pixels, sizeof(saved));
    a.redraw = true;
    present(&a);
    assert(memcmp(saved, surface.pixels, sizeof(saved)) == 0);

    /* A late callback integrates only to its fixed deadline, not host 'now'. */
    now_ns = 13500000000LL;
    a.drift_timer.pending = false;
    a.drift_timer.deadline = -1;
    panel_drift_tick(&a);
    assert(a.t_drift == 13 && a.drift_timer.deadline == 14000000000LL);
    run_until(&b, 13000000000LL);
    assert(memcmp(a.ink, b.ink, W * H * sizeof(*a.ink)) == 0);

    /* Refresh start must not rewind the idle interval and apply it twice. */
    a.t_refresh = 2;
    a.anim_n = a.anim_f = 0;
    a.anim_t0 = 2;
    a.otp_full_frames = 0;
    a.animate = true;
    a.drift = 500;
    float p = a.ink[0].p;
    REFRESH
    assert(a.ink[0].p == p);
    assert(fabsf(a.ink[0].qf - 0.1f * expf(-11)) < 1e-9f);

    /* Completing an animation schedules idle drift after its exact end. */
    a.drift = 500;
    a.anim_t0 = 13;
    RESET_PERIOD
    a.anim_f = 0; a.anim_n = 2;
    panel_run_frames(&a, -1);
    assert(a.t_refresh == 13.05 && a.t_drift == 13.05);
    assert(a.drift_timer.deadline == (int64_t)ceil(14.05 * 1e9));
    assert(a.drift_timer.pending);
    RESET_CASE
    free(a.ink); free(a.frames); free(b.ink); free(b.frames);
    puts("GUI cadence, late dispatch, leakage and refresh boundary: PASS");
}
'''


class PanelDriftTest(unittest.TestCase):
    def test_both_controllers(self):
        for model in ('uc8279', 'uc8179'):
            with self.subTest(model=model):
                raw = (ROOT / f'hw/display/{model}.c').read_text()
                typename = 'Uc' + model[2:]
                ink = raw[raw.index(f'typedef struct {typename}Ink'):
                          raw.index('enum { PLANE_OLD')]
                start = raw.index(f'struct {typename}State {{')
                state = raw[start:raw.index('\n};', start) + 4]
                source = helpers.PRELUDE + DISPLAY + ink
                source += f'typedef struct {typename}State {typename}State;\n'
                source += state + helpers.function(raw, 'ink_move')
                if model == 'uc8179':
                    start = raw.index('typedef struct LutCursor')
                    end = raw.index('} LutCursor;', start) + len('} LutCursor;')
                    source += raw[start:end]
                    source += helpers.function(raw, 'lut_next')
                names = (['row_frames', 'lut_level', 'lut_frames', 'pll_hz',
                          'frame_us'] if model == 'uc8279' else ['lut_frames'])
                names += ['now', 'otp_frames', 'relax', 'drift_tick',
                          'run_frame', 'run_frames', 'refresh',
                          'update_display']
                source += '\n'.join(helpers.function(raw, model + '_' + name)
                                    for name in names)
                if model == 'uc8179':
                    source += '\n#define UC8179(p) ((Uc8179State *)(p))\n'
                    source += helpers.function(raw, 'uc8179_reset_regs')
                    source += helpers.function(raw, 'uc8179_set_rst')
                source += CASES.replace('RESET_CASE', r'''
    /* Reset during playback freezes the ink and restarts idle drift. */
    a.anim_f = 1; a.anim_n = 2;
    now_ns = 13037000000LL;
    PanelInk reset_ink[W * H];
    memcpy(reset_ink, a.ink, sizeof(reset_ink));
    panel_set_rst(&a, 0, 0);
    assert(a.anim_f == a.anim_n && !a.anim_timer.pending);
    assert(a.t_refresh == 13.025 && a.t_drift == 13.025);
    assert(a.drift_timer.deadline == (int64_t)ceil(14.025 * 1e9));
    assert(memcmp(reset_ink, a.ink, sizeof(reset_ink)) == 0);
    /* Leakage includes the uncompleted fraction before the reset edge. */
    float qf = a.ink[0].qf;
    now_ns = 13075000000LL;
    panel_refresh(&a);
    assert(fabsf(a.ink[0].qf - qf * expf(-0.05f)) <= fabsf(qf) * 1e-6f);
''' if model == 'uc8179' else '').replace('RESET_PERIOD',
                    'a.anim_frame_us = 25000;' if model == 'uc8279' else '')
                source = source.replace('FRAME_PERIOD',
                    's->anim_frame_us = s->frame_us;'
                    if model == 'uc8279' else '')
                source = source.replace('REFRESH',
                    'int rows[5]; panel_refresh(&a, rows);' if model == 'uc8279'
                    else 'panel_refresh(&a);')
                source = source.replace(typename, 'Panel')
                source = source.replace(model + '_', 'panel_')
                prefix = model + '-drift-'
                with tempfile.TemporaryDirectory(prefix=prefix) as tmp:
                    c = Path(tmp) / 'test.c'
                    binary = Path(tmp) / 'test'
                    c.write_text(source)
                    subprocess.run([os.environ.get('CC', 'cc'),
                                    '-std=c11', '-O1',
                                    '-g', '-fsanitize=address,undefined',
                                    '-fno-omit-frame-pointer', str(c), '-lm',
                                    '-o', str(binary)], check=True)
                    subprocess.run([str(binary)], check=True)


if __name__ == '__main__':
    unittest.main()
