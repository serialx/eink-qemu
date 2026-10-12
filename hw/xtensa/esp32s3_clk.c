/*
 * ESP32-S3 Clocks definition
 *
 * Copyright (c) 2024 Espressif Systems (Shanghai) Co. Ltd.
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
#include "hw/xtensa/esp32s3_clk.h"
#include "hw/xtensa/esp32s3_clk_defs.h"
#include "cpu.h"
#include "exec/tb-flush.h"
#include "system/cpu-timers.h"

#define CLOCK_DEBUG      0
#define CLOCK_WARNING    0

void esp32s3_clock_set_cpu_stall(ESP32S3ClockState *s, unsigned core, bool stalled)
{
    s->rtc_stalled[core] = stalled;
    if (s->cpus[core]) {
        bool gated = core == 1 &&
                     ((s->core1_control & 5) || !(s->core1_control & 2));
        XtensaCPU *cpu = XTENSA_CPU(s->cpus[core]);
        if (cpu->env.runstall != (stalled || gated)) {
            xtensa_runstall(&cpu->env, stalled || gated);
        }
    }
}

static void esp32s3_clock_update(ESP32S3ClockState *s)
{
    unsigned source = FIELD_EX32(s->sysclk, SYSTEM_SYSCLK_CONF, SOC_CLK_SEL);
    unsigned divider = FIELD_EX32(s->sysclk, SYSTEM_SYSCLK_CONF, PRE_DIV_CNT) + 1;
    unsigned period = FIELD_EX32(s->cpuperconf, SYSTEM_CPU_PER_CONF, CPUPERIOD_SEL);
    uint32_t hz;

    switch (source) {
    case ESP32S3_CLK_SEL_PLL:
        if (period > 2) {
            return;
        }
        hz = (period + 1) * 80000000;
        break;
    case ESP32S3_CLK_SEL_XTAL:
        hz = 40000000 / divider;
        break;
    case ESP32S3_CLK_SEL_RCFAST:
        hz = 17500000 / divider;
        break;
    default:
        return;
    }
    if (hz == s->cpu_hz) {
        return;
    }
    s->cpu_hz = hz;
    for (unsigned i = 0; i < ARRAY_SIZE(s->cpus); i++) {
        if (!s->cpus[i]) {
            continue;
        }
        XtensaCPU *cpu = XTENSA_CPU(s->cpus[i]);
        CPUXtensaState *env = &cpu->env;
        uint64_t now = qemu_clock_get_ns(QEMU_CLOCK_VIRTUAL);
        /* Preserve CCOUNT at the switch, then count at the new frequency. */
        env->ccount_base += clock_ns_to_ticks(cpu->clock, now - env->time_base);
        env->time_base = env->ccount_time = now;
        env->sregs[CCOUNT] = env->ccount_base;
        clock_set_hz(cpu->clock, hz);
        for (unsigned j = 0; j < env->config->nccompare; j++) {
            if (timer_pending(env->ccompare[j].timer)) {
                uint32_t ticks = env->sregs[CCOMPARE + j] - env->sregs[CCOUNT];
                timer_mod_ns(env->ccompare[j].timer, now +
                             clock_ticks_to_ns(cpu->clock, ticks ? ticks : (1ULL << 32)));
            }
        }
        cpu->cost_cpu_hz = hz;
        if (cpu->cost_clock_scaling) {
            queue_tb_flush(s->cpus[i]);
            cpu_interrupt(s->cpus[i], CPU_INTERRUPT_EXITTB);
        }
    }
}

static uint32_t esp32s3_read_cpu_intr(ESP32S3ClockState *s, uint32_t index)
{
    return (s->levels >> index) & 1;
}


static void esp32s3_write_cpu_intr(ESP32S3ClockState *s, uint32_t index, uint32_t value)
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

static uint32_t esp32s3_clock_get_ext_dev_enc_dec_ctrl(ESP32S3ClockState *s)
{
    return s->sys_ext_dev_enc_dec_ctrl;
}

