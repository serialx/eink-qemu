/*
 * guest-clock: the guest's virtual clock, readable over QMP.
 *
 *   -object guest-clock,id=clock
 *   {"execute": "qom-get", "arguments": {"path": "/objects/clock", "property": "ns"}}
 *
 * "ns" is QEMU_CLOCK_VIRTUAL in nanoseconds. Under -icount it is the
 * guest's own time, which stops while the VM is stopped, so a test driver
 * can stamp what it observes and time its input in guest time. eink-qemu's
 * command-line tool (eink-qemu/desktop/cli) uses it; nothing reads it from
 * inside the guest.
 *
 * Copyright (c) 2026 eink-qemu contributors
 * SPDX-License-Identifier: GPL-2.0-or-later
 */

#include "qemu/osdep.h"
#include "qapi/visitor.h"
#include "qemu/module.h"
#include "qemu/timer.h"
#include "qom/object_interfaces.h"

#define TYPE_GUEST_CLOCK "guest-clock"

static void guest_clock_get_ns(Object *obj, Visitor *v, const char *name,
                               void *opaque, Error **errp)
{
    int64_t ns = qemu_clock_get_ns(QEMU_CLOCK_VIRTUAL);

    visit_type_int(v, name, &ns, errp);
}

static void guest_clock_class_init(ObjectClass *oc, const void *data)
{
    object_class_property_add(oc, "ns", "int", guest_clock_get_ns,
                              NULL, NULL, NULL);
    object_class_property_set_description(oc, "ns",
        "QEMU_CLOCK_VIRTUAL in nanoseconds (guest time under -icount)");
}

static const TypeInfo guest_clock_info = {
    .name = TYPE_GUEST_CLOCK,
    .parent = TYPE_OBJECT,
    .instance_size = sizeof(Object),
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
