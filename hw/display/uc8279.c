/*
 * UltraChip UC8279d (or UC8253) e-paper controller on the Xteink X3: 792x528
 * glass on an 800x600 controller, 3-wire SPI where SDA (MOSI) doubles as the
 * read line, DC/RST on GPIO, BUSY_N on GPIO (low = busy).
 *
 * Bytes arrive from the SPI controller (SSI bus) and, for the firmware's
 * controller probe, as bit-banged SCL/SDA edges on GPIO. The VER (0x70) read
 * answers 00 03 66 00 00 (LUT_VER 0x66 = the X3's UC8279d); with "uc8253"
 * nothing answers and SDA floats high, which is how the firmware recognizes
 * the UC8253 run of the X3. FLG (0x71) gives BUSY_N in bit 0.
 *
 * Modeled: PSR (0x00) REG bit, TRES (0x61), the partial window (0x90, 9
 * bytes) with PTIN/PTOUT (0x91/0x92), DTM1 (0x10, OLD) and DTM2 (0x13, NEW)
 * written inside the window (or the TRES frame), the register LUTs 0x20-0x24
 * and DRF (0x12). Ink, ghosting, grayscale mapping, OTP stand-ins and
 * animation use the X4 Pro UC8179 model: ordered per-frame saturation,
 * fast/slow remnant charge, blooming and drift, with identical defaults.
 * X3 LUTs are decoded in group/state/phase order, including VCOM drive.
 * CDI (0x50) N2OCP copies NEW to OLD after the refresh is latched.
 * BUSY_N is low for the longest of VCOM and the four transition rows,
 * plus refresh-overhead-us.
 * The measured X3 banks have equal row lengths: they do NOT establish which
 * row gates BUSY on silicon. Taking the maximum also handles a longer VCOM.
 * UC8253 groups have 6 bytes (levels, four frame counts, repeat count).
 * UC8279d groups have 7 (group repeat, four rail<<6|frames bytes, two state
 * repeats); each state repeats its pair of phases within the group repeat.
 * Zero repeats skip that group/state, not the rest of the table.
 *
 * PLL (0x30) scales frame-us relative to 0x09 (UC8253) / 0x0f (UC8279d).
 * Rate tables: six-byte LUT family UC8179c C0.6 p24 (UC8253), seven-byte
 * family UC8253c A0.61 p26 (UC8279d). These revision mappings are inferred,
 * not a measured PLL sweep or matching-silicon documentation. frame-us is an
 * effective panel period, not the reciprocal of the nominal PLL frequency.
 * PON/POF use pon-ms/pof-ms. See eink-qemu/sdcal/panel.md for calibration and
 * remaining revision uncertainties. Plane bit 1 = white; DSLP sleeps until RST.
 * Shown portrait (528x792) like the device, panel row 0 at the right.
 *
 * This program is free software; you can redistribute it and/or modify
 * it under the terms of the GNU General Public License version 2 or
 * (at your option) any later version.
 */
#include "qemu/osdep.h"
#include <math.h>
#include "qemu/log.h"
#include "qapi/error.h"
#include "qapi/visitor.h"
#include "qemu/module.h"
#include "qemu/timer.h"
#include "hw/ssi/ssi.h"
#include "hw/core/irq.h"
#include "hw/core/qdev-properties.h"
#include "ui/console.h"
#include "ui/pixel_ops.h"
#include "trace.h"

#define TYPE_UC8279 "uc8279"
OBJECT_DECLARE_SIMPLE_TYPE(Uc8279State, UC8279)

#define W 792                   /* glass */
#define H 528
#define RAM_W 800               /* controller RAM */
#define RAM_H 600
#define RAM_WB (RAM_W / 8)
#define LUT_ROWS 5
#define LUT_LEN 64
#define PSR_REG 0x20
#define CDI_N2OCP 0x08
#define MAX_FRAMES 8192
#define SHADES 1024
#define L_WHITE 94.5f
#define L_BLACK 4.7f

/* Same particle and charge state as the X4 Pro UC8179 model. */
typedef struct Uc8279Ink {
    float p;
    float qf, qs;
    float t_drive;
} Uc8279Ink;

enum { PLANE_OLD, PLANE_NEW };