static uint64_t esp32s3_clock_read(void *opaque, hwaddr addr, unsigned int size)
{
    ESP32S3ClockState *s = ESP32S3_CLOCK(opaque);
    uint64_t r = 0;

    switch(addr) {
        case A_SYSTEM_CORE_1_CONTROL_0_REG:
            r = s->core1_control;
            break;
        case A_SYSTEM_CORE_1_CONTROL_1_REG:
                r = s->app_cpu_addr;
            break;
        case A_SYSTEM_CPU_PER_CONF:
            r = s->cpuperconf;
            break;
        case A_SYSTEM_SYSCLK_CONF:
            r = s->sysclk;
            break;
        case A_SYSTEM_BT_LPCK_DIV_INT:
            r = s->bt_lpck_div_int;
            break;
        case A_SYSTEM_BT_LPCK_DIV_FRAC:
            r = s->bt_lpck_div_frac;
            break;
        case A_SYSTEM_CPU_INTR_FROM_CPU_0:
        case A_SYSTEM_CPU_INTR_FROM_CPU_1:
        case A_SYSTEM_CPU_INTR_FROM_CPU_2:
        case A_SYSTEM_CPU_INTR_FROM_CPU_3:
            r = esp32s3_read_cpu_intr(s, (addr - A_SYSTEM_CPU_INTR_FROM_CPU_0) / sizeof(uint32_t));
            break;
        case A_SYSTEM_EXTERNAL_DEVICE_ENCRYPT_DECRYPT_CONTROL:
            r = s->sys_ext_dev_enc_dec_ctrl;
            break;
        default:
#if CLOCK_WARNING
            warn_report("[CLOCK] Unsupported read from %08lx\n", addr);
#endif
            break;
    }
    return r;
}

static void esp32s3_clock_write(void *opaque, hwaddr addr, uint64_t value,
                                unsigned int size)
{
    ESP32S3ClockState *s = ESP32S3_CLOCK(opaque);

    switch(addr) {
        case A_SYSTEM_CORE_1_CONTROL_0_REG:
            if ((value & 4) && !(s->core1_control & 4) && s->cpus[1]) {
                cpu_reset(s->cpus[1]);
            }
            s->core1_control = value & 7;
            esp32s3_clock_set_cpu_stall(s, 1, s->rtc_stalled[1]);
            break;
        case A_SYSTEM_CPU_PER_CONF:
            s->cpuperconf = value & 0xff;
            esp32s3_clock_update(s);
            break;
        case A_SYSTEM_SYSCLK_CONF:
            s->sysclk = value & 0xfffff;
            esp32s3_clock_update(s);
            break;
        case A_SYSTEM_BT_LPCK_DIV_INT:
            s->bt_lpck_div_int = value & R_SYSTEM_BT_LPCK_DIV_INT_BT_LPCK_DIV_NUM_MASK;
            break;
        case A_SYSTEM_BT_LPCK_DIV_FRAC:
            s->bt_lpck_div_frac = value & 0x1fffffff;
            break;
        case A_SYSTEM_CORE_1_CONTROL_1_REG:
                s->app_cpu_addr = (uint32_t)value;
            break;
        case A_SYSTEM_CPU_INTR_FROM_CPU_0:
        case A_SYSTEM_CPU_INTR_FROM_CPU_1:
        case A_SYSTEM_CPU_INTR_FROM_CPU_2:
        case A_SYSTEM_CPU_INTR_FROM_CPU_3:
            esp32s3_write_cpu_intr(s, (addr - A_SYSTEM_CPU_INTR_FROM_CPU_0) / sizeof(uint32_t), value);
            break;
        case A_SYSTEM_EXTERNAL_DEVICE_ENCRYPT_DECRYPT_CONTROL:
            s->sys_ext_dev_enc_dec_ctrl = value;
            break;
        default:
#if CLOCK_WARNING
            warn_report("[CLOCK] Unsupported write to %08lx (%08lx)\n", addr, value);
#endif
            break;
    }
}

static const MemoryRegionOps esp32s3_clock_ops = {
    .read =  esp32s3_clock_read,
    .write = esp32s3_clock_write,
    .endianness = DEVICE_LITTLE_ENDIAN,
};

