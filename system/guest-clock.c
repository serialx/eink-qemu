/*
 * guest-clock: the guest's virtual clock, readable over QMP.
 *
 *   -object guest-clock,id=clock
 *   {"execute": "qom-get", "arguments": {"path": "/objects/clock", "property": "ns"}}
 *
 * "ns" is QEMU_CLOCK_VIRTUAL in nanoseconds. Under -icount it is the
 * guest's own time, which stops while the VM is stopped, so a test driver
 * can stamp what it observes and time its input in guest time.
 *
 * "busy-ns" is the part of that time the CPUs spent executing (the
 * instruction counter, and the device stalls charged to it, in ns); time
 * the guest spends idle (WFI, waiting for an interrupt) is not in it. Over
 * an interval, its growth divided by the growth of "ns" is the CPU load,
 * near 1 while firmware works, even silently, and near 0 while it idles.
 * -1 without -icount.
 *
 * "pause-at-ns" stops the VM (as QMP "stop" does) when "ns" reaches the
 * given value: a driver that sends input with the VM stopped, sets it and
 * continues gets the next input in at an exact guest time, however long it
 * takes to notice the pause (a key held 150 ms is held 150 ms). It reads
 * the pending time, or -1 when none is pending (and after the pause); a
 * negative value cancels it.
 *
 * eink-qemu's command-line tool (eink-qemu/desktop/cli) uses them; nothing
 * reads them from inside the guest.
 *
 * Copyright (c) 2026 eink-qemu contributors
 * SPDX-License-Identifier: GPL-2.0-or-later
 */

#include "qemu/osdep.h"
#include "qapi/visitor.h"
#include "qemu/module.h"
#include "qemu/timer.h"
#include "qom/object_interfaces.h"
#include "exec/icount.h"
#include "system/runstate.h"

#define TYPE_GUEST_CLOCK "guest-clock"
OBJECT_DECLARE_SIMPLE_TYPE(GuestClock, GUEST_CLOCK)

struct GuestClock {
    Object parent_obj;
    QEMUTimer pause_timer;
};

static void guest_clock_pause(void *opaque)
{
    vm_stop(RUN_STATE_PAUSED);
}

static void guest_clock_get_pause_at_ns(Object *obj, Visitor *v,
                                        const char *name, void *opaque,
                                        Error **errp)
{
    GuestClock *s = GUEST_CLOCK(obj);
    int64_t ns = timer_pending(&s->pause_timer) ?
                 timer_expire_time_ns(&s->pause_timer) : -1;

    visit_type_int(v, name, &ns, errp);
}

static void guest_clock_set_pause_at_ns(Object *obj, Visitor *v,
                                        const char *name, void *opaque,
                                        Error **errp)
{
    GuestClock *s = GUEST_CLOCK(obj);
    int64_t ns;

    if (!visit_type_int(v, name, &ns, errp)) {
        return;
    }
    if (ns < 0) {
        timer_del(&s->pause_timer);
    } else {
        /* A time already past pauses as soon as the VM runs. */
        timer_mod(&s->pause_timer, ns);
    }
}

static void guest_clock_init(Object *obj)
{
    GuestClock *s = GUEST_CLOCK(obj);

    timer_init_ns(&s->pause_timer, QEMU_CLOCK_VIRTUAL, guest_clock_pause, s);
}

static void guest_clock_finalize(Object *obj)
{
    timer_del(&GUEST_CLOCK(obj)->pause_timer);
}

static void guest_clock_get_ns(Object *obj, Visitor *v, const char *name,
                               void *opaque, Error **errp)
{
    int64_t ns = qemu_clock_get_ns(QEMU_CLOCK_VIRTUAL);

    visit_type_int(v, name, &ns, errp);
}

static void guest_clock_get_busy_ns(Object *obj, Visitor *v, const char *name,
                                    void *opaque, Error **errp)
{
    int64_t ns = icount_enabled() ? icount_to_ns(icount_get_raw()) : -1;

    visit_type_int(v, name, &ns, errp);
}

static void guest_clock_class_init(ObjectClass *oc, const void *data)
{
    object_class_property_add(oc, "ns", "int", guest_clock_get_ns,
                              NULL, NULL, NULL);
    object_class_property_set_description(oc, "ns",
        "QEMU_CLOCK_VIRTUAL in nanoseconds (guest time under -icount)");
    object_class_property_add(oc, "busy-ns", "int", guest_clock_get_busy_ns,
                              NULL, NULL, NULL);
    object_class_property_set_description(oc, "busy-ns",
        "guest time spent executing, in nanoseconds (-1 without -icount)");
    object_class_property_add(oc, "pause-at-ns", "int",
                              guest_clock_get_pause_at_ns,
                              guest_clock_set_pause_at_ns, NULL, NULL);
    object_class_property_set_description(oc, "pause-at-ns",
        "stop the VM when \"ns\" reaches this (-1: none pending)");
}

static const TypeInfo guest_clock_info = {
    .name = TYPE_GUEST_CLOCK,
    .parent = TYPE_OBJECT,
    .instance_size = sizeof(GuestClock),
    .instance_init = guest_clock_init,
    .instance_finalize = guest_clock_finalize,
    .class_init = guest_clock_class_init,
    .interfaces = (const InterfaceInfo[]) {
        { TYPE_USER_CREATABLE },
        { }
    },
};

static void guest_clock_register_types(void)
{
    type_register_static(&guest_clock_info);
}

type_init(guest_clock_register_types)
