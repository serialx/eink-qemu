/*
 * Xteink X3 keys: Back, Confirm, Left, Right on a resistor ladder into
 * GPIO1 (ADC1 channel 1), Up and Down on a ladder into GPIO2 (ADC1 channel
 * 2), Power on GPIO3 (active low, pull-up). The "adc" outputs carry the raw
 * 12-bit count the firmware's thresholds expect (idle 4095); "power" is the
 * GPIO3 level (two copies: for the GPIO model and the RTC wake pad). Driven from the host keyboard (arrows, Enter = Confirm,
 * Backspace/Escape = Back, P = Power) and from QOM bool properties for
 * scripted presses:
 *   qom-set /machine/x3-keys down true
 * "power-boot-ms" holds Power pressed that long when the RTC controller
 * signals a cold power-on ("boot" line 0), or a wake from deep sleep by a pad
 * ("boot" line 1) while Power is pressed or was pressed in the last 2 s:
 * firmware re-sleeps a power-button boot whose button isn't still held.
 * Software resets, watchdog resets and timer wakes boot with Power released.
 *
 * This program is free software; you can redistribute it and/or modify
 * it under the terms of the GNU General Public License version 2 or
 * (at your option) any later version.
 */
#include "qemu/osdep.h"
#include "qapi/error.h"
#include "qemu/module.h"
#include "qemu/timer.h"
#include "hw/core/sysbus.h"
#include "hw/core/irq.h"
#include "hw/core/qdev-properties.h"
#include "ui/input.h"

#define TYPE_X3_KEYS "x3-keys"
OBJECT_DECLARE_SIMPLE_TYPE(X3KeysState, X3_KEYS)

enum { KEY_BACK, KEY_CONFIRM, KEY_LEFT, KEY_RIGHT, KEY_UP, KEY_DOWN, KEY_POWER, KEY_COUNT };

#define ADC_IDLE 4095
/* Counts from freeink-sdk InputManager's device measurements. Use its measured
 * Back sample 3597 rather than the across-device average 3512: with the virtual
 * C3's default eFuse calibration these give 2419 and 2366 mV respectively.
 * Only the former is within Back's calibrated 2.4--2.7 V band as well as its
 * raw-count band. Other keys keep the measured averages. */
static const uint32_t key_raw[KEY_COUNT] = { 3597, 2694, 1493, 5, 2242, 5, 0 };

struct X3KeysState {
    SysBusDevice parent_obj;
    QemuInputHandlerState *input;
    qemu_irq adc[2];            /* GPIO1, GPIO2 raw count */
    qemu_irq power[2];          /* GPIO3 level: 1 released, 0 pressed (to the GPIO and RTC pads) */
    QEMUTimer boot_timer;
    uint32_t power_boot_ms;
    bool pressed[KEY_COUNT];
    bool boot_hold;
    int64_t power_down_ms;      /* virtual ms Power was last pressed, or -1 */
};

/* A pad wake within this long of a Power press was that press. */
#define POWER_WAKE_WINDOW_MS 2000

static void keys_update(X3KeysState *s)
{
    uint32_t raw1 = ADC_IDLE, raw2 = ADC_IDLE;

    for (int k = KEY_BACK; k <= KEY_RIGHT; k++) {
        if (s->pressed[k]) {
            raw1 = key_raw[k];
        }
    }
    for (int k = KEY_UP; k <= KEY_DOWN; k++) {
        if (s->pressed[k]) {
            raw2 = key_raw[k];
        }
    }
    qemu_set_irq(s->adc[0], raw1);
    qemu_set_irq(s->adc[1], raw2);
    for (int i = 0; i < 2; i++) {
        qemu_set_irq(s->power[i], !(s->pressed[KEY_POWER] || s->boot_hold));
    }
}

static void keys_set(X3KeysState *s, int k, bool down)
{
    if (k == KEY_POWER && down && !s->pressed[k]) {
        s->power_down_ms = qemu_clock_get_ms(QEMU_CLOCK_VIRTUAL);
    }
    s->pressed[k] = down;
    keys_update(s);
}

static void keys_event(DeviceState *dev, QemuConsole *src, QemuInputEvent *evt)
{
    X3KeysState *s = X3_KEYS(dev);
    QemuInputKeyEvent *key = &evt->key;
    int k;

    switch (qemu_input_linux_to_qcode(key->key)) {
    case Q_KEY_CODE_UP:        k = KEY_UP; break;
    case Q_KEY_CODE_DOWN:      k = KEY_DOWN; break;
    case Q_KEY_CODE_LEFT:      k = KEY_LEFT; break;
    case Q_KEY_CODE_RIGHT:     k = KEY_RIGHT; break;
    case Q_KEY_CODE_RET:
    case Q_KEY_CODE_KP_ENTER:  k = KEY_CONFIRM; break;
    case Q_KEY_CODE_BACKSPACE:
    case Q_KEY_CODE_ESC:       k = KEY_BACK; break;
    case Q_KEY_CODE_P:         k = KEY_POWER; break;
    default:                   return;
    }
    keys_set(s, k, key->down);
}

