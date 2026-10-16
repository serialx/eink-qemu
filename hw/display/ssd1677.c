/*
 * Solomon SSD1677 e-paper controller (800x480, 1-bit RAM planes), as wired
 * on the Xteink X4 and Onyx Picco: SPI or GPIO bit banging, with a shared
 * bidirectional SDA line for panel identification. BUSY is active high.
 *
 * Models the BW (0x24) and RED (0x26) RAM planes with the data-entry mode,
 * RAM window and address counters, auto-write fills (0x46/0x47), display
 * update control 1 (0x21) RAM options, and master activation (0x20), which
 * holds BUSY for "busy-ms" and then shows the panel on a graphic console.
 * Waveforms are not simulated: OTP and monochrome transition LUTs show the
 * BW plane; grayscale LUTs interpret both RAM planes as four gray levels,
 * or as steps from the shown image when they leave a pixel code undriven.
 * Everything else (voltages, booster, temperature, border) is accepted and
 * ignored.
 *
 * This program is free software; you can redistribute it and/or modify
 * it under the terms of the GNU General Public License version 2 or
 * (at your option) any later version.
 */
#include "qemu/osdep.h"
#include "qemu/log.h"
#include "qemu/bitops.h"
#include "qemu/module.h"
#include "qemu/timer.h"
#include "hw/ssi/ssi.h"
#include "hw/core/irq.h"
#include "hw/core/qdev-properties.h"
#include "ui/console.h"
#include "ui/pixel_ops.h"

#define TYPE_SSD1677 "ssd1677"
OBJECT_DECLARE_SIMPLE_TYPE(Ssd1677State, SSD1677)

#define W 800
#define H 480
#define WB (W / 8)
#define MAX_ARGS 256

enum { PLANE_BW, PLANE_RED };

struct Ssd1677State {
    SSIPeripheral parent_obj;
    QemuConsole *con;
    qemu_irq busy;
    qemu_irq sda_out;
    QEMUTimer busy_timer;
    uint32_t busy_ms;
    bool portrait;      /* turn 90 degrees clockwise, as the X4 is held */

    bool dc;            /* 1 = data */
    bool sclk;
    bool sda_in;
    uint8_t bits;
    uint8_t shift;
    uint8_t read_pos;
    bool in_reset;
    bool asleep;
    uint8_t cmd;
    uint32_t nargs;
    uint8_t args[MAX_ARGS];

    uint8_t entry;      /* data entry mode: bit0 X inc, bit1 Y inc, bit2 Y first */
    uint16_t xs, xe, ys, ye, xc, yc;
    uint8_t ctrl1[2];
    uint8_t ctrl2;
    bool gray_lut;
    bool gray_diff;     /* gray LUT steps the shown image by gray_step[code] */
    int8_t gray_step[4];

    uint8_t ram[2][H][WB];
    uint8_t shown[H][WB * 2];   /* last activated image as 2-bit pixels, packed */
    bool redraw;
};

static void ssd1677_set_busy(Ssd1677State *s, uint32_t ms)
{
    qemu_set_irq(s->busy, 1);
    timer_mod(&s->busy_timer, qemu_clock_get_ms(QEMU_CLOCK_VIRTUAL) + ms);
}

static void ssd1677_busy_done(void *opaque)
{
    Ssd1677State *s = opaque;
    qemu_set_irq(s->busy, 0);
}

static void ssd1677_ram_write(Ssd1677State *s, int plane, uint8_t v)
{
    /* X is pixel-addressed: D7 lands at the counter, also when decrementing. */
    if (!(s->entry & 1)) {
        v = revbit8(v);
    }
    if (s->xc < W && s->yc < H) {
        s->ram[plane][s->yc][s->xc / 8] = v;
    }
    /* X counts in pixels, 8 per byte; Y-first mode (bit 2) isn't used by the driver. */
    bool xinc = s->entry & 1, yinc = s->entry & 2;
    bool x_at_end = xinc ? s->xc / 8 >= s->xe / 8 : s->xc / 8 <= s->xe / 8;
    if (!x_at_end) {
        s->xc = xinc ? s->xc + 8 : s->xc - 8;
        return;
    }
    s->xc = s->xs;
    if (s->yc == s->ye) {
        s->yc = s->ys;
    } else {
        s->yc = yinc ? s->yc + 1 : s->yc - 1;
    }
}

