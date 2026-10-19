/*
 * iotrace: log every guest memory access to chosen physical address ranges,
 * with the PC of the instruction, to a text file. For working out how a
 * closed driver talks to a device (register block and the memory it shares
 * with it) before modeling it.
 *
 *   -plugin libiotrace.so,range=0x60031000-0x60032000,range=0x3fc00000-0x3fc10000[,out=FILE]
 *
 * Lines: <pc> <R|W> <phys addr> <size> <value>
 *
 * SPDX-License-Identifier: GPL-2.0-or-later
 */
#include <glib.h>
#include <inttypes.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <qemu-plugin.h>

QEMU_PLUGIN_EXPORT int qemu_plugin_version = QEMU_PLUGIN_VERSION;

typedef struct { uint64_t lo, hi; } Range;
static GArray *ranges;
static FILE *out;
static GMutex lock;

static bool in_range(uint64_t a)
{
    for (guint i = 0; i < ranges->len; i++) {
        Range *r = &g_array_index(ranges, Range, i);
        if (a >= r->lo && a < r->hi) {
            return true;
        }
    }
    return false;
}

static void vcpu_mem(unsigned int cpu_index, qemu_plugin_meminfo_t info,
                     uint64_t vaddr, void *udata)
{
    struct qemu_plugin_hwaddr *h = qemu_plugin_get_hwaddr(info, vaddr);
    uint64_t addr = h ? qemu_plugin_hwaddr_phys_addr(h) : vaddr;

    if (!in_range(addr)) {
        return;
    }
    qemu_plugin_mem_value v = qemu_plugin_mem_get_value(info);
    uint64_t val = v.type == QEMU_PLUGIN_MEM_VALUE_U8 ? v.data.u8 :
                   v.type == QEMU_PLUGIN_MEM_VALUE_U16 ? v.data.u16 :
                   v.type == QEMU_PLUGIN_MEM_VALUE_U32 ? v.data.u32 : v.data.u64;
    g_mutex_lock(&lock);
    fprintf(out, "%08" PRIx64 " %c %08" PRIx64 " %u %" PRIx64 "\n",
            (uint64_t)(uintptr_t)udata, qemu_plugin_mem_is_store(info) ? 'W' : 'R',
            addr, 1u << qemu_plugin_mem_size_shift(info), val);
    g_mutex_unlock(&lock);
}

static void vcpu_tb_trans(struct qemu_plugin_tb *tb, void *udata)
{
    size_t n = qemu_plugin_tb_n_insns(tb);
    for (size_t i = 0; i < n; i++) {
        struct qemu_plugin_insn *insn = qemu_plugin_tb_get_insn(tb, i);
        qemu_plugin_register_vcpu_mem_cb(insn, vcpu_mem, QEMU_PLUGIN_CB_NO_REGS,
                                         QEMU_PLUGIN_MEM_RW,
                                         (void *)(uintptr_t)qemu_plugin_insn_vaddr(insn));
    }
}

static void plugin_exit(void *p)
{
    fclose(out);
}

QEMU_PLUGIN_EXPORT int qemu_plugin_install(qemu_plugin_id_t id,
                                           const qemu_info_t *info, int argc,
                                           char **argv)
{
    const char *path = "/tmp/iotrace.txt";

    ranges = g_array_new(FALSE, FALSE, sizeof(Range));
    for (int i = 0; i < argc; i++) {
        char **kv = g_strsplit(argv[i], "=", 2);
        if (!strcmp(kv[0], "range") && kv[1]) {
            char **lh = g_strsplit(kv[1], "-", 2);
            Range r = { strtoull(lh[0], NULL, 0), lh[1] ? strtoull(lh[1], NULL, 0) : 0 };
            g_array_append_val(ranges, r);
            g_strfreev(lh);
        } else if (!strcmp(kv[0], "out") && kv[1]) {
            path = g_strdup(kv[1]);
        } else {
            fprintf(stderr, "iotrace: unknown option %s\n", argv[i]);
            g_strfreev(kv);
            return -1;
        }
        g_strfreev(kv);
    }
    if (!ranges->len) {
        fprintf(stderr, "iotrace: at least one range=LO-HI is required\n");
        return -1;
    }
    out = fopen(path, "w");
    if (!out) {
        fprintf(stderr, "iotrace: cannot write %s\n", path);
        return -1;
    }
    qemu_plugin_register_vcpu_tb_trans_cb(id, vcpu_tb_trans, NULL);
    qemu_plugin_register_atexit_cb(id, plugin_exit, NULL);
    return 0;
}