struct Uc8279State {
    SSIPeripheral parent_obj;
    QemuConsole *con;
    qemu_irq busy_n;
    qemu_irq sda_out;
    QEMUTimer busy_timer;
    QEMUTimer anim_timer;
    QEMUTimer drift_timer;
    int64_t drift_deadline;
    bool ghosting;          /* physical ink artifacts; off for pixel QA */
    bool animate;
    int anim_f, anim_n;
    double anim_t0;
    uint32_t anim_frame_us; /* PLL-scaled period latched for this refresh */
    bool uc8253;
    bool portrait;
    uint32_t busy_ms;       /* fixed DRF BUSY time; 0 = the LUT's frames */
    uint32_t frame_us;
    uint32_t refresh_overhead_us;
    uint32_t pon_ms;
    uint32_t pof_ms;
    uint8_t swing;          /* frames of drive for a full black <-> white swing */
    uint32_t rail_soft;
    uint8_t otp_fast_frames, otp_full_frames;
    uint32_t otp_hold_drive;
    uint32_t remnant_fast, remnant_fast_ms;
    uint32_t remnant_slow, remnant_slow_ms;
    uint32_t bloom;
    uint32_t drift, drift_s;

    bool dc;
    bool in_reset;
    bool asleep;
    bool busy;
    bool refreshing;        /* BUSY belongs to a DRF */
    int64_t refresh_start_ns;
    uint64_t refreshes;     /* completed DRFs, and their BUSY time, since start */
    uint64_t refresh_ns;
    uint8_t cmd;
    uint32_t pos;           /* byte index into the data of the current command */
    int64_t plane_start_ns;
    uint8_t psr;
    uint8_t cdi;
    uint8_t pll;
    uint16_t tres_w, tres_h;
    bool partial;
    uint16_t win[4];        /* xs, xe, ys, ye */
    uint8_t winbuf[9];
    uint8_t lut[LUT_ROWS][LUT_LEN];
    uint8_t lut_n[LUT_ROWS];

    /* bit-banged SPI on GPIO */
    bool sclk;
    bool sda_in;
    uint8_t shift;
    int bits;
    const uint8_t *rd;
    uint32_t rd_len;

    uint8_t ram[2][RAM_H][RAM_WB];
    Uc8279Ink *ink;
    uint8_t cls[H][W];
    float (*frames)[4];
    double t_refresh, t_drift;
    uint8_t shade[SHADES + 1];
    bool redraw;
};

static const uint8_t uc8279_ver[] = { 0x00, 0x03, 0x66, 0x00, 0x00 };

static void uc8279_set_busy(Uc8279State *s, uint64_t us)
{
    s->refreshing = false;
    s->busy = true;
    qemu_set_irq(s->busy_n, 0);
    timer_mod(&s->busy_timer, qemu_clock_get_us(QEMU_CLOCK_VIRTUAL) + us);
}

static void uc8279_busy_done(void *opaque)
{
    Uc8279State *s = opaque;
    if (s->refreshing) {
        s->refreshing = false;
        s->refreshes++;
        s->refresh_ns += qemu_clock_get_ns(QEMU_CLOCK_VIRTUAL) - s->refresh_start_ns;
    }
    s->busy = false;
    qemu_set_irq(s->busy_n, 1);
}

/* Decode timing without truncating it to the ink engine's frame limit. */
static int uc8279_row_frames(Uc8279State *s, int r)
{
    int total = 0, size = s->uc8253 ? 6 : 7;

    for (int g = 0; g + size <= s->lut_n[r]; g += size) {
        const uint8_t *p = s->lut[r] + g;
        for (int ph = 0; ph < 4; ph++) {
            int frames = s->uc8253 ? p[1 + ph] : p[1 + ph] & 0x3f;
            int repeat = s->uc8253 ? p[5] : p[0] * p[5 + ph / 2];
            total += frames * repeat;
        }
    }
    return total;
}

/* Resolve a frame in chronological group/state/phase repeat order. */
static int uc8279_lut_level(Uc8279State *s, int r, int frame)
{
    int size = s->uc8253 ? 6 : 7;

    for (int g = 0; g + size <= s->lut_n[r]; g += size) {
        const uint8_t *p = s->lut[r] + g;
        int f[4], cycle, total;

        for (int ph = 0; ph < 4; ph++) {
            f[ph] = s->uc8253 ? p[1 + ph] : p[1 + ph] & 0x3f;
        }
        cycle = s->uc8253 ? f[0] + f[1] + f[2] + f[3] :
                (f[0] + f[1]) * p[5] + (f[2] + f[3]) * p[6];
        total = cycle * (s->uc8253 ? p[5] : p[0]);
        if (frame >= total) {
            frame -= total;
            continue;
        }
        frame %= cycle;
        if (s->uc8253) {
            for (int ph = 0; ph < 4; ph++) {
                if (frame < f[ph]) {
                    return (p[0] >> (6 - 2 * ph)) & 3;
                }
                frame -= f[ph];
            }
        } else {
            int pair = frame < (f[0] + f[1]) * p[5] ? 0 : 2;
            if (pair) {
                frame -= (f[0] + f[1]) * p[5];
            }
            frame %= f[pair] + f[pair + 1];
            return p[1 + pair + (frame >= f[pair])] >> 6;
        }
    }
    return 0;
}

