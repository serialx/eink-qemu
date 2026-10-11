/*
 * Cypress/Parade TMA525C, single-contact TTSP5 HID-over-I2C controller.
 *
 * Models descriptor requests, bootloader launch, system information, scanning,
 * power state and queued touch reports. Input packets start with LE16 length;
 * a two-byte length peek retains the packet, a complete read consumes it.
 * INT is active low until all queued packets have been consumed.
 *
 * Protocol reference: Linux drivers/input/touchscreen/cyttsp5.c (Parade/Cypress).
 * Picco build 2920 confirms reg1 descriptors, reg4 reports, version bytes9/10
 * in sysinfo, and 7-byte touch headers plus 10-byte contact records. Firmware
 * identity defaults to 1.10, the touch firmware bundled with that Picco build.
 * No guest-specific polling or execution-address checks are used. Firmware
 * programming, multitouch and the HID report descriptor are not implemented.
 *
 * QOM touch="X,Y"/"up", tap="X,Y" use native sensor coordinates, 480x800.
 * Host absolute pointer events use the same coordinates. Optional invert-x/y
 * properties adapt the sensor's physical mounting to the display console.
 * host-touch-ms adapts short host clicks to slow guest execution by retaining
 * a press for at least 100 virtual milliseconds (zero disables). This is host
 * input timing adaptation, not a physical controller requirement. Raw QOM
 * touch and I2C reports do not acquire this minimum hold.
 * GPIO inputs power and rst are active high; output int is active low.
 *
 * SPDX-License-Identifier: GPL-2.0-or-later
 */
#include "qemu/osdep.h"
#include "qemu/bswap.h"
#include "qemu/module.h"
#include "qemu/timer.h"
#include "qapi/error.h"
#include "hw/i2c/i2c.h"
#include "hw/core/irq.h"
#include "hw/core/qdev.h"
#include "hw/core/qdev-properties.h"
#include "ui/input.h"

#define TYPE_TMA525C "tma525c"
OBJECT_DECLARE_SIMPLE_TYPE(TMA525CState, TMA525C)

#define TMA525C_MAX_PACKET 264
#define TMA525C_QUEUE_SIZE 8
#define TMA525C_RES_X 480
#define TMA525C_RES_Y 800

struct TMA525CState {
    I2CSlave parent_obj;
    QemuInputHandlerState *input;
    QEMUTimer *tap_timer;
    QEMUTimer *host_timer;
    qemu_irq irq;
    uint8_t packets[TMA525C_QUEUE_SIZE][TMA525C_MAX_PACKET];
    uint16_t lengths[TMA525C_QUEUE_SIZE];
    unsigned head, count, read_pos, write_pos;
    uint8_t write_buf[TMA525C_MAX_PACKET];
    bool receiving, overflow;
    bool power, rst, bootloader, asleep, scanning;
    bool touch, changed, invert_x, invert_y;
    uint16_t x, y;
    uint16_t host_x, host_y;
    bool host_down, host_release_pending;
    uint32_t host_touch_ms;
    int64_t host_deadline;
    uint8_t fw_major, fw_minor;
};

static bool tma525c_running(TMA525CState *s)
{
    return s->power && s->rst;
}

static void tma525c_update_irq(TMA525CState *s)
{
    qemu_set_irq(s->irq, !tma525c_running(s) || !s->count);
}

static bool tma525c_queue(TMA525CState *s, const uint8_t *data, unsigned length)
{
    unsigned slot;

    if (s->count == TMA525C_QUEUE_SIZE) {
        return false;
    }
    slot = (s->head + s->count) % TMA525C_QUEUE_SIZE;
    memcpy(s->packets[slot], data, length);
    s->lengths[slot] = length;
    s->count++;
    tma525c_update_irq(s);
    return true;
}

