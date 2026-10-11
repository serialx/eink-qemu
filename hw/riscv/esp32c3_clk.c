/*
 * ESP32-C3 CPU Clock and Reset emulation
 *
 * Copyright (c) 2023 Espressif Systems (Shanghai) Co. Ltd.
 *
 * This program is free software; you can redistribute it and/or modify
 * it under the terms of the GNU General Public License version 2 or
 * (at your option) any later version.
 */
#include "qemu/osdep.h"
#include "qemu/log.h"
#include "qemu/module.h"
#include "qemu/timer.h"
#include "qapi/error.h"
#include "qemu/error-report.h"
#include "hw/core/sysbus.h"
#include "hw/core/irq.h"
#include "hw/core/qdev-properties.h"
#include "hw/riscv/esp32c3_clk.h"
#include "hw/riscv/esp32c3_clk_defs.h"
#include "exec/cpu-common.h"
#include "system/physmem.h"
#include "exec/tb-flush.h"
#include "cpu.h"
#include "trace.h"
#include <zlib.h>

#define ESP32C3_RTC_FASTMEM_BASE 0x50000000
#define ESP32C3_RTC_FASTMEM_SIZE 0x2000

/*
 * eink-qemu: the RTC_MEM_CRC engine: START sums RTC fast memory from word
 * ADDR, LEN + 1 words, into RTC_FASTMEM_CRC and raises FINISH at once. The
 * deep-sleep entry and the ROM's wake-stub check both use it, so only its
 * consistency matters, not the silicon's polynomial.
 */
static void esp32c3_rtc_mem_crc(ESP32C3ClockState *s, uint32_t value)
{
    uint32_t addr = FIELD_EX32(value, SYSTEM_RTC_FASTMEM_CONFIG, RTC_MEM_CRC_ADDR) * 4;
    uint32_t len = (FIELD_EX32(value, SYSTEM_RTC_FASTMEM_CONFIG, RTC_MEM_CRC_LEN) + 1) * 4;
    uint8_t buf[ESP32C3_RTC_FASTMEM_SIZE];

    if (addr >= ESP32C3_RTC_FASTMEM_SIZE) {
        return;
    }
    len = MIN(len, ESP32C3_RTC_FASTMEM_SIZE - addr);
    physical_memory_read(ESP32C3_RTC_FASTMEM_BASE + addr, buf, len);
    s->rtc_fastmem_crc = crc32(0, buf, len);
}


#define CLOCK_DEBUG      0
#define CLOCK_WARNING    0

/*
 * SYSTEM clock mux: PLL 80/160 MHz or XTAL/RC divided by PRE_DIV_CNT+1.
 * Cost scaling is relative to 160 MHz. MMIO writes end an icount TB; flush
 * translations and force dispatcher re-entry before executing at the new rate.
 */
static void esp32c3_clock_update(ESP32C3ClockState *s)
{
    unsigned source = FIELD_EX32(s->sysclk, SYSTEM_SYSCLK_CONF, SOC_CLK_SEL);
    unsigned divider =
        FIELD_EX32(s->sysclk, SYSTEM_SYSCLK_CONF, PRE_DIV_CNT) + 1;
    uint32_t hz;

    switch (source) {
    case ESP32C3_CLK_SEL_PLL:
        hz = FIELD_EX32(s->cpuperconf, SYSTEM_CPU_PER_CONF, CPUPERIOD_SEL) ==
             ESP32C3_PERIOD_SEL_160 ? 160000000 : 80000000;
        break;
    case ESP32C3_CLK_SEL_XTAL:
        hz = 40000000 / divider;
        break;
    case ESP32C3_CLK_SEL_RCFAST:
        hz = 17500000 / divider;
        break;
    default:
        return; /* Reserved mux selection: retain the previous rate. */
    }
    if (hz != s->cpu_hz) {
        unsigned scale = DIV_ROUND_UP(160000000, hz);
        s->cpu_hz = hz;
        trace_esp32c3_cpu_clock(hz, scale, s->sysclk, s->cpuperconf);
        if (s->cpu) {
            RISCVCPU *cpu = RISCV_CPU(s->cpu);
            if (cpu->cost_clock_scale != scale) {
                cpu->cost_clock_scale = scale;
                /* Reset can update clocks from the main thread. */
                queue_tb_flush(s->cpu);
                cpu_interrupt(s->cpu, CPU_INTERRUPT_EXITTB);
            }
        }
    }
}

