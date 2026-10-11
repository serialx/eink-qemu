/*
 * ESP32-S3 GPIO emulation
 *
 * Models OUT/ENABLE (and W1TS/W1TC), IN, per-pin interrupt type/enable and
 * STATUS for GPIO0..48. Pins that are neither driven from outside ("pin-in"
 * lines) nor enabled as outputs read as 1, as if pulled up. Outputs are
 * reported on "pin-out" lines (1 while not enabled). The GPIO matrix and IO_MUX are not modeled:
 * the board wiring connects pins directly.
 *
 * Copyright (c) 2023 Espressif Systems (Shanghai) Co. Ltd.
 *
 * This program is free software; you can redistribute it and/or modify
 * it under the terms of the GNU General Public License version 2 or
 * (at your option) any later version.
 */

#include "qemu/osdep.h"
#include "qemu/log.h"
#include "qapi/error.h"
#include "hw/core/sysbus.h"
#include "hw/core/irq.h"
#include "hw/core/qdev-properties.h"
#include "hw/gpio/esp32s3_gpio.h"

#define R_OUT           0x04
#define R_OUT_W1TS      0x08
#define R_OUT_W1TC      0x0C
#define R_OUT1          0x10
#define R_OUT1_W1TS     0x14
#define R_OUT1_W1TC     0x18
#define R_ENABLE        0x20
#define R_ENABLE_W1TS   0x24
#define R_ENABLE_W1TC   0x28
#define R_ENABLE1       0x2C
#define R_ENABLE1_W1TS  0x30
#define R_ENABLE1_W1TC  0x34
#define R_STRAP         0x38
#define R_IN            0x3C
#define R_IN1           0x40
#define R_STATUS        0x44
#define R_STATUS_W1TS   0x48
#define R_STATUS_W1TC   0x4C
#define R_STATUS1       0x50
#define R_STATUS1_W1TS  0x54
#define R_STATUS1_W1TC  0x58
#define R_PCPU_INT      0x5C
#define R_PCPU_INT1     0x68
#define R_PIN0          0x74
#define R_PIN48         (R_PIN0 + 4 * 48)
#define R_STATUS_NEXT   0x14C
#define R_STATUS_NEXT1  0x150
#define R_DATE          0x6FC

#define PIN_INT_TYPE(v) (((v) >> 7) & 0x7)
#define PIN_INT_ENA(v)  (((v) >> 13) & 0x1f)
#define PIN_WAKEUP_EN   BIT(10)

#define LO32(v) ((uint32_t)(v))
#define HI32(v) ((uint32_t)((v) >> 32))
#define PIN_MASK ((1ULL << ESP32S3_GPIO_COUNT) - 1)

static uint64_t gpio_in(ESP32S3GPIOState *s)
{
    uint64_t own = s->out_reg & s->enable & ~s->ext_driven;
    uint64_t floating = ~s->ext_driven & ~s->enable;
    return ((s->ext_level & s->ext_driven) | own | floating) & PIN_MASK;
}

static uint64_t gpio_int_pins(ESP32S3GPIOState *s)
{
    uint64_t m = 0;
    for (int i = 0; i < ESP32S3_GPIO_COUNT; i++) {
        if (PIN_INT_ENA(s->pin[i]) && PIN_INT_TYPE(s->pin[i])) {
            m |= 1ULL << i;
        }
    }
    return m;
}

/* Latch edges/levels into STATUS and drive the interrupt line. */
static void gpio_update(ESP32S3GPIOState *s)
{
    uint64_t in = gpio_in(s);
    uint64_t rise = in & ~s->last_in, fall = ~in & s->last_in;

    for (int i = 0; i < ESP32S3_GPIO_COUNT; i++) {
        uint64_t b = 1ULL << i;
        switch (PIN_INT_TYPE(s->pin[i])) {
        case 1: if (rise & b) s->status |= b; break;
        case 2: if (fall & b) s->status |= b; break;
        case 3: if ((rise | fall) & b) s->status |= b; break;
        case 4: if (!(in & b)) s->status |= b; break;
        case 5: if (in & b) s->status |= b; break;
        }
    }
    s->last_in = in;

    /* A pin that isn't an output floats high (pull-up), so devices see it released. */
    uint64_t out = (s->out_reg & s->enable) | ~s->enable;
    uint64_t changed = (out ^ s->last_out) & PIN_MASK;
    s->last_out = out;
    for (int i = 0; changed; i++, changed >>= 1) {
        if (changed & 1) {
            qemu_set_irq(s->out[i], (out >> i) & 1);
        }
    }
    qemu_set_irq(s->irq, (s->status & gpio_int_pins(s)) != 0);

    /* Light-sleep GPIO wakeup only supports level triggers (4 low, 5 high). */
    bool wake = false;
    for (int i = 0; i < ESP32S3_GPIO_COUNT && !wake; i++) {
        uint32_t t = PIN_INT_TYPE(s->pin[i]);
        bool level = (in >> i) & 1;
        wake = (s->pin[i] & PIN_WAKEUP_EN) && ((t == 4 && !level) || (t == 5 && level));
    }
    qemu_set_irq(s->wake, wake);
}

static void gpio_set_in(void *opaque, int n, int level)
{
    ESP32S3GPIOState *s = opaque;
    s->ext_driven |= 1ULL << n;
    s->ext_level = (s->ext_level & ~(1ULL << n)) | ((uint64_t)(level != 0) << n);
    gpio_update(s);
}