/* X3 LUT encodings feed the same per-class drive as the UC8179 engine. */
static int uc8279_lut_frames(Uc8279State *s, int row_frames[LUT_ROWS])
{
    static const int level[4] = { 0, 1, -1, 0 };
    static const int row_of[4] = { 4, 2, 3, 1 };
    int frames = 0;
    bool vdhr = false;

    for (int r = 0; r < LUT_ROWS; r++) {
        row_frames[r] = uc8279_row_frames(s, r);
        frames = MAX(frames, row_frames[r]);
    }
    if (frames > MAX_FRAMES) {
        qemu_log_mask(LOG_GUEST_ERROR,
                      "uc8279: LUT over %d frames, cut ink playback\n",
                      MAX_FRAMES);
    }
    s->anim_n = MIN(frames, MAX_FRAMES);
    for (int f = 0; f < s->anim_n; f++) {
        int vcom = uc8279_lut_level(s, 0, f);
        for (int k = 0; k < 4; k++) {
            int source = uc8279_lut_level(s, row_of[k], f);
            vdhr |= source == 3;
            s->frames[f][k] = level[vcom] - level[source];
        }
    }
    if (vdhr) {
        qemu_log_mask(LOG_UNIMP, "uc8279: VDHR in a LUT row not modeled\n");
    }
    return frames;
}

static unsigned uc8279_pll_hz(bool uc8253, uint8_t pll)
{
    static const uint8_t six_byte_rates[] = {
        5, 10, 15, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 130, 150, 200
    };
    unsigned frs = pll & 0x1f;

    if (uc8253) {
        return six_byte_rates[pll & 0x0f];
    }
    return frs < 24 ? 5 * (frs + 1) : 130 + 10 * (frs - 24);
}

static uint32_t uc8279_frame_us(Uc8279State *s)
{
    unsigned ref_hz = uc8279_pll_hz(s->uc8253, s->uc8253 ? 0x09 : 0x0f);
    uint64_t period = DIV_ROUND_UP((uint64_t)s->frame_us * ref_hz,
                                  uc8279_pll_hz(s->uc8253, s->pll));

    return MIN(period, UINT32_MAX);
}

static uint64_t uc8279_refresh_us(Uc8279State *s, unsigned frames)
{
    uint64_t us;

    if (s->busy_ms) {
        return (uint64_t)s->busy_ms * 1000;
    }
    us = (uint64_t)frames * uc8279_frame_us(s) + s->refresh_overhead_us;
    return MIN(MAX(us, 1), (uint64_t)UINT32_MAX * 1000);
}

static double uc8279_now(void)
{
    return qemu_clock_get_ns(QEMU_CLOCK_VIRTUAL) / 1e9;
}

/* Stand-in OTP waveforms; returns the frame count. */
static int uc8279_otp_frames(Uc8279State *s)
{
    int n = 0;

    if (s->partial) {
        float h = s->ghosting ? s->otp_hold_drive / 1000.0f : 0;
        for (int i = 0; i < s->otp_fast_frames; i++, n++) {
            memcpy(s->frames[n], (float[4]) { -h, 1, -1, h },
                   sizeof(s->frames[n]));
        }
    } else {
        for (int i = 0; i < 2 * s->otp_full_frames; i++, n++) {
            float to_new = i < s->otp_full_frames ? -1 : 1;
            memcpy(s->frames[n],
                   (float[4]) { -to_new, to_new, -to_new, to_new },
                   sizeof(s->frames[n]));
        }
    }
    return n;
}

/*
 * Moves position p by a (in swings; + toward white). Linear until "sigma"
 * short of the rail it moves toward, then an exponential approach.
 */
static inline float ink_move(float p, float a, float sigma)
{
    float x = a > 0 ? 1 - p : p, d = fabsf(a);

    if (sigma <= 0) {
        x = MAX(x - d, 0);
    } else if (x - d >= sigma) {
        x -= d;
    } else {
        float lin = MAX(x - sigma, 0);
        x = MIN(x, sigma) * expf(-(d - lin) / sigma);
    }
    return a > 0 ? 1 - x : x;
}

