/*
 * UltraChip UC8179 e-paper controller (800x480 visible, 800x600 addressed), as
 * wired on the Xteink X4 Pro: 3-wire SPI where SDA (MOSI) doubles as the
 * read line, DC/RST on GPIO, BUSY_N on GPIO (low = busy).
 *
 * Writes arrive two ways: bytes from the SPI controller (SSI bus), and
 * bit-banged SCL/SDA edges on GPIO, which the firmware uses for its
 * controller probe and for register reads. Reads answer the identity the
 * real X4 Pro panel gives (REV 0x70 = 00 00 01 FF FF, FLG 0x71 = 0x13 idle),
 * a fixed 25 C temperature (0x40), and the OTP (0xA2) of that panel: its
 * bank 0 head and per-temperature-range headers (VCOM_DC) as dumped from the
 * device; the waveform bodies, which were not dumped, read as 0x00.
 *
 * Models DTM1 (0x10, old plane) and DTM2 (0x13, new plane) and display
 * refresh (0x12), which simulates the ink (KW mode, plane bit 1 = white). A
 * refresh is a list of frames giving the drive per pixel class (OLD/NEW
 * bits), played one per "frame-us" of virtual time so flashes and paints
 * show ("animate"=false applies them at once). BUSY_N stays low for the
 * frames' duration, or "busy-ms" if set.
 *  - PSR (0x00) REG=1: the register LUTs (0x20 VCOM, 0x21 WW, 0x22 KW, 0x23
 *    WK, 0x24 KK; 6-byte groups [levels, TP_A..TP_D, RP]); drive = source -
 *    VCOM, VDH -> black, VDL -> white. VDHR (11) on a source row is not modeled.
 *  - REG=0, OTP waveform (bodies not dumped): inside PTIN/PTOUT (0x91/0x92,
 *    whole panel; no 0x90 window) changed pixels get "otp-fast-frames" toward
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
 * Other commands (power, booster, PLL, VCOM, temperature setting) are
 * accepted and ignored.
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
    bool animate;
    int anim_f, anim_n;     /* next frame to run, frames in the refresh */
    double anim_t0;         /* virtual seconds the refresh started */
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
static const uint8_t uc8179_temp[] = { 25, 0x00 };

/* OTP bank 0 as read from an X4 Pro panel: 0x000..0x01E, then TR headers at OTP_TR(n). */
static const uint8_t uc8179_otp_head[] = {
    0xA5, 0x05, 0x0A, 0x0F, 0x14, 0x50, 0x5A, 0x64, 0x7F, 0xFF, 0xFF, 0xFF, 0xA5, 0x1F, 0x27, 0x27,
    0x36, 0x17, 0x00, 0x29, 0x07, 0x22, 0x64, 0x02, 0x58, 0x00, 0x00, 0x00, 0x00, 0x03, 0x00,
};
static const uint8_t uc8179_otp_vcom[8] = { 0x2E, 0x2E, 0x2E, 0x22, 0x22, 0x1A, 0x22, 0x26 };

