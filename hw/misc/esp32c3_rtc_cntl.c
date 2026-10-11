/*
 * ESP32-C3 RTC CNTL
 *
 * eink-qemu adds the RTC timer (TIME_UPDATE latches TIME_LOW0/HIGH0 at the
 * 136 kHz slow clock) and sleep: SLEEP_EN with DG_WRAP_PD_EN clear is light
 * sleep, which ends on the RTC timer alarm or a GPIO wake and raises
 * SLP_WAKEUP in INT_RAW; with it set it is deep sleep, which ends the same
 * way but with a reset whose reason is DEEPSLEEP_RESET and whose cause is in
 * SLP_WAKEUP_CAUSE, the pads that woke it in GPIO_WAKEUP STATUS. The deep
 * sleep pads are GPIO0-5 on the "pad" lines, armed per pin in GPIO_WAKEUP
 * (level triggers 4 low / 5 high).
 *
 * Copyright (c) 2023 Espressif Systems (Shanghai) Co. Ltd.
 *
 * This program is free software; you can redistribute it and/or modify
 * it under the terms of the GNU General Public License version 2 or
 * (at your option) any later version.
 */

#include "qemu/osdep.h"
#include "qemu/log.h"
#include "qemu/error-report.h"
#include "qapi/error.h"
#include "hw/core/sysbus.h"
#include "hw/core/irq.h"
#include "hw/core/qdev-properties.h"
#include "hw/misc/esp32c3_rtc_cntl.h"
#include "hw/timer/esp_timg.h"
#include "qemu/timer.h"
#include "system/runstate.h"


#define RTCCNTL_DEBUG     0
#define RTCCNTL_WARNING   0

#define STATE0_SLEEP_EN     BIT(31)
#define SLP_TIMER1_ALARM_EN BIT(16)
#define TIME_UPDATE         BIT(31)
#define DG_WRAP_PD_EN       BIT(31)
#define INT_SLP_WAKEUP      BIT(0)
#define WAKE_GPIO           BIT(2)
#define WAKE_TIMER          BIT(3)
#define GPIO_WAKEUP_STATUS_CLR BIT(6)
#define LIGHT_SLEEP_EARLY_NS (5 * SCALE_MS)

static void esp32c3_reset_request(void *opaque, int n, int level);

static uint64_t esp32c3_rtc_ticks(ESP32C3RtcCntlState *s)
{
    return muldiv64(qemu_clock_get_ns(QEMU_CLOCK_VIRTUAL) - s->time_base_ns,
                    ESP_RC_SLOW_FREQ, NANOSECONDS_PER_SECOND);
}

static void esp32c3_rtc_update_irq(ESP32C3RtcCntlState *s)
{
    qemu_set_irq(s->irq, (s->int_raw & s->int_ena) != 0);
}

/* Deep-sleep pads at their wake level (GPIO_WAKEUP: enable bit 31-n, type bits 23-3n). */
static uint32_t esp32c3_rtc_pads_awake(ESP32C3RtcCntlState *s)
{
    uint32_t m = 0;

    for (int n = 0; n < ESP32C3_RTC_PAD_COUNT; n++) {
        uint32_t type = (s->gpio_wakeup >> (23 - 3 * n)) & 7;
        bool level = (s->pad_level >> n) & 1;
        if ((s->gpio_wakeup & BIT(31 - n)) && ((type == 4 && !level) || (type == 5 && level))) {
            m |= BIT(n);
        }
    }
    return m;
}

static void esp32c3_rtc_wake(ESP32C3RtcCntlState *s, uint32_t cause)
{
    if (!s->sleeping) {
        return;
    }
    s->sleeping = false;
    timer_del(&s->sleep_timer);
    s->wakeup_cause = cause;
    if (s->dig_pwc & DG_WRAP_PD_EN) {
        /* deep sleep: the digital core restarts */
        s->gpio_wakeup = (s->gpio_wakeup & ~0x3f) | (cause & WAKE_GPIO ? esp32c3_rtc_pads_awake(s) : 0);
        qemu_system_wakeup_request(QEMU_WAKEUP_REASON_OTHER, NULL);
        esp32c3_reset_request(s, ESP32C3_DEEPSLEEP_RESET, 1);
        return;
    }
    /*
     * The systimer keeps counting in QEMU while the guest "sleeps", but IDF
     * also advances it by the RTC-measured sleep time. Hide the sleep from
     * the RTC counter so the time is only counted once.
     */
    s->time_base_ns += qemu_clock_get_ns(QEMU_CLOCK_VIRTUAL) - s->sleep_start_ns;
    s->state0 &= ~STATE0_SLEEP_EN;
    s->int_raw |= INT_SLP_WAKEUP;
    esp32c3_rtc_update_irq(s);
}

