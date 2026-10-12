/*
 * ESP32 RTC_CNTL (RTC block controller) device
 *
 * Copyright (c) 2019-2024 Espressif Systems (Shanghai) Co. Ltd.
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
#include "hw/timer/esp_timg.h"
#include "hw/core/sysbus.h"
#include "hw/core/irq.h"
#include "hw/core/qdev-properties.h"
#include "hw/misc/esp32s3_reg.h"
#include "hw/misc/esp32s3_rtc_cntl.h"

static void esp32s3_rtc_update_cpu_stall(Esp32s3RtcCntlState* s);
static void esp32s3_rtc_update_clk(Esp32s3RtcCntlState* s);


#define R_SLP_TIMER0        0x04
#define R_SLP_TIMER1        0x08
#define R_STATE0            0x18
#define R_WAKEUP_STATE      0x3C
#define R_INT_ENA           0x40
#define R_INT_RAW           0x44
#define R_INT_ST            0x48
#define R_INT_CLR           0x4C
#define R_SLP_WAKEUP_CAUSE  0x130
#define STATE0_SLEEP_EN     BIT(31)
#define SLP_TIMER1_ALARM_EN BIT(16)
#define INT_SLP_WAKEUP      BIT(0)
#define WAKE_GPIO           BIT(2)
#define WAKE_TIMER          BIT(3)
#define R_DIG_PWC           0x90
#define DIG_WRAP_PD_EN      BIT(31)

static void esp32s3_rtc_wake(Esp32s3RtcCntlState *s, uint32_t cause)
{
    if (!s->sleeping) {
        return;
    }
    s->sleeping = false;
    timer_del(&s->sleep_timer);
    qemu_set_irq(s->sleep_state, 0);
    s->state0_reg &= ~STATE0_SLEEP_EN;
    s->wakeup_cause = cause;
    s->int_raw |= INT_SLP_WAKEUP;
    qemu_set_irq(s->irq, (s->int_raw & s->int_ena) != 0);
    if (s->dig_pwc & DIG_WRAP_PD_EN) {
        s->reset_cause[0] = s->reset_cause[1] = ESP32_DEEPSLEEP_RESET;
        qemu_irq_pulse(s->dig_reset_req);
    }
}

static void esp32s3_rtc_sleep_timer_cb(void *opaque)
{
    esp32s3_rtc_wake(opaque, WAKE_TIMER);
}

/* Light sleep: the CPU spins on INT_RAW until a wake source fires. */
static void esp32s3_rtc_start_sleep(Esp32s3RtcCntlState *s)
{
    uint32_t ena = (s->wakeup_state_reg >> 15) & 0x1ffff;
    bool timer = (ena & WAKE_TIMER) && (s->slp_timer[1] & SLP_TIMER1_ALARM_EN);

    s->sleeping = true;
    qemu_set_irq(s->sleep_state, 1);
    s->sleep_start_ns = qemu_clock_get_ns(QEMU_CLOCK_VIRTUAL);
    if ((ena & WAKE_GPIO) && s->gpio_wake) {
        esp32s3_rtc_wake(s, WAKE_GPIO);
        return;
    }
    if (timer) {
        uint64_t target = s->slp_timer[0] | ((uint64_t)(s->slp_timer[1] & 0xffff) << 32);
        int64_t ns = s->time_base_ns;
        if (target > s->ticks_base) {
            ns += muldiv64(target - s->ticks_base, NANOSECONDS_PER_SECOND,
                           s->rtc_slowclk_freq);
        }
        if (ns > qemu_clock_get_ns(QEMU_CLOCK_VIRTUAL)) {
            timer_mod_ns(&s->sleep_timer, ns);
            return;
        }
        esp32s3_rtc_wake(s, WAKE_TIMER);
        return;
    }
    if (!(ena & WAKE_GPIO)) {
        /* No wake source we model: don't hang the guest. */
        esp32s3_rtc_wake(s, 0);
    }
}

static void esp32s3_rtc_set_gpio_wake(void *opaque, int n, int level)
{
    Esp32s3RtcCntlState *s = opaque;
    s->gpio_wake = level != 0;
    if (s->gpio_wake && s->sleeping && (((s->wakeup_state_reg >> 15) & 0x1ffff) & WAKE_GPIO)) {
        esp32s3_rtc_wake(s, WAKE_GPIO);
    }
}