static uint64_t gpio_read(void *opaque, hwaddr addr, unsigned int size)
{
    ESP32S3GPIOState *s = opaque;

    if (addr >= R_PIN0 && addr <= R_PIN48) {
        return s->pin[(addr - R_PIN0) / 4];
    }
    switch (addr) {
    case R_OUT:            return LO32(s->out_reg);
    case R_OUT1:           return HI32(s->out_reg);
    case R_ENABLE:         return LO32(s->enable);
    case R_ENABLE1:        return HI32(s->enable);
    case R_STRAP:          return s->strap_mode;
    case R_IN:             return LO32(gpio_in(s));
    case R_IN1:            return HI32(gpio_in(s));
    case R_STATUS:         return LO32(s->status);
    case R_STATUS1:        return HI32(s->status);
    case R_PCPU_INT:       return LO32(s->status & gpio_int_pins(s));
    case R_PCPU_INT1:      return HI32(s->status & gpio_int_pins(s));
    case R_STATUS_NEXT:    return LO32(s->status);
    case R_STATUS_NEXT1:   return HI32(s->status);
    case R_DATE:           return 0x1907040;
    }
    return 0;
}

static void set_lo(uint64_t *r, uint32_t v) { *r = (*r & ~0xffffffffULL) | v; }
static void set_hi(uint64_t *r, uint32_t v) { *r = (*r & 0xffffffffULL) | ((uint64_t)v << 32); }

static void gpio_write(void *opaque, hwaddr addr, uint64_t value, unsigned int size)
{
    ESP32S3GPIOState *s = opaque;
    uint64_t v = (uint32_t)value;

    if (addr >= R_PIN0 && addr <= R_PIN48) {
        s->pin[(addr - R_PIN0) / 4] = value;
        gpio_update(s);
        return;
    }
    switch (addr) {
    case R_OUT:           set_lo(&s->out_reg, v); break;
    case R_OUT_W1TS:      s->out_reg |= v; break;
    case R_OUT_W1TC:      s->out_reg &= ~v; break;
    case R_OUT1:          set_hi(&s->out_reg, v); break;
    case R_OUT1_W1TS:     s->out_reg |= v << 32; break;
    case R_OUT1_W1TC:     s->out_reg &= ~(v << 32); break;
    case R_ENABLE:        set_lo(&s->enable, v); break;
    case R_ENABLE_W1TS:   s->enable |= v; break;
    case R_ENABLE_W1TC:   s->enable &= ~v; break;
    case R_ENABLE1:       set_hi(&s->enable, v); break;
    case R_ENABLE1_W1TS:  s->enable |= v << 32; break;
    case R_ENABLE1_W1TC:  s->enable &= ~(v << 32); break;
    case R_STATUS:        set_lo(&s->status, v); break;
    case R_STATUS_W1TS:   s->status |= v; break;
    case R_STATUS_W1TC:   s->status &= ~v; break;
    case R_STATUS1:       set_hi(&s->status, v); break;
    case R_STATUS1_W1TS:  s->status |= v << 32; break;
    case R_STATUS1_W1TC:  s->status &= ~(v << 32); break;
    default:
        return;
    }
    gpio_update(s);
}

static const MemoryRegionOps gpio_ops = {
    .read = gpio_read,
    .write = gpio_write,
    .endianness = DEVICE_LITTLE_ENDIAN,
};

static void esp32s3_gpio_reset_hold(Object *obj, ResetType type)
{
    ESP32S3GPIOState *s = ESP32S3_GPIO(obj);
    s->out_reg = s->enable = s->status = 0;
    memset(s->pin, 0, sizeof(s->pin));
    s->last_in = gpio_in(s);
}

static void esp32s3_gpio_reset_exit(Object *obj, ResetType type)
{
    ESP32S3GPIOState *s = ESP32S3_GPIO(obj);

    /*
     * Board wiring may be connected after the initial input callbacks have
     * cached released outputs. Publish every pull-up after all devices reset.
     */
    s->last_out = 0;
    gpio_update(s);
}

static void esp32s3_gpio_init(Object *obj)
{
    ESP32S3GPIOState *s = ESP32S3_GPIO(obj);
    SysBusDevice *sbd = SYS_BUS_DEVICE(obj);

    memory_region_init_io(&s->iomem, obj, &gpio_ops, s, TYPE_ESP32S3_GPIO, 0x1000);
    sysbus_init_mmio(sbd, &s->iomem);
    sysbus_init_irq(sbd, &s->irq);
    qdev_init_gpio_in_named(DEVICE(obj), gpio_set_in, ESP32S3_GPIO_IN, ESP32S3_GPIO_COUNT);
    qdev_init_gpio_out_named(DEVICE(obj), s->out, ESP32S3_GPIO_OUT, ESP32S3_GPIO_COUNT);
    qdev_init_gpio_out_named(DEVICE(obj), &s->wake, ESP32S3_GPIO_WAKE, 1);
}

static const Property esp32s3_gpio_properties[] = {
    DEFINE_PROP_UINT32("strap_mode", ESP32S3GPIOState, strap_mode, ESP32S3_STRAP_MODE_FLASH_BOOT),
};

static void esp32s3_gpio_class_init(ObjectClass *klass, const void *data)
{
    DeviceClass *dc = DEVICE_CLASS(klass);
    ResettableClass *rc = RESETTABLE_CLASS(klass);

    rc->phases.hold = esp32s3_gpio_reset_hold;
    rc->phases.exit = esp32s3_gpio_reset_exit;
    device_class_set_props(dc, esp32s3_gpio_properties);
}

static const TypeInfo esp32s3_gpio_info = {
    .name = TYPE_ESP32S3_GPIO,
    .parent = TYPE_SYS_BUS_DEVICE,
    .instance_size = sizeof(ESP32S3GPIOState),
    .instance_init = esp32s3_gpio_init,
    .class_init = esp32s3_gpio_class_init,
};

static void esp32s3_gpio_register_types(void)
{
    type_register_static(&esp32s3_gpio_info);
}

type_init(esp32s3_gpio_register_types)
