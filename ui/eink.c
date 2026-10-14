/*
 * eink display: stream the panel to a front end over one socket.
 *
 * '-display eink,addr=unix:/path' (or host:port) connects to a listening
 * front end, such as the eink-qemu desktop app, and sends the console of the
 * device with qdev id 'panel' (or device=ID) as 8-bit gray. Without device=ID,
 * it falls back to the first graphic console if 'panel' has none. Records
 * are little-endian: u32 type, u32 payload length, payload.
 *
 *   HELLO   "EINK", u32 version, u32 width, u32 height   on connect
 *   RESIZE  u32 width, u32 height                        surface switch
 *   RECT    u32 x, y, w, h, w*h gray8 pixels             dirty rectangle
 *   FLUSH   (empty)                                      end of an update
 *
 * The e-ink panels write R = G = B, so the green channel is sent. At most one
 * update is queued: dirty areas accumulate while the front end reads, and the
 * next update sends their bounding box with the newest pixels, so the guest
 * never waits for the UI. Input goes through QMP input-send-event. Closing
 * the connection shuts QEMU down, like closing a window (window-close=off
 * keeps it running).
 *
 * Copyright (c) 2026 eink-qemu contributors
 * SPDX-License-Identifier: GPL-2.0-or-later
 */

#include "qemu/osdep.h"
#include "qemu/error-report.h"
#include "qemu/module.h"
#include "qemu/sockets.h"
#include "qapi/error.h"
#include "qemu/bswap.h"
#include "io/channel-socket.h"
#include "system/runstate.h"
#include "system/runstate-action.h"
#include "ui/console.h"
#include "ui/qemu-pixman.h"

#define EINK_VERSION 1

enum {
    EINK_HELLO = 1,
    EINK_RESIZE = 2,
    EINK_RECT = 3,
    EINK_FLUSH = 4,
};

typedef struct EinkDisplay {
    DisplayChangeListener dcl;
    DisplaySurface *ds;
    QIOChannelSocket *sioc;
    GByteArray *out;            /* queued bytes, sent from offset 'sent' */
    size_t sent;
    guint out_watch, in_watch;
    bool window_close;
    bool dirty, resized;
    int x0, y0, x1, y1;         /* dirty bounding box, exclusive end */
} EinkDisplay;

static EinkDisplay *eink;

static void eink_put32(GByteArray *b, uint32_t v)
{
    uint32_t le = cpu_to_le32(v);

    g_byte_array_append(b, (const guint8 *)&le, sizeof(le));
}

static void eink_record(EinkDisplay *e, uint32_t type, uint32_t len)
{
    eink_put32(e->out, type);
    eink_put32(e->out, len);
}

static void eink_disconnect(EinkDisplay *e)
{
    if (!e->sioc) {
        return;
    }
    g_clear_handle_id(&e->out_watch, g_source_remove);
    g_clear_handle_id(&e->in_watch, g_source_remove);
    qio_channel_close(QIO_CHANNEL(e->sioc), NULL);
    object_unref(OBJECT(e->sioc));
    e->sioc = NULL;
    g_byte_array_set_size(e->out, 0);
    e->sent = 0;
    if (e->window_close) {
        shutdown_action = SHUTDOWN_ACTION_POWEROFF;
        qemu_system_shutdown_request(SHUTDOWN_CAUSE_HOST_UI);
    }
}

static gboolean eink_writable(QIOChannel *ioc, GIOCondition cond, void *opaque);

/* Send what the socket takes now; wait for G_IO_OUT for the rest. */
static void eink_flush_out(EinkDisplay *e)
{
    while (e->sioc && e->sent < e->out->len) {
        ssize_t n = qio_channel_write(QIO_CHANNEL(e->sioc),
                                      (char *)e->out->data + e->sent,
                                      e->out->len - e->sent, NULL);
        if (n == QIO_CHANNEL_ERR_BLOCK) {
            if (!e->out_watch) {
                e->out_watch = qio_channel_add_watch(QIO_CHANNEL(e->sioc),
                                                     G_IO_OUT, eink_writable,
                                                     e, NULL);
            }
            return;
        }
        if (n < 0) {
            eink_disconnect(e);
            return;
        }
        e->sent += n;
    }
    g_byte_array_set_size(e->out, 0);
    e->sent = 0;
}

static void eink_send_update(EinkDisplay *e)
{
    int w, h;

    if (!e->sioc || e->out->len || !e->ds || !(e->dirty || e->resized)) {
        return;
    }
    if (e->resized) {
        eink_record(e, EINK_RESIZE, 8);
        eink_put32(e->out, surface_width(e->ds));
        eink_put32(e->out, surface_height(e->ds));
        e->resized = false;
    }
    if (e->dirty) {
        g_autoptr(pixman_image_t) line =
            qemu_pixman_linebuf_create(PIXMAN_x8r8g8b8, e->x1 - e->x0);
        const uint32_t *px = (const uint32_t *)pixman_image_get_data(line);
        size_t at;

        w = e->x1 - e->x0;
        h = e->y1 - e->y0;
        eink_record(e, EINK_RECT, 16 + w * h);
        eink_put32(e->out, e->x0);
        eink_put32(e->out, e->y0);
        eink_put32(e->out, w);
        eink_put32(e->out, h);
        at = e->out->len;
        g_byte_array_set_size(e->out, at + w * h);
        for (int y = e->y0; y < e->y1; y++) {
            qemu_pixman_linebuf_fill(line, e->ds->image, w, e->x0, y);
            for (int x = 0; x < w; x++) {
                e->out->data[at++] = px[x] >> 8;
            }
        }
        e->dirty = false;
    }
    eink_record(e, EINK_FLUSH, 0);
    eink_flush_out(e);
}