static uint64_t esp32s3_rtc_cntl_read(void *opaque, hwaddr addr, unsigned int size)
{
    Esp32s3RtcCntlState *s = ESP32S3_RTC_CNTL(opaque);
    uint64_t r = 0;
    switch (addr) {
    case R_DIG_PWC:          r = s->dig_pwc; break;
    case R_SLP_TIMER0:       r = s->slp_timer[0]; break;
    case R_SLP_TIMER1:       r = s->slp_timer[1]; break;
    case R_STATE0:           r = s->state0_reg; break;
    case R_WAKEUP_STATE:     r = s->wakeup_state_reg; break;
    case R_INT_ENA:          r = s->int_ena; break;
    case R_INT_RAW:          r = s->int_raw; break;
    case R_INT_ST:           r = s->int_raw & s->int_ena; break;
    case R_SLP_WAKEUP_CAUSE: r = s->wakeup_cause; break;
    case A_RTC_CNTL_OPTIONS0:
        r = s->options0_reg;
        break;
    case A_RTC_CNTL_TIME_UPDATE:
        r = R_RTC_CNTL_TIME_UPDATE_VALID_MASK;
        break;
    case A_RTC_CNTL_TIME0:
        r = s->time_reg & UINT32_MAX;
        break;
    case A_RTC_CNTL_TIME1:
        r = s->time_reg >> 32;
        break;

    case A_RTC_CNTL_RESET_STATE:
        r = FIELD_DP32(r, RTC_CNTL_RESET_STATE, RESET_CAUSE_PROCPU, s->reset_cause[0]);
        r = FIELD_DP32(r, RTC_CNTL_RESET_STATE, RESET_CAUSE_APPCPU, s->reset_cause[1]);
        r = FIELD_DP32(r, RTC_CNTL_RESET_STATE, PROCPU_STAT_VECTOR_SEL, s->stat_vector_sel[0]);
        r = FIELD_DP32(r, RTC_CNTL_RESET_STATE, APPCPU_STAT_VECTOR_SEL, s->stat_vector_sel[1]);
        break;

    case A_RTC_CNTL_STORE0:
    case A_RTC_CNTL_STORE1:
    case A_RTC_CNTL_STORE2:
    case A_RTC_CNTL_STORE3:
        r = s->scratch_reg[(addr - A_RTC_CNTL_STORE0) / 4];
        break;

    case A_RTC_CNTL_CLK_CONF:
        r = FIELD_DP32(r, RTC_CNTL_CLK_CONF, SOC_CLK_SEL, s->soc_clk);
        r = FIELD_DP32(r, RTC_CNTL_CLK_CONF, FAST_CLK_RTC_SEL, s->rtc_fastclk);
        r = FIELD_DP32(r, RTC_CNTL_CLK_CONF, ANA_CLK_RTC_SEL, s->rtc_slowclk);
        break;

    case A_RTC_CNTL_SW_CPU_STALL:
        r = s->sw_cpu_stall_reg;
        break;

    case A_RTC_CNTL_STORE4:
    case A_RTC_CNTL_STORE5:
    case A_RTC_CNTL_STORE6:
    case A_RTC_CNTL_STORE7:
        r = s->scratch_reg[(addr - A_RTC_CNTL_STORE4) / 4 + 4];
        break;
    }
    return r;
}

