/*
 * ESP32-S3 general-purpose SPI master (SPI2/SPI3), CPU-driven mode only.
 *
 * Setting CMD.USR shifts MS_DLEN+1 bits out of W0..W15 (byte 0 = W0 bits 7:0)
 * onto the SSI bus at the end of its SPI_CLOCK wire time, stores what comes
 * back in the same buffer, then clears USR and raises TRANS_DONE. A virtual
 * timer adds transaction-overhead-us plus transaction-overhead-ns. Transfers
 * longer than one byte also add buffer-overhead-ns for the buffered PIO path.
 * These effective setup costs default to zero; boards can calibrate them with
 * the guest driver's existing instruction time included in the measurement.
 * CMD.UPDATE self-clears. DMA, address/command/dummy phases and
 * hardware CS are not modeled: the X4 Pro drives CS and DC from GPIO.
 *
 * This program is free software; you can redistribute it and/or modify
 * it under the terms of the GNU General Public License version 2 or
 * (at your option) any later version.
 */
#include "qemu/osdep.h"
#include "qemu/log.h"
#include "qemu/main-loop.h"
#include "qemu/module.h"
#include "qemu/timer.h"
#include "hw/core/qdev-properties.h"
#include "hw/core/sysbus.h"
#include "hw/core/irq.h"
#include "hw/ssi/ssi.h"
#include "trace.h"

#define TYPE_ESP32S3_GPSPI "ssi.esp32s3.gpspi"
OBJECT_DECLARE_SIMPLE_TYPE(Esp32s3GpspiState, ESP32S3_GPSPI)

#define R_CMD           0x00
#define R_CLOCK         0x0C
#define R_MS_DLEN       0x1C
#define R_DMA_INT_ENA   0x34
#define R_DMA_INT_CLR   0x38
#define R_DMA_INT_RAW   0x3C
#define R_DMA_INT_ST    0x40
#define R_DMA_INT_SET   0x44
#define R_W0            0x98
#define R_W15           0xD4
#define R_DATE          0xF0
#define REGS_SIZE       0x100

#define CMD_UPDATE      BIT(23)
#define CMD_USR         BIT(24)
#define INT_TRANS_DONE  BIT(12)

struct Esp32s3GpspiState {
    SysBusDevice parent_obj;
    MemoryRegion iomem;
    qemu_irq irq;
    SSIBus *bus;
    uint32_t regs[REGS_SIZE / 4];
    uint32_t int_raw;
    uint32_t int_ena;
    QEMUTimer *transfer_timer;
    uint32_t transaction_overhead_us;
    uint32_t transaction_overhead_ns;
    uint32_t buffer_overhead_ns;
    bool zero_wire_time;
    uint32_t transfer_bytes;
    uint8_t transfer_buf[64];
};

static void gpspi_update_irq(Esp32s3GpspiState *s)
{
    qemu_set_irq(s->irq, (s->int_raw & s->int_ena) != 0);
}

static void gpspi_transfer(void *opaque)
{
    Esp32s3GpspiState *s = opaque;
    uint8_t *buf = (uint8_t *)&s->regs[R_W0 / 4];

    for (uint32_t i = 0; i < s->transfer_bytes; i++) {
        /* W registers hold bytes little-endian; the host is assumed LE too. */
        buf[i] = ssi_transfer(s->bus, s->transfer_buf[i]);
    }
    qatomic_and(&s->regs[R_CMD / 4], ~(uint32_t)CMD_USR);
    s->int_raw |= INT_TRANS_DONE;
    gpspi_update_irq(s);
}

static void gpspi_start_transfer(Esp32s3GpspiState *s)
{
    uint32_t clock = s->regs[R_CLOCK / 4];
    uint32_t divider = (clock & BIT(31)) ? 1 :
        (((clock >> 18) & 0xf) + 1) * (((clock >> 12) & 0x3f) + 1);
    uint32_t bits = MIN((s->regs[R_MS_DLEN / 4] & 0x3ffff) + 1, 512);
    /* clkcnt_h/l set the duty cycle; clkcnt_n sets the complete period. */
    int64_t duration_ns = DIV_ROUND_UP((uint64_t)bits * divider *
                                      NANOSECONDS_PER_SECOND, 80000000) +
                          (uint64_t)s->transaction_overhead_us * 1000;

    s->transfer_bytes = DIV_ROUND_UP(bits, 8);
    duration_ns += s->transaction_overhead_ns;
    if (s->transfer_bytes > 1) {
        duration_ns += s->buffer_overhead_ns;
    }
    memcpy(s->transfer_buf, &s->regs[R_W0 / 4], s->transfer_bytes);
    if (s->zero_wire_time) {
        /* Explicit non-accurate mode: avoid a timer for each SPI transfer. */
        trace_esp32s3_gpspi_transfer(80000000 / divider, s->transfer_bytes, 0);
        gpspi_transfer(s);
        return;
    }
    trace_esp32s3_gpspi_transfer(80000000 / divider, s->transfer_bytes,
                               duration_ns);
    timer_mod(s->transfer_timer,
              qemu_clock_get_ns(QEMU_CLOCK_VIRTUAL) + duration_ns);
}