static void esp32s3_clock_reset_hold(Object *obj, ResetType type)
{
    ESP32S3ClockState *s = ESP32S3_CLOCK(obj);
    s->core1_control = 4;
    for (unsigned i = 0; i < ARRAY_SIZE(s->cpus); i++) {
        esp32s3_clock_set_cpu_stall(s, i, false);
    }
    s->bt_lpck_div_int = 255;
    s->bt_lpck_div_frac = R_SYSTEM_BT_LPCK_DIV_FRAC_LPCLK_SEL_8M_MASK |
                        (1 << R_SYSTEM_BT_LPCK_DIV_FRAC_BT_LPCK_DIV_A_SHIFT) | 1;
    /* On board reset, set the proper clocks and dividers */
    s->sysclk = ( 1 << R_SYSTEM_SYSCLK_CONF_PRE_DIV_CNT_SHIFT) |
                (ESP32S3_CLK_SEL_PLL << R_SYSTEM_SYSCLK_CONF_SOC_CLK_SEL_SHIFT) |
                (40 << R_SYSTEM_SYSCLK_CONF_CLK_XTAL_FREQ_SHIFT) |
                ( 1 << R_SYSTEM_SYSCLK_CONF_CLK_DIV_EN_SHIFT);

    /* Divider for PLL clock and APB  frequency */
    s->cpuperconf = (ESP32S3_PERIOD_SEL_80 << R_SYSTEM_CPU_PER_CONF_CPUPERIOD_SEL_SHIFT) |
                    (ESP32S3_FREQ_SEL_PLL_480 << R_SYSTEM_CPU_PER_CONF_PLL_FREQ_SEL_SHIFT);
    esp32s3_clock_update(s);

    /* Initialize the IRQs */
    s->levels = 0;
    for (int i = 0 ; i < ESP32S3_SYSTEM_CPU_INTR_COUNT; i++) {
        qemu_irq_lower(s->irqs[i]);
    }
}

static void esp32s3_clock_realize(DeviceState *dev, Error **errp)
{
    /* Initialize the registers */
    esp32s3_clock_reset_hold(OBJECT(dev), RESET_TYPE_COLD);
}

static void esp32s3_clock_init(Object *obj)
{
    ESP32S3ClockState *s = ESP32S3_CLOCK(obj);
    SysBusDevice *sbd = SYS_BUS_DEVICE(obj);

    memory_region_init_io(&s->iomem, obj, &esp32s3_clock_ops, s,
                          TYPE_ESP32S3_CLOCK, A_SYSTEM_COMB_PVT_ERR_HVT_SITE3 + sizeof(uint32_t));
    sysbus_init_mmio(sbd, &s->iomem);

    /* Initialize the output IRQ lines used to manually trigger interrupts */
    for (uint64_t i = 0; i < ESP32S3_SYSTEM_CPU_INTR_COUNT; i++) {
        sysbus_init_irq(sbd, &s->irqs[i]);
    }
}

static void esp32s3_clock_class_init(ObjectClass *klass, const void *data)
{
    DeviceClass *dc = DEVICE_CLASS(klass);
    ESP32S3ClockClass* esp32s3_clock = ESP32S3_CLOCK_CLASS(klass);
    ResettableClass *rc = RESETTABLE_CLASS(klass);

    rc->phases.hold = esp32s3_clock_reset_hold;
    dc->realize = esp32s3_clock_realize;

    esp32s3_clock->get_ext_dev_enc_dec_ctrl = esp32s3_clock_get_ext_dev_enc_dec_ctrl;
}

static const TypeInfo esp32s3_cache_info = {
    .name = TYPE_ESP32S3_CLOCK,
    .parent = TYPE_SYS_BUS_DEVICE,
    .instance_size = sizeof(ESP32S3ClockState),
    .instance_init = esp32s3_clock_init,
    .class_init = esp32s3_clock_class_init,
    .class_size = sizeof(ESP32S3ClockClass)
};

static void esp32s3_cache_register_types(void)
{
    type_register_static(&esp32s3_cache_info);
}

type_init(esp32s3_cache_register_types)
