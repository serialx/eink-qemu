/*
 * SGM41562B I2C charger: configuration and status registers.
 * Models a battery on USB power; analog charging and watchdog timing are not
 * simulated. Register layout: SGM41562A/B datasheet, January 2022, pp.20-25.
 * SPDX-License-Identifier: GPL-2.0-or-later
 */
#include "qemu/osdep.h"
#include "qemu/module.h"
#include "hw/i2c/i2c.h"
#include "hw/core/qdev-properties.h"

#define TYPE_SGM41562 "sgm41562"
OBJECT_DECLARE_SIMPLE_TYPE(SGM41562State, SGM41562)
struct SGM41562State {
    I2CSlave parent_obj;
    uint8_t regs[14], ptr;
    bool ptr_next, usb_present;
};

static void sgm41562_defaults(SGM41562State *s)
{
    static const uint8_t defaults[14] = {
        0x9f, 0xac, 0x0f, 0x91, 0xa3, 0x7a, 0xc0, 0x37,
        0, 0, 0x60, 0, 0, 0
    };
    memcpy(s->regs, defaults, sizeof(defaults));
}

static int sgm41562_event(I2CSlave *i2c, enum i2c_event event)
{
    if (event == I2C_START_SEND) {
        SGM41562(i2c)->ptr_next = true;
    }
    return 0;
}

static uint8_t sgm41562_recv(I2CSlave *i2c)
{
    SGM41562State *s = SGM41562(i2c);
    uint8_t reg = s->ptr++;
    if (reg == 8) {
        /* USB power good, battery charging unless CHG_DIS is set. */
        return (s->regs[8] & 0x60) | (s->usb_present ?
               2 | ((s->regs[1] & 8) ? 0 : 0x10) : 0);
    }
    return reg < sizeof(s->regs) ? s->regs[reg] : 0;
}

static int sgm41562_send(I2CSlave *i2c, uint8_t value)
{
    SGM41562State *s = SGM41562(i2c);
    if (s->ptr_next) {
        s->ptr_next = false;
        s->ptr = value;
    } else {
        uint8_t reg = s->ptr++;
        if (reg == 2 && (value & 0x80)) {
            sgm41562_defaults(s);
        } else if (reg < sizeof(s->regs) && reg != 0x0b) {
            if (reg == 2) value &= ~0x40; /* self-clearing watchdog reset */
            if (reg == 8) value &= 0x60;
            if (reg == 9) value &= 0xc0;
            if (reg == 0x0a) value = (value & 0x0f) | 0x60;
            s->regs[reg] = value;
        }
    }
    return 0;
}

static void sgm41562_init(Object *obj)
{
    sgm41562_defaults(SGM41562(obj));
}

static const Property sgm41562_props[] = {
    DEFINE_PROP_BOOL("usb-present", SGM41562State, usb_present, true),
};

static void sgm41562_class_init(ObjectClass *klass, const void *data)
{
    I2CSlaveClass *sc = I2C_SLAVE_CLASS(klass);
    sc->event = sgm41562_event;
    sc->recv = sgm41562_recv;
    sc->send = sgm41562_send;
    device_class_set_props(DEVICE_CLASS(klass), sgm41562_props);
}

static const TypeInfo sgm41562_info = {
    .name = TYPE_SGM41562,
    .parent = TYPE_I2C_SLAVE,
    .instance_size = sizeof(SGM41562State),
    .instance_init = sgm41562_init,
    .class_init = sgm41562_class_init,
};
static void sgm41562_register_types(void)
{
    type_register_static(&sgm41562_info);
}
type_init(sgm41562_register_types)