/* Drift toward mid gray; optionally leak charge since the last refresh. */
static void uc8279_relax(Uc8279State *s, double now, bool leak)
{
    float rf = s->remnant_fast_ms ?
        expf(-(now - s->t_refresh) * 1e3 / s->remnant_fast_ms) : 0;
    float rs = s->remnant_slow_ms ?
        expf(-(now - s->t_refresh) * 1e3 / s->remnant_slow_ms) : 0;
    bool drift = s->drift && s->drift_s && now > s->t_drift;
    float amt = s->drift / 1000.0f, tau = s->drift_s;

    if (!s->ghosting) {
        s->t_drift = now;
        return;
    }

    for (int i = 0; i < H * W; i++) {
        Uc8279Ink *k = &s->ink[i];
        if (leak) {
            k->qf *= rf;
            k->qs *= rs;
        }
        if (drift) {
            /* dp/dt = (1/2 - p) amt/tau e^(-age/tau): at most amt of the way */
            float pull = amt * (expf(-(s->t_drift - k->t_drive) / tau) -
                                expf(-(now - k->t_drive) / tau));
            k->p += (0.5f - k->p) * pull;
        }
    }
    s->t_drift = now;
}

/*
 * Use fixed virtual-time steps, never host display callbacks.  The existing
 * first-order pull is not composable, so its step boundaries are part of the
 * optical model.  Restart the one-second cadence when a refresh finishes.
 * Use the scheduled deadline even if the main loop dispatches us late.
 */
static void uc8279_drift_tick(void *opaque)
{
    Uc8279State *s = opaque;
    int64_t deadline = s->drift_deadline;

    uc8279_relax(s, deadline / 1e9, false);
    s->redraw = true;
    s->drift_deadline = deadline + 1000000000;
    timer_mod(&s->drift_timer, s->drift_deadline);
}

/* Runs refresh frame f over the whole panel. */
static void uc8279_run_frame(Uc8279State *s, int f)
{
    float dt = s->anim_frame_us / 1e6f, step = 1.0f / s->swing;
    float sigma = s->rail_soft / 1000.0f, bloom = s->bloom / 1000.0f;
    float kf = s->remnant_fast_ms ? expf(-dt * 1e3f / s->remnant_fast_ms) : 0;
    float ks = s->remnant_slow_ms ? expf(-dt * 1e3f / s->remnant_slow_ms) : 0;
    float gf = s->remnant_fast_ms ?
        s->remnant_fast * 1.0f / s->remnant_fast_ms : 0;
    float gs = s->remnant_slow_ms ?
        s->remnant_slow * 1.0f / s->remnant_slow_ms : 0;
    float t = s->anim_t0 + f * dt;
    const float *e = s->frames[f];
    bool edges = bloom && (e[0] != e[1] || e[0] != e[2] || e[0] != e[3]);

    for (int y = 0; y < H; y++) {
        const uint8_t *c = s->cls[y];
        for (int x = 0; x < W; x++) {
            Uc8279Ink *k = &s->ink[y * W + x];
            float d = e[c[x]];
            bool driven = fabsf(d) >= 0.5f;

            if (!s->ghosting) {
                /* Ideal linear ink retains LUT grayscale and undriven pixels. */
                k->p = ink_move(k->p, d * step, 0);
                continue;
            }
            if (edges && driven) {
                float nb = 0;
                nb += x > 0 ? e[c[x - 1]] - e[c[x]] : 0;
                nb += x < W - 1 ? e[c[x + 1]] - e[c[x]] : 0;
                nb += y > 0 ? e[s->cls[y - 1][x]] - e[c[x]] : 0;
                nb += y < H - 1 ? e[s->cls[y + 1][x]] - e[c[x]] : 0;
                d += bloom * nb;
            }
            /* field = fraction / tau * charge; per mille / ms = 1 / s */
            float a = d - gf * k->qf - gs * k->qs;
            k->qf = k->qf * kf + d * dt;
            k->qs = k->qs * ks + d * dt;
            if (a != 0) {
                k->p = ink_move(k->p, a * step, sigma);
            }
            if (driven) {
                k->t_drive = t;
            }
        }
    }
    s->redraw = true;
}

/* Runs the frames due by virtual time "now" (all of them if now < 0). */
static void uc8279_run_frames(Uc8279State *s, double now)
{
    double dt = s->anim_frame_us / 1e6;
    int due = now < 0 ? s->anim_n :
        MIN(s->anim_n, (int)((now - s->anim_t0) / dt + 0.000001));

    while (s->anim_f < due) {
        uc8279_run_frame(s, s->anim_f++);
    }
    if (s->anim_f == s->anim_n) {
        timer_del(&s->anim_timer);
        s->t_refresh = s->t_drift = s->anim_t0 + s->anim_n * dt;
        if (s->ghosting && s->drift && s->drift_s) {
            s->drift_deadline = ceil((s->t_drift + 1) * 1e9);
            timer_mod(&s->drift_timer, s->drift_deadline);
        }
    } else {
        /* Rounding down can rearm an expired timer before a frame is due. */
        int64_t deadline = ceil((s->anim_t0 + (s->anim_f + 1) * dt) * 1e9);

        timer_mod(&s->anim_timer,
                  MAX(deadline, qemu_clock_get_ns(QEMU_CLOCK_VIRTUAL) + 1));
    }
}