static void esp32c3_rtc_sleep_timer_cb(void *opaque)
{
    esp32c3_rtc_wake(opaque, WAKE_TIMER);
}

/* The CPU spins on INT_RAW (or, in deep sleep, until the reset) while this waits. */
static void esp32c3_rtc_start_sleep(ESP32C3RtcCntlState *s)
{
    uint32_t ena = (s->wakeup_state >> 15) & 0x1ffff;
    bool deep = s->dig_pwc & DG_WRAP_PD_EN;
    bool timer = (ena & WAKE_TIMER) && (s->slp_timer[1] & SLP_TIMER1_ALARM_EN);

    s->sleeping = true;
    s->sleep_start_ns = qemu_clock_get_ns(QEMU_CLOCK_VIRTUAL);
    if ((ena & WAKE_GPIO) && (deep ? esp32c3_rtc_pads_awake(s) != 0 : s->gpio_wake)) {
        esp32c3_rtc_wake(s, WAKE_GPIO);
        return;
    }
    if (timer) {
        uint64_t target = s->slp_timer[0] | ((uint64_t)(s->slp_timer[1] & 0xffff) << 32);
        /*
         * Wake a little early: TCG runs the sleep entry/exit code slower than
         * silicon, and FreeRTOS asserts if the measured sleep overshoots the
         * idle time it planned (vTaskStepTick).
         */
        int64_t ns = s->time_base_ns + muldiv64(target, NANOSECONDS_PER_SECOND, ESP_RC_SLOW_FREQ)
                     - (deep ? 0 : LIGHT_SLEEP_EARLY_NS);
        if (ns > qemu_clock_get_ns(QEMU_CLOCK_VIRTUAL)) {
            timer_mod_ns(&s->sleep_timer, ns);
            return;
        }
        esp32c3_rtc_wake(s, WAKE_TIMER);
        return;
    }
    if (!(ena & WAKE_GPIO)) {
        /* No wake source we model: don't hang the guest. */
        esp32c3_rtc_wake(s, 0);
        return;
    }
    if (deep) {
        /*
         * The digital core is off until a pad wakes it: suspend the VM (the
         * virtual clock stops, so no watchdog bites the spinning CPU); the
         * wake resumes it into the reset. A timer wake was armed above and
         * can't fire while suspended: GPIO-only deep sleep is what the X3
         * firmware uses.
         */
        qemu_system_suspend_request();
    }
}

static void esp32c3_rtc_set_gpio_wake(void *opaque, int n, int level)
{
    ESP32C3RtcCntlState *s = opaque;

    s->gpio_wake = level != 0;
    if (s->gpio_wake && s->sleeping && !(s->dig_pwc & DG_WRAP_PD_EN) &&
        (((s->wakeup_state >> 15) & 0x1ffff) & WAKE_GPIO)) {
        esp32c3_rtc_wake(s, WAKE_GPIO);
    }
}

static void esp32c3_rtc_set_pad(void *opaque, int n, int level)
{
    ESP32C3RtcCntlState *s = opaque;

    s->pad_level = (s->pad_level & ~BIT(n)) | (level ? BIT(n) : 0);
    if (s->sleeping && (s->dig_pwc & DG_WRAP_PD_EN) &&
        (((s->wakeup_state >> 15) & 0x1ffff) & WAKE_GPIO) && esp32c3_rtc_pads_awake(s)) {
        esp32c3_rtc_wake(s, WAKE_GPIO);
    }
}


static void esp32c3_reset_request(void *opaque, int n, int level)
{
    ESP32C3RtcCntlState *s = ESP32C3_RTC_CNTL(opaque);
    /* Make sure the "reset reason" is correct */
    assert(n < ESP32C3_COUNT_RESET);

    if (level) {
        s->reason = n;
        qemu_irq_raise(s->cpu_reset);
    }
}