static gboolean eink_writable(QIOChannel *ioc, GIOCondition cond, void *opaque)
{
    EinkDisplay *e = opaque;

    e->out_watch = 0;
    eink_flush_out(e);
    /* Anything that changed while the socket was full goes out now. */
    eink_send_update(e);
    return G_SOURCE_REMOVE;
}

/* The front end sends nothing; a readable socket means it hung up. */
static gboolean eink_readable(QIOChannel *ioc, GIOCondition cond, void *opaque)
{
    EinkDisplay *e = opaque;
    char buf[64];
    ssize_t n = qio_channel_read(ioc, buf, sizeof(buf), NULL);

    if (n == QIO_CHANNEL_ERR_BLOCK || n > 0) {
        return G_SOURCE_CONTINUE;
    }
    e->in_watch = 0;
    eink_disconnect(e);
    return G_SOURCE_REMOVE;
}

static void eink_mark(EinkDisplay *e, int x, int y, int w, int h)
{
    if (!e->ds) {
        return;
    }
    x = MAX(x, 0);
    y = MAX(y, 0);
    w = MIN(x + w, surface_width(e->ds)) - x;
    h = MIN(y + h, surface_height(e->ds)) - y;
    if (w <= 0 || h <= 0) {
        return;
    }
    if (!e->dirty) {
        e->x0 = x;
        e->y0 = y;
        e->x1 = x + w;
        e->y1 = y + h;
        e->dirty = true;
        return;
    }
    e->x0 = MIN(e->x0, x);
    e->y0 = MIN(e->y0, y);
    e->x1 = MAX(e->x1, x + w);
    e->y1 = MAX(e->y1, y + h);
}

static void eink_refresh(DisplayChangeListener *dcl)
{
    EinkDisplay *e = container_of(dcl, EinkDisplay, dcl);

    qemu_console_hw_update(dcl->con);
    eink_send_update(e);
}

static void eink_gfx_update(DisplayChangeListener *dcl,
                            int x, int y, int w, int h)
{
    eink_mark(container_of(dcl, EinkDisplay, dcl), x, y, w, h);
}

static void eink_gfx_switch(DisplayChangeListener *dcl,
                            DisplaySurface *new_surface)
{
    EinkDisplay *e = container_of(dcl, EinkDisplay, dcl);

    e->ds = new_surface;
    e->dirty = false;
    if (new_surface) {
        e->resized = true;
        eink_mark(e, 0, 0, surface_width(new_surface),
                  surface_height(new_surface));
    }
}

static const DisplayChangeListenerOps eink_ops = {
    .dpy_name       = "eink",
    .dpy_refresh    = eink_refresh,
    .dpy_gfx_update = eink_gfx_update,
    .dpy_gfx_switch = eink_gfx_switch,
};

static void eink_init(DisplayState *ds, DisplayOptions *opts)
{
    g_autoptr(SocketAddress) addr = NULL;
    QemuConsole *con = NULL;
    EinkDisplay *e;

    if (opts->u.eink.device) {
        con = qemu_console_lookup_by_device_name(opts->u.eink.device, 0,
                                                 &error_fatal);
    } else {
        /* Boards may also carry Espressif's RGB framebuffer console. */
        con = qemu_console_lookup_by_device_name("panel", 0, NULL);
        for (int i = 0; !con; i++) {
            QemuConsole *c = qemu_console_lookup_by_index(i);

            if (!c) {
                break;
            }
            if (qemu_console_is_graphic(c)) {
                con = c;
            }
        }
    }
    if (!con) {
        error_report("eink display: the machine has no graphic console");
        exit(1);
    }
    addr = socket_parse(opts->u.eink.addr, &error_fatal);

    e = eink = g_new0(EinkDisplay, 1);
    e->out = g_byte_array_new();
    e->window_close = !opts->has_window_close || opts->window_close;
    e->sioc = qio_channel_socket_new();
    qio_channel_set_name(QIO_CHANNEL(e->sioc), "eink-display");
    if (qio_channel_socket_connect_sync(e->sioc, addr, NULL) < 0) {
        error_report("eink display: cannot connect to %s", opts->u.eink.addr);
        exit(1);
    }
    qio_channel_set_blocking(QIO_CHANNEL(e->sioc), false, NULL);
    e->in_watch = qio_channel_add_watch(QIO_CHANNEL(e->sioc),
                                        G_IO_IN | G_IO_HUP | G_IO_ERR,
                                        eink_readable, e, NULL);

    /* The surface arrives through dpy_gfx_switch at registration. */
    qemu_console_register_listener(con, &e->dcl, &eink_ops);
    eink_record(e, EINK_HELLO, 16);
    g_byte_array_append(e->out, (const guint8 *)"EINK", 4);
    eink_put32(e->out, EINK_VERSION);
    eink_put32(e->out, e->ds ? surface_width(e->ds) : 0);
    eink_put32(e->out, e->ds ? surface_height(e->ds) : 0);
    e->resized = false;
    eink_flush_out(e);
}

static void eink_cleanup(void)
{
    if (!eink) {
        return;
    }
    qemu_console_unregister_listener(&eink->dcl);
    eink->window_close = false;
    eink_disconnect(eink);
    g_byte_array_unref(eink->out);
    g_clear_pointer(&eink, g_free);
}

static QemuDisplay qemu_display_eink = {
    .type    = DISPLAY_TYPE_EINK,
    .init    = eink_init,
    .cleanup = eink_cleanup,
};

static void register_eink(void)
{
    qemu_display_register(&qemu_display_eink);
}

type_init(register_eink);