static void uc8279_anim_tick(void *opaque)
{
    Uc8279State *s = opaque;
    uc8279_run_frames(s, uc8279_now());
}

/* Start the copied ink engine using X3 waveform and BUSY timing. */
static int uc8279_refresh(Uc8279State *s, int row_frames[LUT_ROWS])
{
    double now = uc8279_now();
    int frames;

    /* Finish an active refresh without rewinding already-applied drift. */
    if (s->anim_f < s->anim_n) {
        uc8279_run_frames(s, -1);
    }
    timer_del(&s->drift_timer);
    uc8279_relax(s, now, now > s->t_refresh);
    for (int y = 0; y < H; y++) {
        int r = H - 1 - y;
        for (int x = 0; x < W; x++) {
            int o = (s->ram[PLANE_OLD][r][x / 8] >> (7 - x % 8)) & 1;
            int nw = (s->ram[PLANE_NEW][r][x / 8] >> (7 - x % 8)) & 1;
            s->cls[y][x] = o << 1 | nw;
        }
    }
    if (s->psr & PSR_REG) {
        frames = uc8279_lut_frames(s, row_frames);
    } else {
        memset(row_frames, 0, sizeof(int) * LUT_ROWS);
        frames = s->anim_n = uc8279_otp_frames(s);
    }
    s->anim_f = 0;
    s->anim_t0 = now;
    /* A zero period is the existing X3 turbo setting: render synchronously. */
    s->anim_frame_us = MAX(1, uc8279_frame_us(s));
    if (s->cdi & CDI_N2OCP) {
        memcpy(s->ram[PLANE_OLD], s->ram[PLANE_NEW], sizeof(s->ram[PLANE_NEW]));
    }
    uc8279_run_frames(s, s->animate && s->frame_us ? now : -1);
    return frames;
}

static void uc8279_set_window(Uc8279State *s, int xs, int xe, int ys, int ye)
{
    s->win[0] = MIN(xs, RAM_W - 1);
    s->win[1] = MIN(xe, RAM_W - 1);
    s->win[2] = MIN(ys, RAM_H - 1);
    s->win[3] = MIN(ye, RAM_H - 1);
}

static void uc8279_command(Uc8279State *s, uint8_t c)
{
    s->cmd = c;
    s->pos = 0;
    s->rd = NULL;
    s->rd_len = 0;
    if (c == 0x10 || c == 0x13) {
        s->plane_start_ns = qemu_clock_get_ns(QEMU_CLOCK_VIRTUAL);
    }

    switch (c) {
    case 0x02:  /* POF */
        uc8279_set_busy(s, (uint64_t)s->pof_ms * 1000);
        break;
    case 0x04:  /* PON */
        uc8279_set_busy(s, (uint64_t)s->pon_ms * 1000);
        break;
    case 0x12: {    /* DRF */
        int row_frames[LUT_ROWS];
        int frames = uc8279_refresh(s, row_frames);
        uint64_t us = uc8279_refresh_us(s, frames);

        trace_uc8279_refresh(row_frames[0], row_frames[1], row_frames[2],
                             row_frames[3], row_frames[4], s->pll,
                             DIV_ROUND_UP(us, 1000));
        uc8279_set_busy(s, us);
        s->refreshing = true;
        s->refresh_start_ns = qemu_clock_get_ns(QEMU_CLOCK_VIRTUAL);
        break;
    }
    case 0x20 ... 0x24:
        s->lut_n[c - 0x20] = 0;
        break;
    case 0x91:  /* PTIN */
        s->partial = true;
        break;
    case 0x92:  /* PTOUT */
        s->partial = false;
        break;
    case 0x70:
        if (!s->uc8253) {
            s->rd = uc8279_ver;
            s->rd_len = sizeof(uc8279_ver);
        }
        break;
    }
}