static void esp32s3_rtc_cntl_write(void *opaque, hwaddr addr, uint64_t value,
                                 unsigned int size)
{
    Esp32s3RtcCntlState *s = ESP32S3_RTC_CNTL(opaque);
    switch (addr) {
    case R_DIG_PWC:      s->dig_pwc = value; break;
    case R_SLP_TIMER0:   s->slp_timer[0] = value; break;
    case R_SLP_TIMER1:   s->slp_timer[1] = value; break;
    case R_WAKEUP_STATE: s->wakeup_state_reg = value; break;
    case R_INT_ENA:
        s->int_ena = value;
        qemu_set_irq(s->irq, (s->int_raw & s->int_ena) != 0);
        break;
    case R_INT_CLR:
        s->int_raw &= ~value;
        qemu_set_irq(s->irq, (s->int_raw & s->int_ena) != 0);
        break;
    case R_STATE0:
        s->state0_reg = value;
        if ((value & STATE0_SLEEP_EN) && !s->sleeping) {
            esp32s3_rtc_start_sleep(s);
        }
        break;
    case A_RTC_CNTL_OPTIONS0:
        if (value & R_RTC_CNTL_OPTIONS0_SW_SYS_RESET_MASK) {
            s->reset_cause[0] = ESP32_SW_SYS_RESET;
            s->reset_cause[1] = ESP32_SW_SYS_RESET;
            qemu_irq_pulse(s->dig_reset_req);
            value &= ~(R_RTC_CNTL_OPTIONS0_SW_SYS_RESET_MASK);
        }
        if (value & R_RTC_CNTL_OPTIONS0_SW_APPCPU_RESET_MASK) {
            s->reset_cause[1] = ESP32_SW_CPU_RESET;
            qemu_irq_pulse(s->cpu_reset_req[1]);
            value &= ~(R_RTC_CNTL_OPTIONS0_SW_APPCPU_RESET_MASK);
        }
        if (value & R_RTC_CNTL_OPTIONS0_SW_PROCPU_RESET_MASK) {
            s->reset_cause[0] = ESP32_SW_CPU_RESET;
            qemu_irq_pulse(s->cpu_reset_req[0]);
            value &= ~(R_RTC_CNTL_OPTIONS0_SW_PROCPU_RESET_MASK);
        }
        s->options0_reg = value;
        esp32s3_rtc_update_cpu_stall(s);
        break;

    case A_RTC_CNTL_TIME_UPDATE:
        if (value & R_RTC_CNTL_TIME_UPDATE_UPDATE_MASK) {
            s->time_reg = s->ticks_base + muldiv64(
                qemu_clock_get_ns(QEMU_CLOCK_VIRTUAL) - s->time_base_ns,
                s->rtc_slowclk_freq, NANOSECONDS_PER_SECOND);
        }
        break;

    case A_RTC_CNTL_RESET_STATE:
        s->stat_vector_sel[0] = FIELD_EX32(value, RTC_CNTL_RESET_STATE,
                                           PROCPU_STAT_VECTOR_SEL);
        s->stat_vector_sel[1] = FIELD_EX32(value, RTC_CNTL_RESET_STATE,
                                           APPCPU_STAT_VECTOR_SEL);
        break;

    case A_RTC_CNTL_STORE0:
    case A_RTC_CNTL_STORE1:
    case A_RTC_CNTL_STORE2:
    case A_RTC_CNTL_STORE3:
        s->scratch_reg[(addr - A_RTC_CNTL_STORE0) / 4] = value;
        break;

    case A_RTC_CNTL_CLK_CONF:
        /* Changing the mux must not retroactively rescale elapsed time. */
        s->ticks_base += muldiv64(qemu_clock_get_ns(QEMU_CLOCK_VIRTUAL) -
            s->time_base_ns, s->rtc_slowclk_freq, NANOSECONDS_PER_SECOND);
        s->time_base_ns = qemu_clock_get_ns(QEMU_CLOCK_VIRTUAL);
        s->soc_clk = FIELD_EX32(value, RTC_CNTL_CLK_CONF, SOC_CLK_SEL);
        s->rtc_fastclk = FIELD_EX32(value, RTC_CNTL_CLK_CONF, FAST_CLK_RTC_SEL);
        s->rtc_slowclk = FIELD_EX32(value, RTC_CNTL_CLK_CONF, ANA_CLK_RTC_SEL);
        esp32s3_rtc_update_clk(s);
        break;

    case A_RTC_CNTL_SW_CPU_STALL:
        s->sw_cpu_stall_reg = value;
        esp32s3_rtc_update_cpu_stall(s);
        break;

    case A_RTC_CNTL_STORE4:
    case A_RTC_CNTL_STORE5:
    case A_RTC_CNTL_STORE6:
    case A_RTC_CNTL_STORE7:
        s->scratch_reg[(addr - A_RTC_CNTL_STORE4) / 4 + 4] = value;
        break;
    }
}

static void esp32s3_rtc_update_cpu_stall(Esp32s3RtcCntlState* s)
{
    uint32_t procpu_stall = (FIELD_EX32(s->sw_cpu_stall_reg, RTC_CNTL_SW_CPU_STALL, PROCPU_C1) << 2) |
                            (FIELD_EX32(s->options0_reg, RTC_CNTL_OPTIONS0, SW_STALL_PROCPU_C0));

    uint32_t appcpu_stall = (FIELD_EX32(s->sw_cpu_stall_reg, RTC_CNTL_SW_CPU_STALL, APPCPU_C1) << 2) |
                            (FIELD_EX32(s->options0_reg, RTC_CNTL_OPTIONS0, SW_STALL_APPCPU_C0));

    const uint32_t stall_magic_val = 0x86;

    s->cpu_stall_state[0] = procpu_stall == stall_magic_val;
    s->cpu_stall_state[1] = appcpu_stall == stall_magic_val;

    qemu_set_irq(s->cpu_stall_req[0], s->cpu_stall_state[0]);
    qemu_set_irq(s->cpu_stall_req[1], s->cpu_stall_state[1]);
}