/* RAM option from display update control 1: 0 normal, 4 bypass as 0, 8 invert. */
static uint8_t ssd1677_opt(uint8_t opt, uint8_t v)
{
    switch (opt & 0xc) {
    case 0x4: return 0x00;
    case 0x8: return ~v;
    }
    return v;
}

static bool ssd1677_lut_is_gray(const uint8_t *lut, size_t len)
{
    /* The first four 10-byte rows drive WW, BW, WB, BB transitions.
     * Full monochrome waveforms depend only on the new pixel value. */
    if (len < 40) {
        return true;
    }
    if (!memcmp(lut, lut + 20, 10) && !memcmp(lut + 10, lut + 30, 10)) {
        return false;
    }

    /* A partial monochrome waveform holds unchanged pixels and applies
     * opposite drives to BW/WB. Permit one compensation pulse on WW;
     * Picco uses this to counter background drift. Each byte contains
     * four two-bit drive phases (0=hold, 1/2=opposite polarities). */
    unsigned compensation = 0;
    bool transition = false;
    for (unsigned phase = 0; phase < 40; phase++) {
        unsigned shift = 6 - 2 * (phase % 4);
        uint8_t ww = (lut[phase / 4] >> shift) & 3;
        uint8_t bw = (lut[10 + phase / 4] >> shift) & 3;
        uint8_t wb = (lut[20 + phase / 4] >> shift) & 3;
        uint8_t bb = (lut[30 + phase / 4] >> shift) & 3;
        if (bb || ww == 3 || (ww && ++compensation > 1)) {
            return true;
        }
        if (bw || wb) {
            if (!((bw == 1 && wb == 2) || (bw == 2 && wb == 1))) {
                return true;
            }
            transition = true;
        }
    }
    return !transition;
}

/*
 * A grayscale LUT that applies VSS in every phase of some pixel code is
 * differential: those pixels keep the shown image and the driven codes move
 * it instead of setting a level. CrossPoint's anti-aliasing LUT holds code 0,
 * so the black and white of its B/W frame stay, and lightens its gray pixels
 * (black in that frame) by 5 VSL phases for code 3 and 9 for code 2. Without
 * a waveform model, codes are ranked by net phase count per direction and
 * each rank is one gray level: VSL toward white, VSH1/VSH2 toward black.
 * Returns false for absolute LUTs, where every code is driven.
 */
static bool ssd1677_lut_steps(const uint8_t *lut, size_t len, int8_t step[4])
{
    int net[4];
    bool hold = false;

    if (len < 40) {
        return false;
    }
    for (int r = 0; r < 4; r++) {
        bool driven = false;
        net[r] = 0;
        for (unsigned phase = 0; phase < 40; phase++) {
            uint8_t vs = (lut[r * 10 + phase / 4] >> (6 - 2 * (phase % 4))) & 3;
            if (vs) {
                driven = true;
                net[r] += vs == 2 ? 1 : -1;
            }
        }
        hold |= !driven;
    }
    for (int r = 0; r < 4; r++) {
        int rank = 0;
        for (int v = 1; v <= abs(net[r]); v++) {
            for (int o = 0; o < 4; o++) {
                if (net[o] == (net[r] > 0 ? v : -v)) {
                    rank++;
                    break;
                }
            }
        }
        step[r] = net[r] > 0 ? rank : -rank;
    }
    return hold;
}