static uint32_t esp32c3_read_cpu_intr(ESP32C3ClockState *s, uint32_t index)
{
    return (s->levels >> index) & 1;
}


static void esp32c3_write_cpu_intr(ESP32C3ClockState *s, uint32_t index, uint32_t value)
{
    const uint32_t field = FIELD_EX32(value, SYSTEM_CPU_INTR_FROM_CPU_0, CPU_INTR_FROM_CPU_0);
    if (field) {
        s->levels |= BIT(index);
        qemu_set_irq(s->irqs[index], 1);
    } else {
        s->levels &= ~BIT(index);
        qemu_set_irq(s->irqs[index], 0);
    }
}

static uint32_t esp32c3_clock_get_ext_dev_enc_dec_ctrl(ESP32C3ClockState *s)
{
    return s->sys_ext_dev_enc_dec_ctrl;
}

static uint64_t esp32c3_clock_read(void *opaque, hwaddr addr, unsigned int size)
{
    ESP32C3ClockState *s = ESP32C3_CLOCK(opaque);
    uint64_t r = 0;

    switch(addr) {
        case A_SYSTEM_CPU_PER_CONF:
            r = s->cpuperconf;
            break;
        case A_SYSTEM_SYSCLK_CONF:
            r = s->sysclk;
            break;
        case A_SYSTEM_CPU_INTR_FROM_CPU_0:
        case A_SYSTEM_CPU_INTR_FROM_CPU_1:
        case A_SYSTEM_CPU_INTR_FROM_CPU_2:
        case A_SYSTEM_CPU_INTR_FROM_CPU_3:
            r = esp32c3_read_cpu_intr(s, (addr - A_SYSTEM_CPU_INTR_FROM_CPU_0) / sizeof(uint32_t));
            break;
        case A_SYSTEM_EXTERNAL_DEVICE_ENCRYPT_DECRYPT_CONTROL:
            r = s->sys_ext_dev_enc_dec_ctrl;
            break;
        case A_SYSTEM_RTC_FASTMEM_CONFIG:
            r = s->rtc_fastmem_config;
            break;
        case A_SYSTEM_RTC_FASTMEM_CRC:
            r = s->rtc_fastmem_crc;
            break;
        default:
#if CLOCK_WARNING
            warn_report("[CLOCK] Unsupported read from %08lx\n", addr);
#endif
            break;
    }
    return r;
}

static void esp32c3_clock_write(void *opaque, hwaddr addr, uint64_t value,
                                unsigned int size)
{
    ESP32C3ClockState *s = ESP32C3_CLOCK(opaque);

    switch(addr) {
    case A_SYSTEM_CPU_PER_CONF:
        s->cpuperconf = value;
        esp32c3_clock_update(s);
        break;
    case A_SYSTEM_SYSCLK_CONF:
        s->sysclk = value;
        esp32c3_clock_update(s);
        break;
    case A_SYSTEM_CPU_INTR_FROM_CPU_0:
    case A_SYSTEM_CPU_INTR_FROM_CPU_1:
    case A_SYSTEM_CPU_INTR_FROM_CPU_2:
    case A_SYSTEM_CPU_INTR_FROM_CPU_3:
        esp32c3_write_cpu_intr(s,
            (addr - A_SYSTEM_CPU_INTR_FROM_CPU_0) / sizeof(uint32_t), value);
        break;
    case A_SYSTEM_EXTERNAL_DEVICE_ENCRYPT_DECRYPT_CONTROL:
        s->sys_ext_dev_enc_dec_ctrl = value;
        break;
    case A_SYSTEM_RTC_FASTMEM_CONFIG:
        s->rtc_fastmem_config = value &
            ~R_SYSTEM_RTC_FASTMEM_CONFIG_RTC_MEM_CRC_FINISH_MASK;
        if (value & R_SYSTEM_RTC_FASTMEM_CONFIG_RTC_MEM_CRC_START_MASK) {
            esp32c3_rtc_mem_crc(s, value);
            s->rtc_fastmem_config |=
                R_SYSTEM_RTC_FASTMEM_CONFIG_RTC_MEM_CRC_FINISH_MASK;
        }
        break;
    default:
#if CLOCK_WARNING
        warn_report("[CLOCK] Unsupported write to %08lx (%08lx)",
                    addr, value);
#endif
        break;
    }
}