static void esp32s3_rtc_update_clk(Esp32s3RtcCntlState* s)
{
    const uint32_t slowclk_freq[] = {
        ESP_RC_SLOW_FREQ, ESP_XTAL32K_FREQ, ESP_RC_FAST_DIV_FREQ,
        ESP_RC_SLOW_FREQ /* reserved selector: deterministic fallback */
    };
    const uint32_t fastclk_freq[] = {s->xtal_apb_freq / 4, ESP_RC_FAST_FREQ};
    s->rtc_slowclk_freq = slowclk_freq[s->rtc_slowclk];
    s->rtc_fastclk_freq = fastclk_freq[s->rtc_fastclk];
    qemu_irq_pulse(s->clk_update);
}

static const MemoryRegionOps esp32s3_rtc_cntl_ops = {
    .read =  esp32s3_rtc_cntl_read,
    .write = esp32s3_rtc_cntl_write,
    .endianness = DEVICE_LITTLE_ENDIAN,
};

static void esp32s3_rtc_cntl_reset_hold(Object *obj, ResetType type)
{
    Esp32s3RtcCntlState *s = ESP32S3_RTC_CNTL(obj);

    s->time_base_ns = qemu_clock_get_ns(QEMU_CLOCK_VIRTUAL);
    s->ticks_base = 0;
    s->sleeping = false;
    s->dig_pwc = 0;
    qemu_set_irq(s->sleep_state, 0);
    s->int_raw = 0;
    s->state0_reg = 0;
    timer_del(&s->sleep_timer);
}

static void esp32s3_rtc_cntl_realize(DeviceState *dev, Error **errp)
{
}

static void esp32s3_rtc_cntl_init(Object *obj)
{
    Esp32s3RtcCntlState *s = ESP32S3_RTC_CNTL(obj);
    SysBusDevice *sbd = SYS_BUS_DEVICE(obj);

    memory_region_init_io(&s->iomem, obj, &esp32s3_rtc_cntl_ops, s,
                          TYPE_ESP32S3_RTC_CNTL, ESP32S3_RTC_CNTL_SIZE);
    sysbus_init_mmio(sbd, &s->iomem);
    sysbus_init_irq(sbd, &s->irq);
    qdev_init_gpio_out_named(DEVICE(sbd), &s->sleep_state, "sleep", 1);
    qdev_init_gpio_out_named(DEVICE(sbd), &s->dig_reset_req, ESP32S3_RTC_DIG_RESET_GPIO, 1);
    qdev_init_gpio_out_named(DEVICE(sbd), &s->cpu_reset_req[0], ESP32S3_RTC_CPU_RESET_GPIO, ESP32S3_CPU_COUNT);
    qdev_init_gpio_out_named(DEVICE(sbd), &s->cpu_stall_req[0], ESP32S3_RTC_CPU_STALL_GPIO, ESP32S3_CPU_COUNT);
    qdev_init_gpio_out_named(DEVICE(sbd), &s->clk_update, ESP32S3_RTC_CLK_UPDATE_GPIO, 1);
    qdev_init_gpio_in_named(DEVICE(sbd), esp32s3_rtc_set_gpio_wake, ESP32S3_RTC_GPIO_WAKE, 1);
    timer_init_ns(&s->sleep_timer, QEMU_CLOCK_VIRTUAL, esp32s3_rtc_sleep_timer_cb, s);

    for (int i = 0; i < ESP32S3_CPU_COUNT; ++i) {
        s->reset_cause[i] = ESP32_POWERON_RESET;
        s->stat_vector_sel[i] = true;
    }

    s->rtc_slowclk = ESP32_SLOW_CLK_RC;
    s->rtc_fastclk = ESP32_FAST_CLK_8M;
    s->soc_clk = ESP32_SOC_CLK_XTAL;
    s->xtal_apb_freq = 40000000;
    s->pll_apb_freq = 80000000;
    esp32s3_rtc_update_clk(s);
}

static void esp32s3_rtc_cntl_class_init(ObjectClass *klass, const void *data)
{
    DeviceClass *dc = DEVICE_CLASS(klass);
    ResettableClass *rc = RESETTABLE_CLASS(klass);

    rc->phases.hold = esp32s3_rtc_cntl_reset_hold;
    dc->realize = esp32s3_rtc_cntl_realize;
}

static const TypeInfo esp32s3_rtc_cntl_info = {
    .name = TYPE_ESP32S3_RTC_CNTL,
    .parent = TYPE_SYS_BUS_DEVICE,
    .instance_size = sizeof(Esp32s3RtcCntlState),
    .instance_init = esp32s3_rtc_cntl_init,
    .class_init = esp32s3_rtc_cntl_class_init
};

static void esp32s3_rtc_cntl_register_types(void)
{
    type_register_static(&esp32s3_rtc_cntl_info);
}

type_init(esp32s3_rtc_cntl_register_types)