static void ssd1677_activate(Ssd1677State *s)
{
    uint8_t bw_opt = s->ctrl1[0] & 0xf, red_opt = s->ctrl1[0] >> 4;
    /* Update-control bit4 loads the controller's monochrome OTP LUT. */
    if (s->ctrl2 & 0x10) {
        s->gray_lut = false;
    }
    bool gray = s->gray_lut && (red_opt & 0xc) != 0x4;

    /* Gates are wired in reverse on the X4: RAM row r is screen row H-1-r. */
    for (int y = 0; y < H; y++) {
        int r = H - 1 - y;
        for (int xb = 0; xb < WB; xb++) {
            uint8_t bw = ssd1677_opt(bw_opt, s->ram[PLANE_BW][r][xb]);
            uint8_t red = ssd1677_opt(red_opt, s->ram[PLANE_RED][r][xb]);
            uint16_t old = s->shown[y][xb * 2] << 8 | s->shown[y][xb * 2 + 1];
            uint16_t px = 0;
            for (int b = 7; b >= 0; b--) {
                int lvl;
                if (gray) {
                    lvl = (((red >> b) & 1) << 1) | ((bw >> b) & 1);
                    if (s->gray_diff) {
                        lvl = MIN(MAX(((old >> 2 * b) & 3) + s->gray_step[lvl], 0), 3);
                    }
                } else {
                    lvl = ((bw >> b) & 1) ? 3 : 0;
                }
                px = (px << 2) | lvl;
            }
            s->shown[y][xb * 2] = px >> 8;
            s->shown[y][xb * 2 + 1] = px & 0xff;
        }
    }
    s->redraw = true;
}

static void ssd1677_command_done(Ssd1677State *s)
{
    uint8_t *a = s->args;

    switch (s->cmd) {
    case 0x11:
        if (s->nargs >= 1) s->entry = a[0] & 7;
        break;
    case 0x21:
        if (s->nargs >= 1) s->ctrl1[0] = a[0];
        if (s->nargs >= 2) s->ctrl1[1] = a[1];
        break;
    case 0x22:
        if (s->nargs >= 1) s->ctrl2 = a[0];
        break;
    case 0x44:
        if (s->nargs >= 4) {
            s->xs = (a[0] | a[1] << 8) & 0x3ff;
            s->xe = (a[2] | a[3] << 8) & 0x3ff;
        }
        break;
    case 0x45:
        if (s->nargs >= 4) {
            s->ys = (a[0] | a[1] << 8) & 0x3ff;
            s->ye = (a[2] | a[3] << 8) & 0x3ff;
        }
        break;
    case 0x4E:
        if (s->nargs >= 2) s->xc = (a[0] | a[1] << 8) & 0x3ff;
        break;
    case 0x4F:
        if (s->nargs >= 2) s->yc = (a[0] | a[1] << 8) & 0x3ff;
        break;
    case 0x46:
    case 0x47:
        if (s->nargs >= 1) {
            /* Auto write: bit 7 = fill value, the rest picks a pattern; plain fill is enough. */
            memset(s->ram[s->cmd == 0x46 ? PLANE_BW : PLANE_RED], (a[0] & 0x80) ? 0xff : 0x00,
                   sizeof(s->ram[0]));
            ssd1677_set_busy(s, 5);
        }
        break;
    }
}

static void ssd1677_command_start(Ssd1677State *s, uint8_t c)
{
    if (s->cmd == 0x32 && s->nargs) {
        s->gray_lut = ssd1677_lut_is_gray(s->args, s->nargs);
        s->gray_diff = ssd1677_lut_steps(s->args, s->nargs, s->gray_step);
    }
    s->cmd = c;
    s->nargs = 0;
    s->read_pos = 0;

    switch (c) {
    case 0x12:  /* SWRESET */
        s->entry = 3;
        s->xs = 0; s->xe = W - 1; s->ys = 0; s->ye = H - 1;
        s->xc = 0; s->yc = 0;
        s->ctrl1[0] = s->ctrl1[1] = 0;
        s->ctrl2 = 0;
        s->gray_lut = false;
        ssd1677_set_busy(s, 2);
        break;
    case 0x20:  /* master activation */
        ssd1677_activate(s);
        ssd1677_set_busy(s, s->busy_ms);
        break;
    case 0x32:
        s->gray_lut = true;
        s->gray_diff = false;
        break;
    }
}