static void tma525c_report_touch(TMA525CState *s)
{
    uint8_t report[17] = { 0 };
    unsigned length = s->touch ? 17 : 7;

    if (!tma525c_running(s) || s->bootloader || s->asleep ||
        !s->scanning || !s->changed) {
        return;
    }
    stw_le_p(report, length);
    report[2] = 1;
    report[5] = s->touch;
    if (s->touch) {
        /* Finger object, tracking ID 0, one contact. */
        stw_le_p(report + 9, s->x);
        stw_le_p(report + 11, s->y);
        report[13] = 32;
        report[14] = report[15] = 8;
    }
    if (tma525c_queue(s, report, length)) {
        s->changed = false;
    }
}

static void tma525c_set_contact(TMA525CState *s, bool down, uint16_t x, uint16_t y)
{
    s->changed |= s->touch != down || (down && (s->x != x || s->y != y));
    s->touch = down;
    s->x = x;
    s->y = y;
    tma525c_report_touch(s);
}

static uint16_t tma525c_crc16(const uint8_t *data, unsigned size)
{
    uint16_t crc = 0xffff;
    unsigned i;

    while (size--) {
        crc ^= (uint16_t)*data++ << 8;
        for (i = 0; i < 8; i++) {
            crc = (crc << 1) ^ ((crc & 0x8000) ? 0x1021 : 0);
        }
    }
    return crc;
}

static void tma525c_descriptor(TMA525CState *s)
{
    uint8_t desc[32] = { 0 };

    stw_le_p(desc, sizeof(desc));
    desc[2] = s->bootloader ? 0xff : 0xf7;
    stw_le_p(desc + 4, 0x0100); /* HID version */
    stw_le_p(desc + 8, 2); /* report descriptor register; length zero */
    stw_le_p(desc + 10, 3); /* input register */
    stw_le_p(desc + 12, TMA525C_MAX_PACKET);
    stw_le_p(desc + 14, 4); /* output register */
    stw_le_p(desc + 16, TMA525C_MAX_PACKET);
    stw_le_p(desc + 18, 5); /* command register */
    stw_le_p(desc + 20, 6); /* data register */
    stw_le_p(desc + 22, 0x04b4); /* Cypress USB vendor ID */
    stw_le_p(desc + 26, (s->fw_major << 8) | s->fw_minor);
    tma525c_queue(s, desc, sizeof(desc));
}

static void tma525c_app_command(TMA525CState *s, uint8_t cmd,
                                const uint8_t *args, unsigned length)
{
    uint8_t response[51] = { 0 };
    unsigned size = 6;

    response[2] = 0x1f;
    response[4] = cmd;
    if (s->bootloader) {
        response[5] = 1;
    } else {
        switch (cmd) {
        case 0x01: /* start bootloader */
            s->bootloader = true;
            tma525c_queue(s, (uint8_t[]){0, 0}, 2);
            return;
        case 0x02: /* get system information */
            size = sizeof(response);
            response[9] = s->fw_major;
            response[10] = s->fw_minor;
            stw_le_p(response + 39, TMA525C_RES_X);
            stw_le_p(response + 41, TMA525C_RES_Y);
            stw_le_p(response + 43, 255); /* maximum pressure */
            response[50] = 1; /* maximum simultaneous contacts */
            break;
        case 0x03: /* suspend scanning */
            s->scanning = false;
            break;
        case 0x04: /* resume scanning */
            s->scanning = true;
            break;
        case 0x28: /* calibrate IDAC, scan must be suspended */
        case 0x29: /* initialize baselines */
            if (s->scanning || length != 1) {
                response[5] = 1;
            }
            break;
        default:
            response[5] = 1; /* unsupported command, never pretend success */
            break;
        }
    }
    stw_le_p(response, size);
    tma525c_queue(s, response, size);
    tma525c_report_touch(s);
}