static void uc8279_data(Uc8279State *s, uint8_t v)
{
    switch (s->cmd) {
    case 0x50:
        if (s->pos++ == 0) {
            s->cdi = v;
        }
        break;
    case 0x30:
        if (s->pos++ == 0) {
            s->pll = v & (s->uc8253 ? 0x0f : 0x1f);
        }
        break;
    case 0x00:
        if (s->pos++ == 0) {
            s->psr = v;
        }
        break;
    case 0x07:
        if (v == 0xA5) {
            s->asleep = true;
        }
        break;
    case 0x20 ... 0x24: {
        int r = s->cmd - 0x20;
        if (s->lut_n[r] < LUT_LEN) {
            s->lut[r][s->lut_n[r]++] = v;
        }
        break;
    }
    case 0x61:  /* TRES: HRES[15:8] HRES[7:0] VRES[15:8] VRES[7:0] */
        if (s->pos < 4) {
            s->winbuf[s->pos] = v;
        }
        if (++s->pos == 4) {
            s->tres_w = s->winbuf[0] << 8 | s->winbuf[1];
            s->tres_h = s->winbuf[2] << 8 | s->winbuf[3];
        }
        break;
    case 0x90:  /* PTL: HRST HRED VRST VRED (2 bytes each), PT_SCAN */
        if (s->pos < 9) {
            s->winbuf[s->pos] = v;
        }
        if (++s->pos == 8) {
            uc8279_set_window(s, (s->winbuf[0] << 8 | s->winbuf[1]) & ~7,
                              (s->winbuf[2] << 8 | s->winbuf[3]) | 7,
                              s->winbuf[4] << 8 | s->winbuf[5],
                              s->winbuf[6] << 8 | s->winbuf[7]);
        }
        break;
    case 0x10:
    case 0x13: {
        int xs, xe, ys, ye;
        if (s->partial) {
            xs = s->win[0]; xe = s->win[1]; ys = s->win[2]; ye = s->win[3];
        } else {
            xs = 0; xe = MIN(s->tres_w, RAM_W) - 1; ys = 0; ye = MIN(s->tres_h, RAM_H) - 1;
        }
        int wb = xe / 8 - xs / 8 + 1;
        int row = ys + s->pos / wb;
        if (row <= ye) {
            s->ram[s->cmd == 0x10 ? PLANE_OLD : PLANE_NEW][row][xs / 8 + s->pos % wb] = v;
            s->pos++;
            if (s->pos == wb * (ye - ys + 1)) {
                trace_uc8279_plane_write(s->cmd, s->pos,
                    qemu_clock_get_ns(QEMU_CLOCK_VIRTUAL) - s->plane_start_ns);
            }
        }
        break;
    }
    }
}

static void uc8279_byte(Uc8279State *s, uint8_t v)
{
    if (s->in_reset || s->asleep) {
        return;
    }
    if (!s->dc) {
        uc8279_command(s, v);
    } else {
        uc8279_data(s, v);
    }
}

static uint32_t uc8279_transfer(SSIPeripheral *dev, uint32_t data)
{
    uc8279_byte(UC8279(dev), data);
    return 0xff;
}

static uint8_t uc8279_read_byte(Uc8279State *s)
{
    if (s->cmd == 0x71) {
        return s->busy ? 0x12 : 0x13;
    }
    if (!s->rd) {
        return 0xff;
    }
    return s->pos < s->rd_len ? s->rd[s->pos] : 0xff;
}

static bool uc8279_reading(Uc8279State *s)
{
    return s->dc && !s->in_reset && !s->uc8253 && (s->cmd == 0x71 || s->rd);
}

static void uc8279_present_bit(Uc8279State *s)
{
    if (uc8279_reading(s)) {
        qemu_set_irq(s->sda_out, (uc8279_read_byte(s) >> (7 - s->bits)) & 1);
    } else {
        qemu_set_irq(s->sda_out, 1);
    }
}

/*
 * The controller shifts read bits out on the falling edge, so a host may
 * sample while SCL is low or right after the rising edge.
 */
static void uc8279_set_sclk(void *opaque, int n, int level)
{
    Uc8279State *s = UC8279(opaque);
    bool rise = level && !s->sclk, fall = !level && s->sclk;

    s->sclk = level;
    if (s->parent_obj.cs) {     /* CS is active low */
        return;
    }
    if (uc8279_reading(s)) {
        if (fall) {
            if (++s->bits == 8) {
                s->bits = 0;
                s->pos++;
            }
            uc8279_present_bit(s);
        }
    } else if (rise) {
        s->shift = (s->shift << 1) | s->sda_in;
        if (++s->bits == 8) {
            s->bits = 0;
            uc8279_byte(s, s->shift);
        }
    }
}

static void uc8279_set_sda(void *opaque, int n, int level)
{
    UC8279(opaque)->sda_in = level != 0;
}

