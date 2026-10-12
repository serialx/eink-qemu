/*
 * UltraChip UC8179 / UC8279 revision 0x68 (800x480 visible, 800x600 addressed), as
 * wired on the Xteink X4 Pro: 3-wire SPI where SDA (MOSI) doubles as the
 * read line, DC/RST on GPIO, BUSY_N on GPIO (low = busy).
 *
 * Writes arrive two ways: bytes from the SPI controller (SSI bus), and
 * bit-banged SCL/SDA edges on GPIO, which the firmware uses for its
 * controller probe and for register reads. Reads answer the identity the
 * selected X4 Pro panel gives (UC8179 REV = 00 00 01 FF FF;
 * UC8279-68 REV = 00 0F 68 01 00; FLG 0x71 = 0x13 idle),
 * a fixed 25 C temperature (0x40), and the OTP (0xA2) of that panel: its
 * bank 0 head and per-temperature-range headers (VCOM_DC) as dumped from the
 * device; UC8279-68 supplies its measured 48-byte header. Waveform bodies,
 * which were not dumped, read as 0x00.
 *
 * Models DTM1 (0x10, old plane) and DTM2 (0x13, new plane) and display
 * refresh (0x12), which simulates the ink (KW mode, plane bit 1 = white). A
 * refresh is a list of frames giving the drive per pixel class (OLD/NEW
 * bits), played across the refresh's virtual BUSY duration so flashes and
 * paints show ("animate"=false applies them at once). Register timing uses
 * "frame-us" plus "refresh-overhead-us"; OTP timing has independent profile
 * defaults. "busy-ms", when nonzero, overrides both.
 *  - PSR (0x00) REG=1: the register LUTs (0x20 VCOM, 0x21 WW, 0x22 KW, 0x23
 *    WK, 0x24 KK; 6-byte groups [levels, TP_A..TP_D, RP]); drive = source -
 *    VCOM, VDH -> black, VDL -> white. UC8279-68 uses seven-byte groups:
 *    repeat, four level/duration phases, pair-1 repeat, pair-2 repeat.
 *    VDHR (11) on a source row is not modeled.
 *  - REG=0, OTP waveform (bodies not dumped): inside PTIN/PTOUT (0x91/0x92,
 *    UC8279-68 respects the 0x90 window) changed pixels get "otp-fast-frames" toward
 *    NEW and held ones a weak "otp-hold-drive" (per mille) toward their own
 *    color, which wears a ghost down over later fast refreshes as the panel
 *    does; otherwise every pixel gets "otp-full-frames" away from NEW, then as
 *    many toward it.
 *  - CDI (0x50) N2OCP: NEW is copied to OLD after the refresh.
 * Per pixel and frame (ghosting mechanisms from the e-paper literature):
 *  - particle position moves 1/"swing-frames" of a full swing; the last
 *    "rail-soft" of the way to black or white is an exponential approach, so
 *    short drives fall short and long ones saturate (and erase history);
 *  - remnant voltage: two charges (fast, slow) integrate the applied drive and
 *    leak with their time constants; their field opposes it ("remnant-*" is
 *    its steady-state fraction), so the next update lands by history and held
 *    pixels kick back where the last image changed;
 *  - blooming: a driven pixel loses part of its drive to fringe fields
 *    toward 4-neighbors driven differently ("bloom" per mille per neighbor),
 *    so thin strokes land short; pixels without a full drive are left alone
 *    (their field is mostly lateral), which keeps halos from piling up;
 *  - drift: between updates, ink relaxes toward mid gray by at most "drift"
 *    per mille, with time constant "drift-s" since it was last driven.
 * Shown lightness is linear in position (L* from black to white), which puts
 * the vendor gray LUT's levels at even steps.
 * UC8279-68 maps gates 120..599 to the visible panel and honors TRES and
 * partial RAM addressing. Timing is fitted only at the observed PLL=0x0e;
 * other PLL values are recorded in traces but do not scale the clock.
 * Power commands assert BUSY. Booster, VCOM and temperature settings are
 * accepted without electrical effects. Optical parameters remain approximate.
 *
 * This program is free software; you can redistribute it and/or modify
 * it under the terms of the GNU General Public License version 2 or
 * (at your option) any later version.
 */
#include "qemu/osdep.h"
#include <math.h>
#include "qapi/error.h"
#include "qemu/log.h"
#include "qemu/module.h"
#include "qemu/timer.h"
#include "hw/ssi/ssi.h"
#include "hw/core/irq.h"
#include "hw/core/qdev-properties.h"
#include "ui/console.h"
#include "ui/pixel_ops.h"
#include "trace.h"

#define TYPE_UC8179 "uc8179"
OBJECT_DECLARE_SIMPLE_TYPE(Uc8179State, UC8179)

#define W 800
#define H 480
#define H_ADDR 600
#define WB (W / 8)
#define OTP_SIZE 0x1000
#define OTP_TR(n) (0x49 + (n) * 0xF7)
#define LUT_ROWS 5
#define LUT_LEN 60              /* 10 groups; the X4 Pro driver writes 7 */
#define LUT_GROUPS (LUT_LEN / 6)
#define PSR_REG 0x20
#define PSR_UD 0x08
#define PSR_SHL 0x04
#define CDI_N2OCP 0x08
#define MAX_FRAMES 8192
#define SHADES 1024
#define L_WHITE 94.5f           /* L* of the shown white (sRGB 0xF0) and black (0x10) */
#define L_BLACK 4.7f