static uint64_t esp32c3_rtc_cntl_read(void* opaque, hwaddr addr, unsigned int size)
{
    ESP32C3RtcCntlState *s = ESP32C3_RTC_CNTL(opaque);
    uint64_t r = 0;

    switch(addr) {
        case A_RTC_CNTL_RTC_OPTIONS0:
            r = s->options0;
            break;
        case A_RTC_CNTL_RTC_RESET_STATE:
            r = s->reason;
            break;
        case A_RTC_CNTL_RTC_SLP_TIMER0:     r = s->slp_timer[0]; break;
        case A_RTC_CNTL_RTC_SLP_TIMER1:     r = s->slp_timer[1]; break;
        case A_RTC_CNTL_RTC_TIME_UPDATE:    r = TIME_UPDATE; break;
        case A_RTC_CNTL_RTC_TIME_LOW0:
        case A_RTC_CNTL_RTC_TIME_LOW1:      r = (uint32_t)s->time_latch; break;
        case A_RTC_CNTL_RTC_TIME_HIGH0:
        case A_RTC_CNTL_RTC_TIME_HIGH1:     r = (s->time_latch >> 32) & 0xffff; break;
        case A_RTC_CNTL_RTC_STATE0:         r = s->state0; break;
        case A_RTC_CNTL_RTC_WAKEUP_STATE:   r = s->wakeup_state; break;
        case A_RTC_CNTL_INT_ENA_RTC:        r = s->int_ena; break;
        case A_RTC_CNTL_INT_RAW_RTC:        r = s->int_raw; break;
        case A_RTC_CNTL_INT_ST_RTC:         r = s->int_raw & s->int_ena; break;
        case A_RTC_CNTL_DIG_PWC:            r = s->dig_pwc; break;
        case A_RTC_CNTL_RTC_SLP_WAKEUP_CAUSE: r = s->wakeup_cause; break;
        case A_RTC_CNTL_RTC_CNTL_GPIO_WAKEUP: r = s->gpio_wakeup; break;

        case A_RTC_CNTL_RTC_STORE0:
        case A_RTC_CNTL_RTC_STORE1:
        case A_RTC_CNTL_RTC_STORE2:
        case A_RTC_CNTL_RTC_STORE3:
            r = s->scratch_reg[(addr - A_RTC_CNTL_RTC_STORE0) / 4];
            break;

        case A_RTC_CNTL_RTC_STORE4:
        case A_RTC_CNTL_RTC_STORE5:
        case A_RTC_CNTL_RTC_STORE6:
        case A_RTC_CNTL_RTC_STORE7:
            r = s->scratch_reg[(addr - A_RTC_CNTL_RTC_STORE4) / 4 + 4];
            break;
        default:
#if RTCCNTL_WARNING
            /* Other registers are not supported yet */
            warn_report("[RTCCNTL] Unsupported read to %08lx", addr);
#endif
            break;
    }

    return r;
}


static void esp32c3_rtc_cntl_write(void* opaque, hwaddr addr, uint64_t value, unsigned int size)
{
    ESP32C3RtcCntlState *s = ESP32C3_RTC_CNTL(opaque);
    const uint32_t c_value = value;

    switch(addr) {
        case A_RTC_CNTL_RTC_OPTIONS0:
            CLEAR_BIT(value, R_RTC_CNTL_RTC_OPTIONS0_SW_SYS_RST_SHIFT);
            CLEAR_BIT(value, R_RTC_CNTL_RTC_OPTIONS0_SW_PROCPU_RST_SHIFT);
            s->options0 = value;
            /* Check if we have to reset the CPU/machine */
            if (FIELD_EX32(c_value, RTC_CNTL_RTC_OPTIONS0, SW_SYS_RST)) {
                esp32c3_reset_request(opaque, ESP32C3_RTC_SW_SYS_RESET, 1);
            } else if (FIELD_EX32(c_value, RTC_CNTL_RTC_OPTIONS0, SW_PROCPU_RST)) {
                esp32c3_reset_request(opaque, ESP32C3_RTC_SW_CPU_RESET, 1);
            }
            break;

        case A_RTC_CNTL_RTC_STORE0:
        case A_RTC_CNTL_RTC_STORE1:
        case A_RTC_CNTL_RTC_STORE2:
        case A_RTC_CNTL_RTC_STORE3:
            s->scratch_reg[(addr - A_RTC_CNTL_RTC_STORE0) / 4] = value;
            break;

        case A_RTC_CNTL_RTC_STORE4:
        case A_RTC_CNTL_RTC_STORE5:
        case A_RTC_CNTL_RTC_STORE6:
        case A_RTC_CNTL_RTC_STORE7:
            s->scratch_reg[(addr - A_RTC_CNTL_RTC_STORE4) / 4 + 4] = value;
            break;

        case A_RTC_CNTL_RTC_SLP_TIMER0:     s->slp_timer[0] = value; break;
        case A_RTC_CNTL_RTC_SLP_TIMER1:     s->slp_timer[1] = value; break;
        case A_RTC_CNTL_RTC_TIME_UPDATE:
            if (value & TIME_UPDATE) {
                s->time_latch = esp32c3_rtc_ticks(s);
            }
            break;
        case A_RTC_CNTL_RTC_WAKEUP_STATE:   s->wakeup_state = value; break;
        case A_RTC_CNTL_DIG_PWC:            s->dig_pwc = value; break;
        case A_RTC_CNTL_INT_ENA_RTC:
            s->int_ena = value;
            esp32c3_rtc_update_irq(s);
            break;
        case A_RTC_CNTL_INT_CLR_RTC:
            s->int_raw &= ~value;
            esp32c3_rtc_update_irq(s);
            break;
        case A_RTC_CNTL_RTC_CNTL_GPIO_WAKEUP:
            s->gpio_wakeup = (value & ~0x3f) | (value & GPIO_WAKEUP_STATUS_CLR ? 0 : s->gpio_wakeup & 0x3f);
            break;
        case A_RTC_CNTL_RTC_STATE0:
            s->state0 = value;
            if ((value & STATE0_SLEEP_EN) && !s->sleeping) {
                esp32c3_rtc_start_sleep(s);
            }
            break;

        default:
#if RTCCNTL_WARNING
            /* Other registers are not supported yet */
            warn_report("[RTCCNTL] Unsupported write to %08lx (%08lx)", addr, value);
#endif
            break;
    }
}


