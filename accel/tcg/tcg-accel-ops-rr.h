/*
 * QEMU TCG Single Threaded vCPUs implementation
 *
 * Copyright 2020 SUSE LLC
 *
 * This work is licensed under the terms of the GNU GPL, version 2 or later.
 * See the COPYING file in the top-level directory.
 */

#ifndef TCG_ACCEL_OPS_RR_H
#define TCG_ACCEL_OPS_RR_H

#define TCG_KICK_PERIOD (NANOSECONDS_PER_SECOND / 10)

/* Kick all RR vCPUs. */
void rr_kick_vcpu_thread(CPUState *unused);

#ifdef __EMSCRIPTEN__
/* Interrupt a possible single-vCPU retained BQL owner for a host waiter. */
void rr_kick_bql_holder(void);
#endif

/* start the round robin vcpu thread */
void rr_start_vcpu_thread(CPUState *cpu);

#endif /* TCG_ACCEL_OPS_RR_H */