static uint64_t gpspi_read(void *opaque, hwaddr addr, unsigned int size)
{
    Esp32s3GpspiState *s = opaque;

    /* CMD polling needs only the register value, not the device's BQL. */
    if (addr == R_CMD) {
        return qatomic_read(&s->regs[R_CMD / 4]);
    }

    BQL_LOCK_GUARD();
    switch (addr) {
    case R_DMA_INT_RAW: return s->int_raw;
    case R_DMA_INT_ST:  return s->int_raw & s->int_ena;
    case R_DMA_INT_ENA: return s->int_ena;
    case R_DATE:        return 0x2101190;
    }
    return s->regs[addr / 4];
}

static void gpspi_write(void *opaque, hwaddr addr, uint64_t value, unsigned int size)
{
    Esp32s3GpspiState *s = opaque;

    BQL_LOCK_GUARD();
    switch (addr) {
    case R_CMD:
        if (qatomic_read(&s->regs[R_CMD / 4]) & CMD_USR) {
            if (value & CMD_USR) {
                qemu_log_mask(LOG_GUEST_ERROR,
                              "esp32s3_gpspi: USR while transfer pending\n");
            }
            break;
        }
        qatomic_set(&s->regs[R_CMD / 4], value & ~CMD_UPDATE);
        if (value & CMD_USR) {
            gpspi_start_transfer(s);
        }
        break;
    case R_DMA_INT_ENA: s->int_ena = value; break;
    case R_DMA_INT_CLR: s->int_raw &= ~value; break;
    case R_DMA_INT_SET: s->int_raw |= value; break;
    default:
        s->regs[addr / 4] = value;
        break;
    }
    gpspi_update_irq(s);
}

static const MemoryRegionOps gpspi_ops = {
    .read = gpspi_read,
    .write = gpspi_write,
    .endianness = DEVICE_LITTLE_ENDIAN,
    .valid.min_access_size = 4,
    .valid.max_access_size = 4,
};

static void gpspi_init(Object *obj)
{
    Esp32s3GpspiState *s = ESP32S3_GPSPI(obj);
    SysBusDevice *sbd = SYS_BUS_DEVICE(obj);

    memory_region_init_io(&s->iomem, obj, &gpspi_ops, s, TYPE_ESP32S3_GPSPI, REGS_SIZE);
    /*
     * Only CMD reads are lock-free; all other accesses take the BQL in
     * their callbacks. This CPU-driven controller does not initiate DMA,
     * so it cannot re-enter guest MMIO and needs no device-wide IO guard.
     */
    memory_region_enable_lockless_io(&s->iomem);
    memory_region_set_readonly_word(&s->iomem, R_CMD, &s->regs[R_CMD / 4]);
    sysbus_init_mmio(sbd, &s->iomem);
    sysbus_init_irq(sbd, &s->irq);
    s->bus = ssi_create_bus(DEVICE(obj), "spi");
    s->transfer_timer = timer_new_ns(QEMU_CLOCK_VIRTUAL, gpspi_transfer, s);
}

static void gpspi_finalize(Object *obj)
{
    Esp32s3GpspiState *s = ESP32S3_GPSPI(obj);

    timer_free(s->transfer_timer);
}

static void gpspi_reset(DeviceState *dev)
{
    Esp32s3GpspiState *s = ESP32S3_GPSPI(dev);

    timer_del(s->transfer_timer);
    qatomic_set(&s->regs[R_CMD / 4], 0);
    memset(&s->regs[1], 0, sizeof(s->regs) - sizeof(s->regs[0]));
    s->int_raw = 0;
    s->int_ena = 0;
    s->transfer_bytes = 0;
    gpspi_update_irq(s);
}

static const Property gpspi_properties[] = {
    DEFINE_PROP_UINT32("transaction-overhead-us", Esp32s3GpspiState,
                       transaction_overhead_us, 0),
    DEFINE_PROP_UINT32("transaction-overhead-ns", Esp32s3GpspiState,
                       transaction_overhead_ns, 0),
    DEFINE_PROP_UINT32("buffer-overhead-ns", Esp32s3GpspiState,
                       buffer_overhead_ns, 0),
    DEFINE_PROP_BOOL("zero-wire-time", Esp32s3GpspiState, zero_wire_time, false),
};

static void gpspi_class_init(ObjectClass *klass, const void *data)
{
    DeviceClass *dc = DEVICE_CLASS(klass);

    device_class_set_props(dc, gpspi_properties);
    device_class_set_legacy_reset(dc, gpspi_reset);
}

static const TypeInfo gpspi_info = {
    .name = TYPE_ESP32S3_GPSPI,
    .parent = TYPE_SYS_BUS_DEVICE,
    .instance_size = sizeof(Esp32s3GpspiState),
    .instance_init = gpspi_init,
    .instance_finalize = gpspi_finalize,
    .class_init = gpspi_class_init,
};

static void gpspi_register_types(void)
{
    type_register_static(&gpspi_info);
}

type_init(gpspi_register_types)