static void uc8179_set_busy(Uc8179State *s, uint32_t ms)
{
    s->busy = true;
    qemu_set_irq(s->busy_n, 0);
    timer_mod(&s->busy_timer, qemu_clock_get_ms(QEMU_CLOCK_VIRTUAL) + ms);
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
        float h = s->otp_hold_drive / 1000.0f;
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
    float dt = s->frame_us / 1e6f, step = 1.0f / s->swing;
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
            float d = e[c[x]];
            bool driven = fabsf(d) >= 0.5f;
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
static void uc8179_run_frames(Uc8179State *s, double now)
{
    double dt = s->frame_us / 1e6;
    int due = now < 0 ? s->anim_n : MIN(s->anim_n, (int)((now - s->anim_t0) / dt + 1e-6));

    while (s->anim_f < due) {
        uc8179_run_frame(s, s->anim_f++);
    }
    if (s->anim_f == s->anim_n) {
        timer_del(&s->anim_timer);
        s->t_refresh = s->t_drift = s->anim_t0 + s->anim_n * dt;
        if (s->drift && s->drift_s) {
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

static void uc8179_anim_tick(void *opaque)
{
    Uc8179State *s = opaque;
    uc8179_run_frames(s, uc8179_now());
}

/* Starts a refresh; returns its duration in ms. */
static uint32_t uc8179_refresh(Uc8179State *s)
{
    double now = uc8179_now();

    /* Finish an active refresh without rewinding already-applied drift. */
    if (s->anim_f < s->anim_n) {
        uc8179_run_frames(s, -1);
    }
    timer_del(&s->drift_timer);
    uc8179_relax(s, now, now > s->t_refresh);
    /* Apply the controller's gate-scan and source-shift directions. */
    for (int y = 0; y < H; y++) {
        int r = s->psr & PSR_UD ? H - 1 - y : y;
        for (int x = 0; x < W; x++) {
            int c = s->psr & PSR_SHL ? x : W - 1 - x;
            int o = (s->ram[PLANE_OLD][r][c / 8] >> (7 - c % 8)) & 1;
            int nw = (s->ram[PLANE_NEW][r][c / 8] >> (7 - c % 8)) & 1;
            s->cls[y][x] = o << 1 | nw;
        }
    }
    s->anim_n = s->psr & PSR_REG ? uc8179_lut_frames(s) : uc8179_otp_frames(s);
    s->anim_f = 0;
    s->anim_t0 = now;
    if (s->cdi & CDI_N2OCP) {
        memcpy(s->ram[PLANE_OLD], s->ram[PLANE_NEW], sizeof(s->ram[PLANE_NEW]));
    }
    uc8179_run_frames(s, s->animate ? now : -1);
    return MAX(1, (uint64_t)s->anim_n * s->frame_us / 1000);
}

static void uc8179_command(Uc8179State *s, uint8_t c)
{
    s->cmd = c;
    s->pos = 0;
    s->rd = NULL;
    s->rd_len = 0;

    switch (c) {
    case 0x02:  /* POF */
    case 0x04:  /* PON */
        uc8179_set_busy(s, 2);
        break;
    case 0x07:  /* DSLP; the check code 0xA5 follows as data */
        break;
    case 0x12: {    /* DRF */
        uint32_t ms = uc8179_refresh(s);
        uc8179_set_busy(s, s->busy_ms ? s->busy_ms : ms);
        break;
    }
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
        s->rd = uc8179_rev;
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
        }
        break;
    case 0x50:
        if (s->pos++ == 0) {
            s->cdi = v;
        }
        break;
    case 0x10:
    case 0x13:
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
    return s->pos < s->rd_len ? s->rd[s->pos] : s->rd[s->rd_len - 1];
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
    memset(s->lut, 0, sizeof(s->lut));
}

static void uc8179_set_rst(void *opaque, int n, int level)
{
    Uc8179State *s = UC8179(opaque);
    if (!level) {
        s->in_reset = true;
        /* The panel stops being driven mid-refresh; idle drift resumes. */
        if (s->anim_f < s->anim_n) {
            double dt = s->frame_us / 1e6;

            /* Charge is integrated through the last completed frame. */
            s->t_refresh = s->t_drift = s->anim_t0 + s->anim_f * dt;
            if (s->drift && s->drift_s) {
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
    timer_init_ms(&s->busy_timer, QEMU_CLOCK_VIRTUAL, uc8179_busy_done, s);
    timer_init_ns(&s->anim_timer, QEMU_CLOCK_VIRTUAL, uc8179_anim_tick, s);
    timer_init_ns(&s->drift_timer, QEMU_CLOCK_VIRTUAL, uc8179_drift_tick, s);
    if (s->drift && s->drift_s) {
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

static void uc8179_register_types(void)
{
    type_register_static(&uc8179_info);
}

type_init(uc8179_register_types)