/* One pixel's ink. */
typedef struct Uc8179Ink {
    float p;                /* particle position: 0 black .. 1 white */
    float qf, qs;           /* remnant charge, fast and slow (drive x seconds) */
    float t_drive;          /* virtual seconds when it was last driven */
} Uc8179Ink;

enum { PLANE_OLD, PLANE_NEW };

struct Uc8179State {
    SSIPeripheral parent_obj;
    QemuConsole *con;
    qemu_irq busy_n;
    qemu_irq sda_out;
    QEMUTimer busy_timer;
    QEMUTimer anim_timer;
    QEMUTimer drift_timer;
    int64_t drift_deadline;
    uint32_t busy_ms;       /* fixed DRF BUSY time; 0 = the frames' duration */
    bool uc8279_68;
    uint32_t pon_ms;
    uint32_t pof_ms;
    uint32_t refresh_overhead_us;
    uint32_t otp_fast_us, otp_full_us;
    bool ghosting;          /* physical ink artifacts; off for pixel QA */
    bool animate;
    int anim_f, anim_n;     /* next frame to run, frames in the refresh */
    double anim_t0;         /* virtual seconds the refresh started */
    uint64_t anim_frame_us;
    bool portrait;

    bool dc;
    bool in_reset;
    bool asleep;
    bool busy;
    uint8_t cmd;
    uint32_t pos;           /* byte index into the plane or LUT being written, or read index */
    uint8_t psr;            /* PSR byte 0 */
    uint8_t cdi;            /* CDI byte 0 */
    bool partial;           /* between PTIN and PTOUT */
    uint8_t lut[LUT_ROWS][LUT_LEN];
    uint8_t lut_n[LUT_ROWS];
    uint8_t args[9];
    uint8_t pll;
    bool otp_clock;
    uint16_t width, height;
    uint16_t win[4];        /* x start/end, y start/end in controller RAM */
    bool window_valid;
    uint8_t swing;          /* frames of one-way drive for a full black <-> white swing */
    uint32_t rail_soft;     /* per mille */
    uint8_t otp_fast_frames;
    uint8_t otp_full_frames;
    uint32_t otp_hold_drive;    /* per mille */
    uint32_t remnant_fast, remnant_fast_ms;
    uint32_t remnant_slow, remnant_slow_ms;
    uint32_t bloom;         /* per mille per neighbor */
    uint32_t drift, drift_s;
    uint32_t frame_us;

    /* bit-banged SPI on GPIO */
    bool sclk;
    bool sda_in;
    uint8_t shift;
    int bits;
    const uint8_t *rd;      /* read data for the current read command */
    uint32_t rd_len;

    uint8_t otp[OTP_SIZE];
    uint8_t ram[2][H_ADDR][WB];
    Uc8179Ink *ink;         /* H x W */
    uint8_t cls[H][W];      /* OLD << 1 | NEW of the current refresh */
    float (*frames)[4];     /* drive per class, + toward white */
    double t_refresh;       /* virtual seconds at the end of the last refresh */
    double t_drift;         /* drift applied up to here */
    uint8_t shade[SHADES + 1];
    bool redraw;
};

static const uint8_t uc8179_rev[] = { 0x00, 0x00, 0x01, 0xff, 0xff };
static const uint8_t uc8279_68_rev[] = { 0x00, 0x0f, 0x68, 0x01, 0x00 };
static const uint8_t uc8179_temp[] = { 25, 0x00 };

/* OTP bank 0 as read from an X4 Pro panel: 0x000..0x01E, then TR headers at OTP_TR(n). */
static const uint8_t uc8179_otp_head[] = {
    0xA5, 0x05, 0x0A, 0x0F, 0x14, 0x50, 0x5A, 0x64, 0x7F, 0xFF, 0xFF, 0xFF, 0xA5, 0x1F, 0x27, 0x27,
    0x36, 0x17, 0x00, 0x29, 0x07, 0x22, 0x64, 0x02, 0x58, 0x00, 0x00, 0x00, 0x00, 0x03, 0x00,
};
static const uint8_t uc8179_otp_vcom[8] = { 0x2E, 0x2E, 0x2E, 0x22, 0x22, 0x1A, 0x22, 0x26 };

/* Only this MTP header was captured; the waveform bodies are not known. */
static const uint8_t uc8279_68_otp_head[] = {
    0xa5, 0xa5, 0x1f, 0x20, 0x25, 0x25, 0x3c, 0x00,
    0x97, 0x02, 0x02, 0x03, 0x20, 0x02, 0x58, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0f,
    0x00, 0x00, 0x68, 0x01, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x05, 0x0a, 0x0f, 0x14, 0x50, 0x5f, 0x64, 0x7f,
};

static void uc8179_set_busy_us(Uc8179State *s, uint64_t us)
{
    int64_t now = qemu_clock_get_ns(QEMU_CLOCK_VIRTUAL);

    s->busy = true;
    qemu_set_irq(s->busy_n, 0);
    timer_mod(&s->busy_timer, now + MIN(us, (INT64_MAX - now) / 1000) * 1000);
}