static void uc8279_set_dc(void *opaque, int n, int level)
{
    Uc8279State *s = UC8279(opaque);
    s->dc = level != 0;
    s->bits = 0;
    uc8279_present_bit(s);
}

static int uc8279_set_cs(SSIPeripheral *dev, bool level)
{
    Uc8279State *s = UC8279(dev);
    s->bits = 0;
    if (level) {
        qemu_set_irq(s->sda_out, 1);
    }
    return 0;
}

static void uc8279_reset_regs(Uc8279State *s)
{
    s->psr = 0x0F;
    s->cdi = 0x31;
    s->pll = s->uc8253 ? 0x09 : 0x0f;
    s->partial = false;
    s->tres_w = RAM_W;
    s->tres_h = RAM_H;
    uc8279_set_window(s, 0, RAM_W - 1, 0, RAM_H - 1);
    memset(s->lut_n, 0, sizeof(s->lut_n));
}

static void uc8279_set_rst(void *opaque, int n, int level)
{
    Uc8279State *s = UC8279(opaque);
    if (!level) {
        s->in_reset = true;
    } else if (s->in_reset) {
        s->in_reset = false;
        s->asleep = false;
        uc8279_reset_regs(s);
        s->cmd = 0;
        s->bits = 0;
        s->rd = NULL;
        uc8279_present_bit(s);
    }
}

static bool uc8279_update_display(void *opaque)
{
    Uc8279State *s = opaque;
    DisplaySurface *surface = qemu_console_surface(s->con);

    if (!s->redraw) {
        return true;
    }
    s->redraw = false;
    uint32_t *d = (uint32_t *)surface_data(surface);
    int stride = surface_stride(surface) / 4;
    for (int y = 0; y < H; y++) {
        for (int x = 0; x < W; x++) {
            float p = MIN(1.0f, MAX(0.0f, s->ink[y * W + x].p));
            uint8_t g = s->shade[(int)(p * SHADES + 0.5f)];
            int dx = s->portrait ? H - 1 - y : x, dy = s->portrait ? x : y;
            d[dy * stride + dx] = rgb_to_pixel32(g, g, g);
        }
    }
    qemu_console_update(s->con, 0, 0,
                        surface_width(surface), surface_height(surface));
    return true;
}

static void uc8279_invalidate(void *opaque)
{
    UC8279(opaque)->redraw = true;
}

static const GraphicHwOps uc8279_ops = {
    .invalidate = uc8279_invalidate,
    .gfx_update = uc8279_update_display,
};

static void uc8279_realize(SSIPeripheral *d, Error **errp)
{
    DeviceState *dev = DEVICE(d);
    Uc8279State *s = UC8279(d);

    if (!s->swing) {
        error_setg(errp, "uc8279: swing-frames must be at least 1");
        return;
    }
    memset(s->ram, 0xff, sizeof(s->ram));
    s->ink = g_new0(Uc8279Ink, H * W);
    for (int i = 0; i < H * W; i++) {
        s->ink[i].p = 1;
    }
    s->frames = g_malloc(MAX_FRAMES * sizeof(*s->frames));
    /* position -> L* (linear) -> sRGB */
    for (int i = 0; i <= SHADES; i++) {
        float l = L_BLACK + (L_WHITE - L_BLACK) * i / SHADES;
        float y = l > 8 ? powf((l + 16) / 116, 3) : l / 903.3f;
        float v = y <= 0.0031308f ? 12.92f * y :
            1.055f * powf(y, 1 / 2.4f) - 0.055f;
        s->shade[i] = MIN(255, (int)(v * 255 + 0.5f));
    }
    uc8279_reset_regs(s);
    s->anim_frame_us = MAX(1, uc8279_frame_us(s));
    s->redraw = true;
    s->con = qemu_graphic_console_create(dev, 0, &uc8279_ops, s);
    qemu_console_resize(s->con, s->portrait ? H : W, s->portrait ? W : H);
    timer_init_us(&s->busy_timer, QEMU_CLOCK_VIRTUAL, uc8279_busy_done, s);
    timer_init_ns(&s->anim_timer, QEMU_CLOCK_VIRTUAL, uc8279_anim_tick, s);
    timer_init_ns(&s->drift_timer, QEMU_CLOCK_VIRTUAL, uc8279_drift_tick, s);
    if (s->ghosting && s->drift && s->drift_s) {
        s->drift_deadline = qemu_clock_get_ns(QEMU_CLOCK_VIRTUAL) +
                            1000000000;
        timer_mod(&s->drift_timer, s->drift_deadline);
    }
    qdev_init_gpio_in_named(dev, uc8279_set_dc, "dc", 1);
    qdev_init_gpio_in_named(dev, uc8279_set_rst, "rst", 1);
    qdev_init_gpio_in_named(dev, uc8279_set_sclk, "sclk", 1);
    qdev_init_gpio_in_named(dev, uc8279_set_sda, "sda", 1);
    qdev_init_gpio_out_named(dev, &s->busy_n, "busy", 1);
    qdev_init_gpio_out_named(dev, &s->sda_out, "sda-out", 1);
}