static bool ssd1677_reading(Ssd1677State *s)
{
    return s->dc && !s->in_reset && !s->asleep && s->cmd == 0x2e;
}

static uint8_t ssd1677_read_byte(Ssd1677State *s)
{
    /* DEPG0397 controller identity, accepted by the Picco stock driver. */
    static const uint8_t id[] = {
        0xca, 0xfe, 0x00, 0x16, 0x77, 0x00, 0x08, 0x01, 0x00, 0x4e,
    };
    return s->read_pos < sizeof(id) ? id[s->read_pos] : 0xff;
}

static uint32_t ssd1677_transfer(SSIPeripheral *dev, uint32_t data)
{
    Ssd1677State *s = SSD1677(dev);
    uint8_t v = data;

    if (s->in_reset || (s->asleep && !s->dc)) {
        return 0;
    }
    if (!s->dc) {
        ssd1677_command_start(s, v);
        return 0;
    }
    if (ssd1677_reading(s)) {
        uint8_t value = ssd1677_read_byte(s);
        if (s->read_pos < 10) {
            s->read_pos++;
        }
        return value;
    }
    switch (s->cmd) {
    case 0x24:
        ssd1677_ram_write(s, PLANE_BW, v);
        break;
    case 0x26:
        ssd1677_ram_write(s, PLANE_RED, v);
        break;
    case 0x10:
        if (v & 3) s->asleep = true;
        break;
    default:
        if (s->nargs < MAX_ARGS) {
            s->args[s->nargs++] = v;
        }
        /* Commands apply as soon as their argument bytes are in. */
        switch (s->cmd) {
        case 0x11: case 0x22: case 0x46: case 0x47:
            if (s->nargs == 1) ssd1677_command_done(s);
            break;
        case 0x21: case 0x4E: case 0x4F:
            if (s->nargs <= 2) ssd1677_command_done(s);
            break;
        case 0x44: case 0x45:
            if (s->nargs == 4) ssd1677_command_done(s);
            break;
        }
        break;
    }
    return 0;
}

static void ssd1677_present_bit(Ssd1677State *s)
{
    qemu_set_irq(s->sda_out, ssd1677_reading(s) && !s->parent_obj.cs ?
                 (ssd1677_read_byte(s) >> (7 - s->bits)) & 1 : 1);
}

static void ssd1677_set_sclk(void *opaque, int n, int level)
{
    Ssd1677State *s = SSD1677(opaque);
    bool rise = level && !s->sclk;
    bool fall = !level && s->sclk;

    s->sclk = level != 0;
    if (s->parent_obj.cs || s->in_reset || s->asleep) {
        return;
    }
    if (ssd1677_reading(s)) {
        /* Picco samples before the rising edge. Keep the bit stable through
         * that edge, then present the next one on the falling edge. */
        if (fall) {
            if (++s->bits == 8) {
                s->bits = 0;
                if (s->read_pos < 10) {
                    s->read_pos++;
                }
            }
            ssd1677_present_bit(s);
        }
    } else if (rise) {
        s->shift = (s->shift << 1) | s->sda_in;
        if (++s->bits == 8) {
            s->bits = 0;
            ssd1677_transfer(&s->parent_obj, s->shift);
        }
    }
}

static void ssd1677_set_sda(void *opaque, int n, int level)
{
    SSD1677(opaque)->sda_in = level != 0;
}

static void ssd1677_set_dc(void *opaque, int n, int level)
{
    Ssd1677State *s = SSD1677(opaque);
    if (s->dc != (level != 0)) {
        s->bits = 0;
    }
    s->dc = level != 0;
    ssd1677_present_bit(s);
}

static int ssd1677_set_cs(SSIPeripheral *dev, bool level)
{
    Ssd1677State *s = SSD1677(dev);
    s->bits = 0;
    /* SSI updates parent_obj.cs after this callback. */
    qemu_set_irq(s->sda_out, level || !ssd1677_reading(s) ? 1 :
                 (ssd1677_read_byte(s) >> 7) & 1);
    return 0;
}