static void uc8179_set_busy(Uc8179State *s, uint32_t ms)
{
    uc8179_set_busy_us(s, (uint64_t)ms * 1000);
}

static void uc8179_busy_done(void *opaque)
{
    Uc8179State *s = opaque;
    s->busy = false;
    qemu_set_irq(s->busy_n, 1);
}

/* Walks one LUT row frame by frame. */
typedef struct LutCursor {
    const uint8_t *row;
    int group, rep, phase, frame;
} LutCursor;

static bool lut_next(LutCursor *c, int *level)
{
    while (c->group < LUT_GROUPS) {
        const uint8_t *g = c->row + c->group * 6;
        if (c->rep >= g[5]) {
            c->group++;
            c->rep = c->phase = c->frame = 0;
        } else if (c->phase >= 4) {
            c->rep++;
            c->phase = c->frame = 0;
        } else if (c->frame >= g[1 + c->phase]) {
            c->phase++;
            c->frame = 0;
        } else {
            c->frame++;
            *level = (g[0] >> (6 - 2 * c->phase)) & 3;
            return true;
        }
    }
    return false;
}

static double uc8179_now(void)
{
    return qemu_clock_get_ns(QEMU_CLOCK_VIRTUAL) / 1e9;
}

static int64_t uc8179_deadline(double seconds)
{
    double ns = ceil(seconds * 1e9);

    return ns >= (double)INT64_MAX ? INT64_MAX : (int64_t)ns;
}

static unsigned uc8279_68_row_frames(Uc8179State *s, int row)
{
    unsigned frames = 0;

    for (int g = 0; g + 7 <= s->lut_n[row]; g += 7) {
        const uint8_t *p = s->lut[row] + g;

        for (int ph = 0; ph < 4; ph++) {
            frames += (p[ph + 1] & 0x3f) * p[0] * p[5 + ph / 2];
        }
    }
    return frames;
}

static int uc8279_68_lut_level(Uc8179State *s, int row, unsigned frame)
{
    for (int g = 0; g + 7 <= s->lut_n[row]; g += 7) {
        const uint8_t *p = s->lut[row] + g;
        unsigned f[4], pair_frames[2], cycle, total;

        for (int ph = 0; ph < 4; ph++) {
            f[ph] = p[ph + 1] & 0x3f;
        }
        pair_frames[0] = (f[0] + f[1]) * p[5];
        pair_frames[1] = (f[2] + f[3]) * p[6];
        cycle = pair_frames[0] + pair_frames[1];
        total = cycle * p[0];
        if (frame >= total) {
            frame -= total;
            continue;
        }
        frame %= cycle;
        int pair = frame < pair_frames[0] ? 0 : 2;

        if (pair) {
            frame -= pair_frames[0];
        }
        frame %= f[pair] + f[pair + 1];
        return p[1 + pair + (frame >= f[pair])] >> 6;
    }
    return 0;
}

static unsigned uc8279_68_lut_frames(Uc8179State *s)
{
    static const int level[] = { 0, 1, -1, 0 };
    static const int row_of[] = { 4, 2, 3, 1 };
    unsigned frames = 0;

    for (int r = 0; r < LUT_ROWS; r++) {
        frames = MAX(frames, uc8279_68_row_frames(s, r));
    }
    s->anim_n = MIN(frames, MAX_FRAMES);
    for (int f = 0; f < s->anim_n; f++) {
        int vcom = uc8279_68_lut_level(s, 0, f);

        for (int k = 0; k < 4; k++) {
            int source = uc8279_68_lut_level(s, row_of[k], f);

            s->frames[f][k] = level[vcom] - level[source];
        }
    }
    return frames;
}

/* Frames of the uploaded LUTs; returns the frame count. */
static int uc8179_lut_frames(Uc8179State *s)
{
    static const int level[4] = { 0, 1, -1, 0 };    /* GND, VDH, VDL, VDHR/float: + toward black */
    static const int row_of[4] = { 4, 2, 3, 1 };    /* class: KK, KW, WK, WW */
    LutCursor c[LUT_ROWS];
    bool vdhr = false;
    int n = 0;

    for (int r = 0; r < LUT_ROWS; r++) {
        c[r] = (LutCursor) { .row = s->lut[r] };
    }
    for (;;) {
        int l[LUT_ROWS] = { 0 };
        bool more = false;
        for (int r = 0; r < LUT_ROWS; r++) {
            more |= lut_next(&c[r], &l[r]);
        }
        if (!more) {
            break;
        }
        if (n == MAX_FRAMES) {
            qemu_log_mask(LOG_GUEST_ERROR, "uc8179: LUT over %d frames, cut\n", MAX_FRAMES);
            break;
        }
        for (int k = 0; k < 4; k++) {
            int r = row_of[k];
            vdhr |= l[r] == 3;
            s->frames[n][k] = level[l[0]] - level[l[r]];
        }
        n++;
    }
    if (vdhr) {
        qemu_log_mask(LOG_UNIMP, "uc8179: VDHR in a LUT row not modeled\n");
    }
    return n;
}