static const MemoryRegionOps esp_rtc_cntl_ops = {
    .read =  esp32c3_rtc_cntl_read,
    .write = esp32c3_rtc_cntl_write,
    .endianness = DEVICE_LITTLE_ENDIAN,
};


static void esp32c3_rtc_cntl_reset_hold(Object *obj, ResetType type)
{
    static bool first_boot = true;
    ESP32C3RtcCntlState *s = ESP32C3_RTC_CNTL(obj);
    s->options0 = 0;

    if (first_boot) {
        s->reason = ESP32C3_POWERON_RESET;
        s->wakeup_cause = 0;
        s->gpio_wakeup = 0;
        s->pad_level = 0x3f;
        first_boot = false;
    }
    /* the wake cause and pad status survive a deep-sleep reset */
    s->sleeping = false;
    s->gpio_wake = false;
    s->state0 = 0;
    s->int_raw = 0;
    s->int_ena = 0;
    s->dig_pwc = 0;
    s->wakeup_state = 0;
    s->time_latch = 0;
    s->time_base_ns = qemu_clock_get_ns(QEMU_CLOCK_VIRTUAL);
    timer_del(&s->sleep_timer);

    qemu_irq_lower(s->cpu_reset);
}


static void esp32c3_rtc_cntl_realize(DeviceState *dev, Error **errp)
{
    ESP32C3RtcCntlState *s = ESP32C3_RTC_CNTL(dev);
    esp32c3_rtc_cntl_reset_hold(OBJECT(dev), RESET_TYPE_COLD);
    (void) s;
}


static void esp32c3_rtc_cntl_init(Object *obj)
{
    ESP32C3RtcCntlState *s = ESP32C3_RTC_CNTL(obj);
    SysBusDevice *sbd = SYS_BUS_DEVICE(obj);

    memory_region_init_io(&s->iomem, obj, &esp_rtc_cntl_ops, s,
                          TYPE_ESP32C3_RTC_CNTL, ESP32C3_RTC_CNTL_IO_SIZE);
    sysbus_init_mmio(sbd, &s->iomem);

    /* Initialize ESP32C3_COUNT_RESET input lines, each representing a reset source (reason) */
    qdev_init_gpio_in(DEVICE(s), esp32c3_reset_request, ESP32C3_COUNT_RESET);
    /* Initialize the GPIO that will notify the CPU to reset itself */
    qdev_init_gpio_out_named(DEVICE(s), &s->cpu_reset, ESP32C3_RTC_CPU_RESET_GPIO, 1);
    sysbus_init_irq(sbd, &s->irq);
    qdev_init_gpio_in_named(DEVICE(s), esp32c3_rtc_set_gpio_wake, ESP32C3_RTC_GPIO_WAKE, 1);
    qdev_init_gpio_in_named(DEVICE(s), esp32c3_rtc_set_pad, ESP32C3_RTC_PAD, ESP32C3_RTC_PAD_COUNT);
    timer_init_ns(&s->sleep_timer, QEMU_CLOCK_VIRTUAL, esp32c3_rtc_sleep_timer_cb, s);
    qemu_system_wakeup_enable(QEMU_WAKEUP_REASON_OTHER, true);
}


static void esp32c3_rtc_cntl_class_init(ObjectClass *klass, const void *data)
{
    DeviceClass *dc = DEVICE_CLASS(klass);
    ResettableClass *rc = RESETTABLE_CLASS(klass);

    rc->phases.hold = esp32c3_rtc_cntl_reset_hold;
    dc->realize = esp32c3_rtc_cntl_realize;
}


static const TypeInfo esp32c3_rtc_cntl_info = {
    .name = TYPE_ESP32C3_RTC_CNTL,
    .parent = TYPE_SYS_BUS_DEVICE,
    .instance_size = sizeof(ESP32C3RtcCntlState),
    .instance_init = esp32c3_rtc_cntl_init,
    .class_init = esp32c3_rtc_cntl_class_init
};


static void esp32c3_rtc_cntl_register_types(void)
{
    type_register_static(&esp32c3_rtc_cntl_info);
}


type_init(esp32c3_rtc_cntl_register_types)