static void ssd1677_set_rst(void *opaque, int n, int level)
{
    Ssd1677State *s = SSD1677(opaque);
    if (!level) {
        s->in_reset = true;
        timer_del(&s->busy_timer);
        qemu_set_irq(s->busy, 0);
        s->bits = 0;
        s->read_pos = 0;
    } else if (s->in_reset) {
        s->in_reset = false;
        s->asleep = false;
        s->cmd = 0;
        s->nargs = 0;
    }
    ssd1677_present_bit(s);
}

static bool ssd1677_update_display(void *opaque)
{
    Ssd1677State *s = opaque;
    static const uint8_t shade[4] = { 0x10, 0x60, 0xa8, 0xf0 };
    DisplaySurface *surface = qemu_console_surface(s->con);

    if (!s->redraw) {
        return true;
    }
    s->redraw = false;
    uint32_t *d = (uint32_t *)surface_data(surface);
    int stride = surface_stride(surface) / 4;
    for (int y = 0; y < H; y++) {
        for (int x = 0; x < W; x++) {
            uint8_t byte = s->shown[y][x / 4];
            uint8_t g = shade[(byte >> (6 - 2 * (x % 4))) & 3];
            int dx = s->portrait ? H - 1 - y : x, dy = s->portrait ? x : y;
            d[dy * stride + dx] = rgb_to_pixel32(g, g, g);
        }
    }
    qemu_console_update(s->con, 0, 0,
                        surface_width(surface), surface_height(surface));
    return true;
}

static void ssd1677_invalidate(void *opaque)
{
    SSD1677(opaque)->redraw = true;
}

static const GraphicHwOps ssd1677_ops = {
    .invalidate = ssd1677_invalidate,
    .gfx_update = ssd1677_update_display,
};

static void ssd1677_realize(SSIPeripheral *d, Error **errp)
{
    DeviceState *dev = DEVICE(d);
    Ssd1677State *s = SSD1677(d);

    memset(s->ram, 0xff, sizeof(s->ram));
    memset(s->shown, 0xff, sizeof(s->shown));
    s->redraw = true;
    s->con = qemu_graphic_console_create(dev, 0, &ssd1677_ops, s);
    qemu_console_resize(s->con, s->portrait ? H : W, s->portrait ? W : H);
    timer_init_ms(&s->busy_timer, QEMU_CLOCK_VIRTUAL, ssd1677_busy_done, s);
    qdev_init_gpio_in_named(dev, ssd1677_set_dc, "dc", 1);
    qdev_init_gpio_in_named(dev, ssd1677_set_rst, "rst", 1);
    qdev_init_gpio_in_named(dev, ssd1677_set_sclk, "sclk", 1);
    qdev_init_gpio_in_named(dev, ssd1677_set_sda, "sda", 1);
    qdev_init_gpio_out_named(dev, &s->busy, "busy", 1);
    qdev_init_gpio_out_named(dev, &s->sda_out, "sda-out", 1);
}

static const Property ssd1677_properties[] = {
    DEFINE_PROP_UINT32("busy-ms", Ssd1677State, busy_ms, 300),
    DEFINE_PROP_BOOL("portrait", Ssd1677State, portrait, true),
};

static void ssd1677_class_init(ObjectClass *klass, const void *data)
{
    DeviceClass *dc = DEVICE_CLASS(klass);
    SSIPeripheralClass *k = SSI_PERIPHERAL_CLASS(klass);

    k->realize = ssd1677_realize;
    k->transfer = ssd1677_transfer;
    k->set_cs = ssd1677_set_cs;
    k->cs_polarity = SSI_CS_LOW;
    device_class_set_props(dc, ssd1677_properties);
}

static const TypeInfo ssd1677_info = {
    .name = TYPE_SSD1677,
    .parent = TYPE_SSI_PERIPHERAL,
    .instance_size = sizeof(Ssd1677State),
    .class_init = ssd1677_class_init,
};

static void ssd1677_register_types(void)
{
    type_register_static(&ssd1677_info);
}

type_init(ssd1677_register_types)