static const Property uc8279_properties[] = {
    DEFINE_PROP_BOOL("uc8253", Uc8279State, uc8253, false),
    DEFINE_PROP_BOOL("portrait", Uc8279State, portrait, true),
    DEFINE_PROP_UINT32("busy-ms", Uc8279State, busy_ms, 0),
    DEFINE_PROP_UINT32("frame-us", Uc8279State, frame_us, 25000),
    DEFINE_PROP_UINT32("refresh-overhead-us", Uc8279State,
                       refresh_overhead_us, 0),
    DEFINE_PROP_UINT32("pon-ms", Uc8279State, pon_ms, 2),
    DEFINE_PROP_UINT32("pof-ms", Uc8279State, pof_ms, 2),
    DEFINE_PROP_UINT8("swing-frames", Uc8279State, swing, 6),
    DEFINE_PROP_BOOL("ghosting", Uc8279State, ghosting, false),
    DEFINE_PROP_BOOL("animate", Uc8279State, animate, true),
    DEFINE_PROP_UINT32("rail-soft", Uc8279State, rail_soft, 100),
    DEFINE_PROP_UINT8("otp-fast-frames", Uc8279State, otp_fast_frames, 10),
    DEFINE_PROP_UINT8("otp-full-frames", Uc8279State, otp_full_frames, 30),
    DEFINE_PROP_UINT32("otp-hold-drive", Uc8279State, otp_hold_drive, 6),
    DEFINE_PROP_UINT32("remnant-fast", Uc8279State, remnant_fast, 30),
    DEFINE_PROP_UINT32("remnant-fast-ms", Uc8279State, remnant_fast_ms, 1000),
    DEFINE_PROP_UINT32("remnant-slow", Uc8279State, remnant_slow, 10),
    DEFINE_PROP_UINT32("remnant-slow-ms", Uc8279State, remnant_slow_ms, 30000),
    DEFINE_PROP_UINT32("bloom", Uc8279State, bloom, 35),
    DEFINE_PROP_UINT32("drift", Uc8279State, drift, 50),
    DEFINE_PROP_UINT32("drift-s", Uc8279State, drift_s, 1800),
};

static bool uc8279_get_busy(Object *obj, Error **errp)
{
    return UC8279(obj)->busy;
}

static void uc8279_get_refreshes(Object *obj, Visitor *v, const char *name,
                                 void *opaque, Error **errp)
{
    uint64_t n = UC8279(obj)->refreshes;
    visit_type_uint64(v, name, &n, errp);
}

static void uc8279_get_refresh_ns(Object *obj, Visitor *v, const char *name,
                                  void *opaque, Error **errp)
{
    uint64_t ns = UC8279(obj)->refresh_ns;
    visit_type_uint64(v, name, &ns, errp);
}

static void uc8279_class_init(ObjectClass *klass, const void *data)
{
    DeviceClass *dc = DEVICE_CLASS(klass);
    SSIPeripheralClass *k = SSI_PERIPHERAL_CLASS(klass);

    k->realize = uc8279_realize;
    k->transfer = uc8279_transfer;
    k->set_cs = uc8279_set_cs;
    k->cs_polarity = SSI_CS_LOW;
    device_class_set_props(dc, uc8279_properties);
    /* Read-only BUSY state, for test drivers waiting for a refresh to end. */
    object_class_property_add_bool(klass, "busy", uc8279_get_busy, NULL);
    /* Completed refreshes and their total BUSY time, for firmware-neutral metrics. */
    object_class_property_add(klass, "refreshes", "uint64",
                              uc8279_get_refreshes, NULL, NULL, NULL);
    object_class_property_add(klass, "refresh-ns", "uint64",
                              uc8279_get_refresh_ns, NULL, NULL, NULL);
}

static const TypeInfo uc8279_info = {
    .name = TYPE_UC8279,
    .parent = TYPE_SSI_PERIPHERAL,
    .instance_size = sizeof(Uc8279State),
    .class_init = uc8279_class_init,
};

static void uc8279_register_types(void)
{
    type_register_static(&uc8279_info);
}

type_init(uc8279_register_types)