static void tma525c_bootloader_command(TMA525CState *s,
                                       const uint8_t *data, unsigned length)
{
    uint8_t response[11] = {11, 0, 0x30, 0, 1, 0x0f, 0, 0, 0, 0, 0x17};

    if (length < 11 || data[4] != 1 || data[length - 1] != 0x17 ||
        lduw_le_p(data + 6) != length - 11 ||
        lduw_le_p(data + length - 3) != tma525c_crc16(data + 4, length - 7)) {
        return;
    }
    if (data[5] == 0x3b && length == 11) { /* launch installed application */
        s->bootloader = false;
        s->scanning = true;
        s->asleep = false;
        tma525c_queue(s, (uint8_t[]){0, 0}, 2);
        return;
    }
    /* Programming is deliberately unsupported; report a bootloader error. */
    stw_le_p(response + 8, tma525c_crc16(response + 4, 4));
    tma525c_queue(s, response, sizeof(response));
}

static void tma525c_process_write(TMA525CState *s)
{
    uint8_t *data = s->write_buf;
    unsigned length = s->write_pos;
    uint16_t reg;

    s->write_pos = 0;
    if (s->overflow || length < 2) {
        return;
    }
    reg = lduw_le_p(data);
    if (reg == 1 && length == 2) {
        tma525c_descriptor(s);
    } else if (reg == 4 && length >= 7 && lduw_le_p(data + 2) == length - 2) {
        if (data[4] == 0x2f) {
            tma525c_app_command(s, data[6] & 0x7f, data + 7, length - 7);
        } else if (data[4] == 0x40) {
            tma525c_bootloader_command(s, data + 2, length - 2);
        }
    } else if (reg == 5 && length == 4 && data[3] == 8 && data[2] <= 1) {
        uint8_t response[] = {5, 0, 0xf0, data[2], 8};

        s->asleep = data[2] != 0;
        tma525c_queue(s, response, sizeof(response));
        tma525c_report_touch(s);
    }
}

static void tma525c_finish_read(TMA525CState *s)
{
    if (s->receiving && s->count && s->read_pos >= s->lengths[s->head]) {
        s->head = (s->head + 1) % TMA525C_QUEUE_SIZE;
        s->count--;
        tma525c_report_touch(s);
    }
    s->receiving = false;
    tma525c_update_irq(s);
}

static int tma525c_event(I2CSlave *i2c, enum i2c_event event)
{
    TMA525CState *s = TMA525C(i2c);

    if ((event == I2C_START_SEND || event == I2C_START_RECV) &&
        !tma525c_running(s)) {
        return -1;
    }
    switch (event) {
    case I2C_START_SEND:
        tma525c_finish_read(s);
        s->write_pos = 0;
        s->overflow = false;
        break;
    case I2C_START_RECV:
        tma525c_finish_read(s);
        tma525c_process_write(s);
        s->read_pos = 0;
        s->receiving = true;
        break;
    case I2C_FINISH:
        if (s->receiving) {
            tma525c_finish_read(s);
        } else {
            tma525c_process_write(s);
        }
        break;
    default:
        break;
    }
    return 0;
}

static int tma525c_send(I2CSlave *i2c, uint8_t byte)
{
    TMA525CState *s = TMA525C(i2c);

    if (s->write_pos == sizeof(s->write_buf)) {
        s->overflow = true;
        return -1;
    }
    s->write_buf[s->write_pos++] = byte;
    return 0;
}

static uint8_t tma525c_recv(I2CSlave *i2c)
{
    TMA525CState *s = TMA525C(i2c);
    unsigned pos = s->read_pos++;

    if (!s->count) {
        return pos == 0 ? 2 : 0; /* TTSP5 empty input report */
    }
    return pos < s->lengths[s->head] ? s->packets[s->head][pos] : 0;
}

static void tma525c_reset_protocol(TMA525CState *s)
{
    s->head = s->count = s->read_pos = s->write_pos = 0;
    s->receiving = s->overflow = false;
    s->bootloader = true;
    s->asleep = false;
    s->scanning = true;
    s->touch = s->changed = false;
    s->host_down = s->host_release_pending = false;
    timer_del(s->tap_timer);
    timer_del(s->host_timer);
    tma525c_queue(s, (uint8_t[]){0, 0}, 2);
}