static const QemuInputHandler keys_handler = {
    .name = "X3 keys",
    .mask = INPUT_EVENT_MASK_KEY,
    .event = keys_event,
};

#define KEY_PROP(name, idx)                                                    \
    static bool get_##name(Object *o, Error **e) { return X3_KEYS(o)->pressed[idx]; } \
    static void set_##name(Object *o, bool v, Error **e) { keys_set(X3_KEYS(o), idx, v); }
KEY_PROP(back, KEY_BACK)
KEY_PROP(confirm, KEY_CONFIRM)
KEY_PROP(left, KEY_LEFT)
KEY_PROP(right, KEY_RIGHT)
KEY_PROP(up, KEY_UP)
KEY_PROP(down, KEY_DOWN)
KEY_PROP(power, KEY_POWER)

static void keys_boot_release(void *opaque)
{
    X3KeysState *s = opaque;
    s->boot_hold = false;
    keys_update(s);
}

/* "boot" line 0: cold power-on; line 1: woken from deep sleep by a pad. */
static void keys_boot(void *opaque, int n, int level)
{
    X3KeysState *s = opaque;
    int64_t now = qemu_clock_get_ms(QEMU_CLOCK_VIRTUAL);

    if (!level || !s->power_boot_ms) {
        return;
    }
    if (n == 1 && !s->pressed[KEY_POWER] &&
        (s->power_down_ms < 0 || now - s->power_down_ms > POWER_WAKE_WINDOW_MS)) {
        return;
    }
    s->boot_hold = true;
    timer_mod(&s->boot_timer, now + s->power_boot_ms);
    keys_update(s);
}

/*
 * Every reset ends the previous boot's synthetic Power hold (the keys
 * themselves stay as they are); the RTC's "boot" line starts a new one
 * for a cold power-on or a Power-pad wake. This runs in the enter phase,
 * before any hold phase, so the S3 SoC's reset (a hold phase) and the
 * C3 RTC's (exit phase) can signal the new boot after it.
 */
static void keys_reset_enter(Object *obj, ResetType type)
{
    X3KeysState *s = X3_KEYS(obj);

    timer_del(&s->boot_timer);
    s->boot_hold = false;
}

static void keys_reset_hold(Object *obj, ResetType type)
{
    /* Physical keys keep their state across a reset; republish the levels. */
    keys_update(X3_KEYS(obj));
}

static void keys_realize(DeviceState *dev, Error **errp)
{
    X3KeysState *s = X3_KEYS(dev);

    timer_init_ms(&s->boot_timer, QEMU_CLOCK_VIRTUAL, keys_boot_release, s);
    s->input = qemu_input_handler_register(dev, &keys_handler);
}

static void keys_unrealize(DeviceState *dev)
{
    X3KeysState *s = X3_KEYS(dev);

    qemu_input_handler_unregister(s->input);
    timer_del(&s->boot_timer);
}

static void keys_init(Object *obj)
{
    X3KeysState *s = X3_KEYS(obj);

    qdev_init_gpio_out_named(DEVICE(obj), s->adc, "adc", 2);
    qdev_init_gpio_out_named(DEVICE(obj), s->power, "power", 2);
    qdev_init_gpio_in_named(DEVICE(obj), keys_boot, "boot", 2);
    s->power_down_ms = -1;
    object_property_add_bool(obj, "back", get_back, set_back);
    object_property_add_bool(obj, "confirm", get_confirm, set_confirm);
    object_property_add_bool(obj, "left", get_left, set_left);
    object_property_add_bool(obj, "right", get_right, set_right);
    object_property_add_bool(obj, "up", get_up, set_up);
    object_property_add_bool(obj, "down", get_down, set_down);
    object_property_add_bool(obj, "power", get_power, set_power);
}

static const Property keys_properties[] = {
    DEFINE_PROP_UINT32("power-boot-ms", X3KeysState, power_boot_ms, 1500),
};

static void keys_class_init(ObjectClass *klass, const void *data)
{
    DeviceClass *dc = DEVICE_CLASS(klass);
    ResettableClass *rc = RESETTABLE_CLASS(klass);

    dc->realize = keys_realize;
    dc->unrealize = keys_unrealize;
    rc->phases.enter = keys_reset_enter;
    rc->phases.hold = keys_reset_hold;
    device_class_set_props(dc, keys_properties);
}

static const TypeInfo keys_info = {
    .name = TYPE_X3_KEYS,
    .parent = TYPE_SYS_BUS_DEVICE,
    .instance_size = sizeof(X3KeysState),
    .instance_init = keys_init,
    .class_init = keys_class_init,
};

static void keys_register_types(void)
{
    type_register_static(&keys_info);
}

type_init(keys_register_types)