static const MemoryRegionOps esp32c3_clock_ops = {
    .read =  esp32c3_clock_read,
    .write = esp32c3_clock_write,
    .endianness = DEVICE_LITTLE_ENDIAN,
};

static void esp32c3_clock_reset_hold(Object *obj, ResetType type)
{
    ESP32C3ClockState *s = ESP32C3_CLOCK(obj);
    /* On board reset, set the proper clocks and dividers */
    s->sysclk = ( 1 << R_SYSTEM_SYSCLK_CONF_PRE_DIV_CNT_SHIFT) |
                (ESP32C3_CLK_SEL_PLL << R_SYSTEM_SYSCLK_CONF_SOC_CLK_SEL_SHIFT) |
                (40 << R_SYSTEM_SYSCLK_CONF_CLK_XTAL_FREQ_SHIFT) |
                ( 1 << R_SYSTEM_SYSCLK_CONF_CLK_DIV_EN_SHIFT);

    /* Divider for PLL clock and APB  frequency */
    s->cpuperconf = (ESP32C3_PERIOD_SEL_80 << R_SYSTEM_CPU_PER_CONF_CPUPERIOD_SEL_SHIFT) |
                    (ESP32C3_FREQ_SEL_PLL_480 << R_SYSTEM_CPU_PER_CONF_PLL_FREQ_SEL_SHIFT);

    esp32c3_clock_update(s);

    /* Initialize the IRQs */
    s->levels = 0;
    for (int i = 0 ; i < ESP32C3_SYSTEM_CPU_INTR_COUNT; i++) {
        qemu_irq_lower(s->irqs[i]);
    }
}

static void esp32c3_clock_realize(DeviceState *dev, Error **errp)
{
    /* Initialize the registers */
    esp32c3_clock_reset_hold(OBJECT(dev), RESET_TYPE_COLD);
}

static void esp32c3_clock_init(Object *obj)
{
    ESP32C3ClockState *s = ESP32C3_CLOCK(obj);
    SysBusDevice *sbd = SYS_BUS_DEVICE(obj);

    memory_region_init_io(&s->iomem, obj, &esp32c3_clock_ops, s,
                          TYPE_ESP32C3_CLOCK, A_SYSTEM_COMB_PVT_ERR_HVT_SITE3 + sizeof(uint32_t));
    sysbus_init_mmio(sbd, &s->iomem);

    /* Initialize the output IRQ lines used to manually trigger interrupts */
    for (uint64_t i = 0; i < ESP32C3_SYSTEM_CPU_INTR_COUNT; i++) {
        sysbus_init_irq(sbd, &s->irqs[i]);
    }
}

static const Property esp32c3_clock_properties[] = {
    DEFINE_PROP_LINK("cpu", ESP32C3ClockState, cpu, TYPE_CPU, CPUState *),
};

static void esp32c3_clock_class_init(ObjectClass *klass, const void *data)
{
    DeviceClass *dc = DEVICE_CLASS(klass);
    ESP32C3ClockClass* esp32c3_clock = ESP32C3_CLOCK_CLASS(klass);
    ResettableClass *rc = RESETTABLE_CLASS(klass);

    rc->phases.hold = esp32c3_clock_reset_hold;
    dc->realize = esp32c3_clock_realize;
    device_class_set_props(dc, esp32c3_clock_properties);

    esp32c3_clock->get_ext_dev_enc_dec_ctrl = esp32c3_clock_get_ext_dev_enc_dec_ctrl;
}

static const TypeInfo esp32c3_cache_info = {
    .name = TYPE_ESP32C3_CLOCK,
    .parent = TYPE_SYS_BUS_DEVICE,
    .instance_size = sizeof(ESP32C3ClockState),
    .instance_init = esp32c3_clock_init,
    .class_init = esp32c3_clock_class_init,
    .class_size = sizeof(ESP32C3ClockClass)
};

static void esp32c3_cache_register_types(void)
{
    type_register_static(&esp32c3_cache_info);
}

type_init(esp32c3_cache_register_types)