/* Stand-in OTP waveforms; returns the frame count. */
static int uc8179_otp_frames(Uc8179State *s)
{
    int n = 0;

    if (s->partial) {
        float h = s->ghosting ? s->otp_hold_drive / 1000.0f : 0;
        for (int i = 0; i < s->otp_fast_frames; i++, n++) {
            memcpy(s->frames[n], (float[4]) { -h, 1, -1, h }, sizeof(s->frames[n]));
        }
    } else {
        for (int i = 0; i < 2 * s->otp_full_frames; i++, n++) {
            float to_new = i < s->otp_full_frames ? -1 : 1;
            memcpy(s->frames[n], (float[4]) { -to_new, to_new, -to_new, to_new },
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

/* Ink drifts toward mid gray; with "leak", remnant charge leaks since the last refresh. */
static void uc8179_relax(Uc8179State *s, double now, bool leak)
{
    float rf = s->remnant_fast_ms ? expf(-(now - s->t_refresh) * 1e3 / s->remnant_fast_ms) : 0;
    float rs = s->remnant_slow_ms ? expf(-(now - s->t_refresh) * 1e3 / s->remnant_slow_ms) : 0;
    bool drift = s->drift && s->drift_s && now > s->t_drift;
    float amt = s->drift / 1000.0f, tau = s->drift_s;

    if (!s->ghosting) {
        s->t_drift = now;
        return;
    }

    for (int i = 0; i < H * W; i++) {
        Uc8179Ink *k = &s->ink[i];
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
static void uc8179_drift_tick(void *opaque)
{
    Uc8179State *s = opaque;
    int64_t deadline = s->drift_deadline;

    uc8179_relax(s, deadline / 1e9, false);
    s->redraw = true;
    s->drift_deadline = deadline + 1000000000;
    timer_mod(&s->drift_timer, s->drift_deadline);
}

/* Runs refresh frame f over the whole panel. */
static void uc8179_run_frame(Uc8179State *s, int f)
{
    float dt = s->anim_frame_us / 1e6f, step = 1.0f / s->swing;
    float sigma = s->rail_soft / 1000.0f, bloom = s->bloom / 1000.0f;
    float kf = s->remnant_fast_ms ? expf(-dt * 1e3f / s->remnant_fast_ms) : 0;
    float ks = s->remnant_slow_ms ? expf(-dt * 1e3f / s->remnant_slow_ms) : 0;
    float gf = s->remnant_fast_ms ? s->remnant_fast * 1.0f / s->remnant_fast_ms : 0;
    float gs = s->remnant_slow_ms ? s->remnant_slow * 1.0f / s->remnant_slow_ms : 0;
    float t = s->anim_t0 + f * dt;
    const float *e = s->frames[f];
    bool edges = bloom && (e[0] != e[1] || e[0] != e[2] || e[0] != e[3]);

    for (int y = 0; y < H; y++) {
        const uint8_t *c = s->cls[y];
        for (int x = 0; x < W; x++) {
            Uc8179Ink *k = &s->ink[y * W + x];
            if (c[x] >= 4) {
                continue;
            }
            float d = e[c[x]];
            bool driven = fabsf(d) >= 0.5f;

            if (!s->ghosting) {
                /* Ideal linear ink retains LUT grayscale and undriven pixels. */
                k->p = ink_move(k->p, d * step, 0);
                continue;
            }
            if (edges && driven) {
                float nb = 0;
                nb += x > 0 && c[x - 1] < 4 ? e[c[x - 1]] - e[c[x]] : 0;
                nb += x < W - 1 && c[x + 1] < 4 ? e[c[x + 1]] - e[c[x]] : 0;
                nb += y > 0 && s->cls[y - 1][x] < 4 ?
                      e[s->cls[y - 1][x]] - e[c[x]] : 0;
                nb += y < H - 1 && s->cls[y + 1][x] < 4 ?
                      e[s->cls[y + 1][x]] - e[c[x]] : 0;
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
static void uc8179_run_frames(Uc8179State *s, double now)
{
    double dt = s->anim_frame_us / 1e6;
    int due = now < 0 ? s->anim_n :
              MIN(s->anim_n, MAX(0, (now - s->anim_t0) / dt + 1e-6));

    while (s->anim_f < due) {
        uc8179_run_frame(s, s->anim_f++);
    }
    if (s->anim_f == s->anim_n) {
        timer_del(&s->anim_timer);
        s->t_refresh = s->t_drift = s->anim_t0 + s->anim_n * dt;
        if (s->ghosting && s->drift && s->drift_s) {
            s->drift_deadline = uc8179_deadline(s->t_drift + 1);
            timer_mod(&s->drift_timer, s->drift_deadline);
        }
    } else {
        /* Rounding down can rearm an expired timer before a frame is due. */
        int64_t deadline = uc8179_deadline(s->anim_t0 + (s->anim_f + 1) * dt);

        timer_mod(&s->anim_timer,
                  MAX(deadline, qemu_clock_get_ns(QEMU_CLOCK_VIRTUAL) + 1));
    }
}

static void uc8179_anim_tick(void *opaque)
{
    Uc8179State *s = opaque;
    uc8179_run_frames(s, uc8179_now());
}

static uint64_t uc8179_refresh_us(Uc8179State *s, unsigned frames)
{
    if (s->busy_ms) {
        return (uint64_t)s->busy_ms * 1000;
    }
    if (!(s->psr & PSR_REG)) {
        uint32_t otp_us = s->partial ? s->otp_fast_us : s->otp_full_us;

        if (otp_us) {
            return otp_us;
        }
    }
    if (s->uc8279_68 && !s->frame_us && !s->refresh_overhead_us) {
        /* Measured electrical BUSY, x4cal panel/matrix/vendor sweeps.
         * OTP refresh changes the internal clock until the next PLL write.
         * Only the observed 480-row partial window has a measured correction.
         * Other PLL/window values use the nearest measured baseline. */
        if (s->otp_clock) {
            uint32_t overhead = s->partial && s->window_valid &&
                s->win[3] - s->win[2] + 1 == 480 ? 84585 : 95870;
            return (uint64_t)frames * 18903 + overhead;
        }
        switch (s->pll) {
        case 0x0d: return (uint64_t)frames * 13378 + 84348;
        case 0x0f: return (uint64_t)frames * 11632 + 80690;
        default:   return (uint64_t)frames * 12506 + 82510;
        }
    }
    return MAX(1000, (uint64_t)frames * s->frame_us + s->refresh_overhead_us);
}

/* Starts a refresh; returns its duration in microseconds. */
static uint64_t uc8179_refresh(Uc8179State *s)
{
    double now = uc8179_now();
    unsigned frames;
    uint64_t us;

    /* Finish an active refresh without rewinding already-applied drift. */
    if (s->anim_f < s->anim_n) {
        uc8179_run_frames(s, -1);
    }
    timer_del(&s->drift_timer);
    uc8179_relax(s, now, now > s->t_refresh);
    /* Apply the controller's gate-scan and source-shift directions. */
    for (int y = 0; y < H; y++) {
        int r = (s->psr & PSR_UD ? H - 1 - y : y) +
                (s->uc8279_68 ? H_ADDR - H : 0);
        for (int x = 0; x < W; x++) {
            int c = s->psr & PSR_SHL ? x : W - 1 - x;
            if (s->uc8279_68 &&
                (r >= s->height || c >= s->width ||
                 (s->partial && (!s->window_valid ||
                  c < s->win[0] || c > s->win[1] ||
                  r < s->win[2] || r > s->win[3])))) {
                s->cls[y][x] = 4;
                continue;
            }
            int o = (s->ram[PLANE_OLD][r][c / 8] >> (7 - c % 8)) & 1;
            int nw = (s->ram[PLANE_NEW][r][c / 8] >> (7 - c % 8)) & 1;
            s->cls[y][x] = o << 1 | nw;
        }
    }
    if ((s->psr & PSR_REG) && s->uc8279_68) {
        frames = uc8279_68_lut_frames(s);
    } else {
        frames = s->anim_n = s->psr & PSR_REG ?
                 uc8179_lut_frames(s) : uc8179_otp_frames(s);
    }
    us = uc8179_refresh_us(s, frames);
    if (s->uc8279_68 && !(s->psr & PSR_REG)) {
        s->otp_clock = true;
    }
    us = MIN(us, (INT64_MAX - qemu_clock_get_ns(QEMU_CLOCK_VIRTUAL)) / 1000);
    s->anim_f = 0;
    s->anim_t0 = now;
    s->anim_frame_us = MAX(1, us / MAX(1, s->anim_n));
    if (s->cdi & CDI_N2OCP) {
        memcpy(s->ram[PLANE_OLD], s->ram[PLANE_NEW], sizeof(s->ram[PLANE_NEW]));
    }
    uc8179_run_frames(s, s->animate ? now : -1);
    trace_uc8179_refresh(s->uc8279_68, s->psr, s->pll, s->partial,
                        frames, us, qemu_clock_get_ns(QEMU_CLOCK_VIRTUAL));
    return us;
}

static void uc8179_command(Uc8179State *s, uint8_t c)
{
    s->cmd = c;
    s->pos = 0;
    s->rd = NULL;
    s->rd_len = 0;

    switch (c) {
    case 0x02:  /* POF */
        if (s->uc8279_68 && !s->pof_ms) {
            uc8179_set_busy_us(s, s->pll == 0x0d ? 53625 :
                                 s->pll == 0x0f ? 46624 : 50120);
        } else {
            uc8179_set_busy(s, s->pof_ms);
        }
        break;
    case 0x04:  /* PON */
        if (s->uc8279_68 && !s->pon_ms) {
            uc8179_set_busy_us(s, 39398);
        } else {
            uc8179_set_busy(s, s->pon_ms);
        }
        break;
    case 0x07:  /* DSLP; the check code 0xA5 follows as data */
        break;
    case 0x12: {    /* DRF */
        uint64_t us = uc8179_refresh(s);
        uc8179_set_busy_us(s, us);
        break;
    }
    case 0x20 ... 0x24:
        memset(s->lut[c - 0x20], 0, LUT_LEN);
        s->lut_n[c - 0x20] = 0;
        break;
    case 0x91:  /* PTIN */
        s->partial = true;
        break;
    case 0x92:  /* PTOUT */
        s->partial = false;
        break;
    case 0x40:  /* TSC: BUSY while sensing, then the reading is clocked out */
        uc8179_set_busy(s, 5);
        s->rd = uc8179_temp;
        s->rd_len = sizeof(uc8179_temp);
        break;
    case 0x70:
        s->rd = s->uc8279_68 ? uc8279_68_rev : uc8179_rev;
        s->rd_len = sizeof(uc8179_rev);
        break;
    case 0xA2:
        s->rd = s->otp;
        s->rd_len = OTP_SIZE;
        break;
    }
}

static void uc8179_data(Uc8179State *s, uint8_t v)
{
    switch (s->cmd) {
    case 0x00:
        if (s->pos++ == 0) {
            s->psr = v;
        }
        break;
    case 0x07:
        if (v == 0xA5) s->asleep = true;
        break;
    case 0x20 ... 0x24:
        if (s->pos < LUT_LEN) {
            s->lut[s->cmd - 0x20][s->pos++] = v;
            s->lut_n[s->cmd - 0x20] = s->pos;
        }
        break;
    case 0x30:
        if (s->pos++ == 0) {
            s->pll = v;
            s->otp_clock = false;
        }
        break;
    case 0x61:
    case 0x90:
        if (s->uc8279_68 && s->pos < sizeof(s->args)) {
            s->args[s->pos++] = v;
            if (s->cmd == 0x61 && s->pos == 4) {
                s->width = MIN(W, (s->args[0] << 8 | s->args[1]) & ~7);
                s->height = MIN(H_ADDR, s->args[2] << 8 | s->args[3]);
            } else if (s->cmd == 0x90 && s->pos == 9) {
                s->win[0] = (s->args[0] << 8 | s->args[1]) & ~7;
                s->win[1] = MIN(W - 1, (s->args[2] << 8 | s->args[3]) | 7);
                s->win[2] = s->args[4] << 8 | s->args[5];
                s->win[3] = MIN(H_ADDR - 1, s->args[6] << 8 | s->args[7]);
                s->window_valid = s->win[0] <= s->win[1] &&
                                  s->win[2] <= s->win[3];
            }
        }
        break;
    case 0x50:
        if (s->pos++ == 0) {
            s->cdi = v;
        }
        break;
    case 0x10:
    case 0x13:
        if (s->uc8279_68) {
            unsigned x = 0, y = 0, wb = s->width / 8, height = s->height;

            if (s->partial) {
                if (!s->window_valid) {
                    break;
                }
                x = s->win[0] / 8;
                y = s->win[2];
                wb = (s->win[1] - s->win[0] + 1) / 8;
                height = s->win[3] - s->win[2] + 1;
            }
            if (wb && s->pos < wb * height) {
                s->ram[s->cmd == 0x10 ? PLANE_OLD : PLANE_NEW]
                      [y + s->pos / wb][x + s->pos % wb] = v;
                s->pos++;
            }
            break;
        }
        if (s->pos < H_ADDR * WB) {
            s->ram[s->cmd == 0x10 ? PLANE_OLD : PLANE_NEW][s->pos / WB][s->pos % WB] = v;
            s->pos++;
        }
        break;
    }
}

static void uc8179_byte(Uc8179State *s, uint8_t v)
{
    if (s->in_reset || s->asleep) {
        return;
    }
    if (!s->dc) {
        uc8179_command(s, v);
    } else {
        uc8179_data(s, v);
    }
}

/* Next bit the controller presents on SDA during a register read. */
static uint8_t uc8179_read_byte(Uc8179State *s)
{
    if (s->cmd == 0x71) {
        return s->busy ? 0x12 : 0x13;
    }
    if (!s->rd) {
        return 0xff;
    }
    unsigned pos = s->pos;

    if (s->uc8279_68 && s->cmd == 0xa2) {
        /* RMTP clocks a dummy byte before MTP address zero. */
        if (!pos) {
            return 0xff;
        }
        pos--;
    }
    return pos < s->rd_len ? s->rd[pos] : s->rd[s->rd_len - 1];
}

static bool uc8179_reading(Uc8179State *s)
{
    return s->dc && !s->in_reset && (s->cmd == 0x71 || s->rd);
}

static uint32_t uc8179_transfer(SSIPeripheral *dev, uint32_t data)
{
    Uc8179State *s = UC8179(dev);

    if (uc8179_reading(s)) {
        uint8_t value = uc8179_read_byte(s);
        s->pos++;
        return value;
    }
    uc8179_byte(s, data);
    return 0xff;
}

static void uc8179_present_bit(Uc8179State *s)
{
    if (uc8179_reading(s)) {
        qemu_set_irq(s->sda_out, (uc8179_read_byte(s) >> (7 - s->bits)) & 1);
    } else {
        qemu_set_irq(s->sda_out, 1);
    }
}

static void uc8179_set_sclk(void *opaque, int n, int level)
{
    Uc8179State *s = UC8179(opaque);
    bool rise = level && !s->sclk;

    s->sclk = level;
    if (!rise || s->parent_obj.cs) {     /* CS is active low */
        return;
    }
    if (uc8179_reading(s)) {
        /* The host sampled the presented bit while SCL was low. */
        if (++s->bits == 8) {
            s->bits = 0;
            s->pos++;
        }
    } else {
        s->shift = (s->shift << 1) | s->sda_in;
        if (++s->bits == 8) {
            s->bits = 0;
            uc8179_byte(s, s->shift);
        }
    }
    uc8179_present_bit(s);
}

static void uc8179_set_sda(void *opaque, int n, int level)
{
    UC8179(opaque)->sda_in = level != 0;
}

static void uc8179_set_dc(void *opaque, int n, int level)
{
    Uc8179State *s = UC8179(opaque);
    s->dc = level != 0;
    s->bits = 0;
    uc8179_present_bit(s);
}

static int uc8179_set_cs(SSIPeripheral *dev, bool level)
{
    Uc8179State *s = UC8179(dev);
    s->bits = 0;
    if (level) {
        qemu_set_irq(s->sda_out, 1);
    }
    return 0;
}

static void uc8179_reset_regs(Uc8179State *s)
{
    s->psr = 0x0F;      /* REG=0: OTP waveforms */
    s->cdi = 0x31;      /* N2OCP off */
    s->partial = false;
    s->window_valid = false;
    s->width = W;
    s->height = H_ADDR;
    s->pll = s->uc8279_68 ? 0x0e : 0;
    s->otp_clock = false;
    s->anim_frame_us = MAX(1, s->frame_us);
    memset(s->lut, 0, sizeof(s->lut));
    memset(s->lut_n, 0, sizeof(s->lut_n));
}

static void uc8179_set_rst(void *opaque, int n, int level)
{
    Uc8179State *s = UC8179(opaque);
    if (!level) {
        s->in_reset = true;
        timer_del(&s->busy_timer);
        uc8179_busy_done(s);
        qemu_set_irq(s->sda_out, 1);
        /* The panel stops being driven mid-refresh; idle drift resumes. */
        if (s->anim_f < s->anim_n) {
            double dt = s->anim_frame_us / 1e6;

            /* Charge is integrated through the last completed frame. */
            s->t_refresh = s->t_drift = s->anim_t0 + s->anim_f * dt;
            if (s->ghosting && s->drift && s->drift_s) {
                s->drift_deadline = ceil((s->t_drift + 1) * 1e9);
                timer_mod(&s->drift_timer, s->drift_deadline);
            }
        }
        timer_del(&s->anim_timer);
        s->anim_n = s->anim_f;
    } else if (s->in_reset) {
        s->in_reset = false;
        s->asleep = false;
        uc8179_reset_regs(s);
        s->cmd = 0;
        s->bits = 0;
        s->rd = NULL;
    }
}

static bool uc8179_update_display(void *opaque)
{
    Uc8179State *s = opaque;
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

static void uc8179_invalidate(void *opaque)
{
    UC8179(opaque)->redraw = true;
}

static const GraphicHwOps uc8179_ops = {
    .invalidate = uc8179_invalidate,
    .gfx_update = uc8179_update_display,
};

static void uc8179_realize(SSIPeripheral *d, Error **errp)
{
    DeviceState *dev = DEVICE(d);
    Uc8179State *s = UC8179(d);

    memset(s->otp, 0x00, OTP_TR(12));
    memset(s->otp + OTP_TR(12), 0xff, OTP_SIZE - OTP_TR(12));
    memcpy(s->otp, uc8179_otp_head, sizeof(uc8179_otp_head));
    for (int n = 0; n < 12; n++) {
        static const uint8_t tr[] = { 0x67, 0xBF, 0x3F, 0x0D, 0x00, 0x00, 0x00 };
        memcpy(s->otp + OTP_TR(n), n < 8 ? tr : (const uint8_t[7]){ 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff }, 7);
        if (n < 8) {
            s->otp[OTP_TR(n) + 5] = uc8179_otp_vcom[n];
        }
    }
    if (s->uc8279_68) {
        memset(s->otp, 0, sizeof(s->otp));
        memcpy(s->otp, uc8279_68_otp_head, sizeof(uc8279_68_otp_head));
    }
    memset(s->ram, 0xff, sizeof(s->ram));
    if (!s->swing) {
        error_setg(errp, "uc8179: swing-frames must be at least 1");
        return;
    }
    s->ink = g_new0(Uc8179Ink, H * W);
    for (int i = 0; i < H * W; i++) {
        s->ink[i].p = 1;
    }
    s->frames = g_malloc(MAX_FRAMES * sizeof(*s->frames));
    /* position -> L* (linear) -> sRGB */
    for (int i = 0; i <= SHADES; i++) {
        float l = L_BLACK + (L_WHITE - L_BLACK) * i / SHADES;
        float y = l > 8 ? powf((l + 16) / 116, 3) : l / 903.3f;
        float v = y <= 0.0031308f ? 12.92f * y : 1.055f * powf(y, 1 / 2.4f) - 0.055f;
        s->shade[i] = MIN(255, (int)(v * 255 + 0.5f));
    }
    uc8179_reset_regs(s);
    s->redraw = true;
    s->con = qemu_graphic_console_create(dev, 0, &uc8179_ops, s);
    qemu_console_resize(s->con, s->portrait ? H : W, s->portrait ? W : H);
    timer_init_ns(&s->busy_timer, QEMU_CLOCK_VIRTUAL, uc8179_busy_done, s);
    timer_init_ns(&s->anim_timer, QEMU_CLOCK_VIRTUAL, uc8179_anim_tick, s);
    timer_init_ns(&s->drift_timer, QEMU_CLOCK_VIRTUAL, uc8179_drift_tick, s);
    if (s->ghosting && s->drift && s->drift_s) {
        s->drift_deadline = qemu_clock_get_ns(QEMU_CLOCK_VIRTUAL) +
                            1000000000;
        timer_mod(&s->drift_timer, s->drift_deadline);
    }
    qdev_init_gpio_in_named(dev, uc8179_set_dc, "dc", 1);
    qdev_init_gpio_in_named(dev, uc8179_set_rst, "rst", 1);
    qdev_init_gpio_in_named(dev, uc8179_set_sclk, "sclk", 1);
    qdev_init_gpio_in_named(dev, uc8179_set_sda, "sda", 1);
    qdev_init_gpio_out_named(dev, &s->busy_n, "busy", 1);
    qdev_init_gpio_out_named(dev, &s->sda_out, "sda-out", 1);
}

static const Property uc8179_properties[] = {
    DEFINE_PROP_UINT32("busy-ms", Uc8179State, busy_ms, 0),
    DEFINE_PROP_UINT32("pon-ms", Uc8179State, pon_ms, 2),
    DEFINE_PROP_UINT32("pof-ms", Uc8179State, pof_ms, 2),
    DEFINE_PROP_UINT32("refresh-overhead-us", Uc8179State, refresh_overhead_us, 0),
    DEFINE_PROP_UINT32("otp-fast-us", Uc8179State, otp_fast_us, 0),
    DEFINE_PROP_UINT32("otp-full-us", Uc8179State, otp_full_us, 0),
    DEFINE_PROP_BOOL("ghosting", Uc8179State, ghosting, false),
    DEFINE_PROP_BOOL("animate", Uc8179State, animate, true),
    DEFINE_PROP_BOOL("portrait", Uc8179State, portrait, true),
    /* 6: the vendor gray LUT's black + 2 / + 4 white frames read 1/3 and 2/3 */
    DEFINE_PROP_UINT8("swing-frames", Uc8179State, swing, 6),
    DEFINE_PROP_UINT32("rail-soft", Uc8179State, rail_soft, 100),
    DEFINE_PROP_UINT8("otp-fast-frames", Uc8179State, otp_fast_frames, 10),
    DEFINE_PROP_UINT8("otp-full-frames", Uc8179State, otp_full_frames, 30),
    DEFINE_PROP_UINT32("otp-hold-drive", Uc8179State, otp_hold_drive, 6),
    DEFINE_PROP_UINT32("remnant-fast", Uc8179State, remnant_fast, 30),
    DEFINE_PROP_UINT32("remnant-fast-ms", Uc8179State, remnant_fast_ms, 1000),
    DEFINE_PROP_UINT32("remnant-slow", Uc8179State, remnant_slow, 10),
    DEFINE_PROP_UINT32("remnant-slow-ms", Uc8179State, remnant_slow_ms, 30000),
    DEFINE_PROP_UINT32("bloom", Uc8179State, bloom, 35),
    DEFINE_PROP_UINT32("drift", Uc8179State, drift, 50),
    DEFINE_PROP_UINT32("drift-s", Uc8179State, drift_s, 1800),
    DEFINE_PROP_UINT32("frame-us", Uc8179State, frame_us, 25000),
};

static void uc8179_class_init(ObjectClass *klass, const void *data)
{
    DeviceClass *dc = DEVICE_CLASS(klass);
    SSIPeripheralClass *k = SSI_PERIPHERAL_CLASS(klass);

    k->realize = uc8179_realize;
    k->transfer = uc8179_transfer;
    k->set_cs = uc8179_set_cs;
    k->cs_polarity = SSI_CS_LOW;
    device_class_set_props(dc, uc8179_properties);
}

static const TypeInfo uc8179_info = {
    .name = TYPE_UC8179,
    .parent = TYPE_SSI_PERIPHERAL,
    .instance_size = sizeof(Uc8179State),
    .class_init = uc8179_class_init,
};

/* Instance defaults precede user globals, including globals on uc8179. */
static void uc8279_68_init(Object *obj)
{
    Uc8179State *s = UC8179(obj);

    s->uc8279_68 = true;
    /* Zero selects the measured state-dependent timing; nonzero overrides
     * select literal frame/overhead or millisecond power durations. */
    s->frame_us = 0;
    s->pon_ms = 0;
    s->pof_ms = 0;
    s->refresh_overhead_us = 0;
    s->otp_fast_us = 483000;
    s->otp_full_us = 1328000;
}

static const TypeInfo uc8279_68_info = {
    .name = "uc8279-68",
    .parent = TYPE_UC8179,
    .instance_init = uc8279_68_init,
};

static void uc8179_register_types(void)
{
    type_register_static(&uc8179_info);
    type_register_static(&uc8279_68_info);
}

type_init(uc8179_register_types)