static void tma525c_wires(TMA525CState *s, bool power, bool rst)
{
    bool running = tma525c_running(s);

    s->power = power;
    s->rst = rst;
    if (!running && tma525c_running(s)) {
        tma525c_reset_protocol(s);
    }
    tma525c_update_irq(s);
}

static void tma525c_power_in(void *opaque, int n, int level)
{
    TMA525CState *s = opaque;

    tma525c_wires(s, level, s->rst);
}

static void tma525c_rst_in(void *opaque, int n, int level)
{
    TMA525CState *s = opaque;

    tma525c_wires(s, s->power, level);
}

static void tma525c_host_release(void *opaque)
{
    TMA525CState *s = opaque;

    s->host_release_pending = false;
    tma525c_set_contact(s, false, s->x, s->y);
}

static void tma525c_input_event(DeviceState *dev, QemuConsole *src,
                               QemuInputEvent *evt)
{
    TMA525CState *s = TMA525C(dev);

    if (evt->type == INPUT_EVENT_KIND_ABS) {
        InputMoveEvent *move = &evt->abs;
        int value;

        if (move->axis == INPUT_AXIS_X) {
            value = qemu_input_scale_axis(move->value, INPUT_EVENT_ABS_MIN,
                                         INPUT_EVENT_ABS_MAX, 0, TMA525C_RES_X - 1);
            s->host_x = s->invert_x ? TMA525C_RES_X - 1 - value : value;
        } else if (move->axis == INPUT_AXIS_Y) {
            value = qemu_input_scale_axis(move->value, INPUT_EVENT_ABS_MIN,
                                         INPUT_EVENT_ABS_MAX, 0, TMA525C_RES_Y - 1);
            s->host_y = s->invert_y ? TMA525C_RES_Y - 1 - value : value;
        }
        /* Idle hover must not move a raw QOM contact or a contact retained
         * after host release. Stage coordinates for the next host press. */
        if (s->host_down) {
            s->changed |= s->touch &&
                          (s->x != s->host_x || s->y != s->host_y);
            s->x = s->host_x;
            s->y = s->host_y;
        }
    } else if (evt->type == INPUT_EVENT_KIND_BTN &&
               evt->btn.button == INPUT_BUTTON_LEFT &&
               s->host_down != evt->btn.down) {
        int64_t now = qemu_clock_get_ms(QEMU_CLOCK_VIRTUAL);

        s->host_down = evt->btn.down;
        if (s->host_down) {
            if (s->host_release_pending) {
                timer_del(s->host_timer);
                tma525c_host_release(s);
            }
            s->host_deadline = now + s->host_touch_ms;
            s->x = s->host_x;
            s->y = s->host_y;
            s->changed |= !s->touch;
            s->touch = true;
        } else if (s->host_touch_ms && now < s->host_deadline) {
            s->host_release_pending = true;
            timer_mod(s->host_timer, s->host_deadline);
        } else {
            s->changed |= s->touch;
            s->touch = false;
        }
    }
}

static void tma525c_input_sync(DeviceState *dev)
{
    tma525c_report_touch(TMA525C(dev));
}

static const QemuInputHandler tma525c_input_handler = {
    .name = "TMA525C touchscreen",
    .mask = INPUT_EVENT_MASK_BTN | INPUT_EVENT_MASK_ABS,
    .event = tma525c_input_event,
    .sync = tma525c_input_sync,
};

static void tma525c_touch(Object *obj, const char *str, Error **errp)
{
    TMA525CState *s = TMA525C(obj);
    unsigned x, y;
    char extra;

    if (!strcmp(str, "up")) {
        timer_del(s->host_timer);
        s->host_down = s->host_release_pending = false;
        timer_del(s->tap_timer);
        tma525c_set_contact(s, false, s->x, s->y);
    } else if (sscanf(str, "%u,%u%c", &x, &y, &extra) == 2 &&
               x < TMA525C_RES_X && y < TMA525C_RES_Y) {
        timer_del(s->host_timer);
        s->host_down = s->host_release_pending = false;
        timer_del(s->tap_timer);
        tma525c_set_contact(s, true, x, y);
    } else {
        error_setg(errp, "expected X,Y with X < 480 and Y < 800, or up");
    }
}

static void tma525c_tap(Object *obj, const char *str, Error **errp)
{
    TMA525CState *s = TMA525C(obj);
    Error *local_err = NULL;

    tma525c_touch(obj, str, &local_err);
    if (local_err) {
        error_propagate(errp, local_err);
    } else if (s->touch) {
        timer_mod(s->tap_timer, qemu_clock_get_ms(QEMU_CLOCK_VIRTUAL) + 100);
    }
}

static void tma525c_tap_end(void *opaque)
{
    TMA525CState *s = opaque;

    tma525c_set_contact(s, false, s->x, s->y);
}

static void tma525c_reset_hold(Object *obj, ResetType type)
{
    tma525c_reset_protocol(TMA525C(obj));
}

static void tma525c_reset_exit(Object *obj, ResetType type)
{
    tma525c_update_irq(TMA525C(obj));
}

static void tma525c_realize(DeviceState *dev, Error **errp)
{
    TMA525CState *s = TMA525C(dev);

    s->tap_timer = timer_new_ms(QEMU_CLOCK_VIRTUAL, tma525c_tap_end, s);
    s->host_timer = timer_new_ms(QEMU_CLOCK_VIRTUAL, tma525c_host_release, s);
    s->input = qemu_input_handler_register(dev, &tma525c_input_handler);
    qemu_input_handler_activate(s->input);
}

static void tma525c_unrealize(DeviceState *dev)
{
    TMA525CState *s = TMA525C(dev);

    qemu_input_handler_unregister(s->input);
    timer_del(s->host_timer);
    timer_free(s->host_timer);
    timer_del(s->tap_timer);
    timer_free(s->tap_timer);
}

static void tma525c_init(Object *obj)
{
    TMA525CState *s = TMA525C(obj);

    s->power = s->rst = true;
    qdev_init_gpio_out_named(DEVICE(obj), &s->irq, "int", 1);
    qdev_init_gpio_in_named(DEVICE(obj), tma525c_power_in, "power", 1);
    qdev_init_gpio_in_named(DEVICE(obj), tma525c_rst_in, "rst", 1);
}

static const Property tma525c_properties[] = {
    DEFINE_PROP_UINT8("fw-major", TMA525CState, fw_major, 1),
    DEFINE_PROP_UINT8("fw-minor", TMA525CState, fw_minor, 10),
    DEFINE_PROP_BOOL("invert-x", TMA525CState, invert_x, false),
    DEFINE_PROP_BOOL("invert-y", TMA525CState, invert_y, false),
    DEFINE_PROP_UINT32("host-touch-ms", TMA525CState, host_touch_ms, 100),
};

static void tma525c_class_init(ObjectClass *klass, const void *data)
{
    DeviceClass *dc = DEVICE_CLASS(klass);
    I2CSlaveClass *sc = I2C_SLAVE_CLASS(klass);
    ResettableClass *rc = RESETTABLE_CLASS(klass);

    dc->realize = tma525c_realize;
    dc->unrealize = tma525c_unrealize;
    device_class_set_props(dc, tma525c_properties);
    sc->event = tma525c_event;
    sc->send = tma525c_send;
    sc->recv = tma525c_recv;
    rc->phases.hold = tma525c_reset_hold;
    rc->phases.exit = tma525c_reset_exit;
    object_class_property_add_str(klass, "touch", NULL, tma525c_touch);
    object_class_property_add_str(klass, "tap", NULL, tma525c_tap);
}

static const TypeInfo tma525c_info = {
    .name = TYPE_TMA525C,
    .parent = TYPE_I2C_SLAVE,
    .instance_size = sizeof(TMA525CState),
    .instance_init = tma525c_init,
    .class_init = tma525c_class_init,
};

static void tma525c_register_types(void)
{
    type_register_static(&tma525c_info);
}

type_init(tma525c_register_types)
