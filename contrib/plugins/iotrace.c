/*
 * iotrace: log every guest memory access to chosen physical address ranges,
 * with the PC of the instruction, to a text file. For working out how a
 * closed driver talks to a device (register block and the memory it shares
 * with it) before modeling it.
 *
 *   -plugin libiotrace.so,range=0x60031000-0x60032000,range=0x3fc00000-0x3fc10000[,out=FILE][,limit=N]
 *
 * Lines: <pc> <R|W> <phys addr> <size> <value>. A range is [LO, HI). `limit`
 * stops recording after N lines (default unlimited) and ends the file with a
 * `#` comment line saying so. A write error stops recording and is reported
 * on stderr.
 *
 * SPDX-License-Identifier: GPL-2.0-or-later
 */
#include <errno.h>
#include <glib.h>
#include <inttypes.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <qemu-plugin.h>

QEMU_PLUGIN_EXPORT int qemu_plugin_version = QEMU_PLUGIN_VERSION;

typedef struct {
    uint64_t lo;
    uint64_t hi;
} Range;

static GArray *ranges;
static FILE *out;
static GMutex lock;
static uint64_t limit;
static uint64_t lines;
static bool stopped;

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
    qemu_plugin_mem_value v;
    char val[33];
    int n;

    if (!in_range(addr)) {
        return;
    }
    v = qemu_plugin_mem_get_value(info);
    switch (v.type) {
    case QEMU_PLUGIN_MEM_VALUE_U8:
        snprintf(val, sizeof(val), "%" PRIx8, v.data.u8);
        break;
    case QEMU_PLUGIN_MEM_VALUE_U16:
        snprintf(val, sizeof(val), "%" PRIx16, v.data.u16);
        break;
    case QEMU_PLUGIN_MEM_VALUE_U32:
        snprintf(val, sizeof(val), "%" PRIx32, v.data.u32);
        break;
    case QEMU_PLUGIN_MEM_VALUE_U64:
        snprintf(val, sizeof(val), "%" PRIx64, v.data.u64);
        break;
    case QEMU_PLUGIN_MEM_VALUE_U128:
        snprintf(val, sizeof(val), "%016" PRIx64 "%016" PRIx64,
                 v.data.u128.high, v.data.u128.low);
        break;
    default:
        snprintf(val, sizeof(val), "?");
        break;
    }
    g_mutex_lock(&lock);
    if (!stopped) {
        n = fprintf(out, "%08" PRIx64 " %c %08" PRIx64 " %u %s\n",
                    (uint64_t)(uintptr_t)udata,
                    qemu_plugin_mem_is_store(info) ? 'W' : 'R',
                    addr, 1u << qemu_plugin_mem_size_shift(info), val);
        lines++;
        if (n < 0) {
            stopped = true;
            fprintf(stderr, "iotrace: write failed: %s\n", strerror(errno));
        } else if (limit && lines >= limit) {
            stopped = true;
            n = fprintf(out, "# iotrace: limit of %" PRIu64 " lines reached\n",
                        limit);
            if (n < 0) {
                fprintf(stderr, "iotrace: write failed: %s\n", strerror(errno));
            }
        }
    }
    g_mutex_unlock(&lock);
}

static void vcpu_tb_trans(struct qemu_plugin_tb *tb, void *udata)
{
    size_t n = qemu_plugin_tb_n_insns(tb);

    for (size_t i = 0; i < n; i++) {
        struct qemu_plugin_insn *insn = qemu_plugin_tb_get_insn(tb, i);
        uintptr_t pc = qemu_plugin_insn_vaddr(insn);

        qemu_plugin_register_vcpu_mem_cb(insn, vcpu_mem, QEMU_PLUGIN_CB_NO_REGS,
                                         QEMU_PLUGIN_MEM_RW, (void *)pc);
    }
}

static void plugin_exit(void *p)
{
    /* A buffered write can fail at the flush, which fclose() may not report. */
    if (fflush(out) != 0 || ferror(out)) {
        fprintf(stderr, "iotrace: write failed: %s\n", strerror(errno));
    }
    if (fclose(out) != 0) {
        fprintf(stderr, "iotrace: close failed: %s\n", strerror(errno));
    }
    g_array_free(ranges, TRUE);
}

/* Parse one unsigned number that takes the whole string. */
static bool parse_u64(const char *s, uint64_t *v)
{
    char *end;

    if (!s || !*s || *s == '-' || *s == '+' || g_ascii_isspace(*s)) {
        return false;
    }
    errno = 0;
    *v = strtoull(s, &end, 0);
    return errno == 0 && *end == '\0';
}

static bool parse_range(const char *s, Range *r)
{
    g_auto(GStrv) lh = g_strsplit(s, "-", 2);

    if (!lh[0] || !lh[1] || !parse_u64(lh[0], &r->lo) ||
        !parse_u64(lh[1], &r->hi) || r->lo >= r->hi) {
        return false;
    }
    return true;
}

QEMU_PLUGIN_EXPORT int qemu_plugin_install(qemu_plugin_id_t id,
                                           const qemu_info_t *info, int argc,
                                           char **argv)
{
    g_autofree char *path = g_strdup("/tmp/iotrace.txt");

    ranges = g_array_new(FALSE, FALSE, sizeof(Range));
    for (int i = 0; i < argc; i++) {
        g_auto(GStrv) kv = g_strsplit(argv[i], "=", 2);
        const char *key = kv[0];
        const char *value = kv[0] ? kv[1] : NULL;
        Range r;

        if (key && value && !strcmp(key, "range")) {
            if (!parse_range(value, &r)) {
                fprintf(stderr, "iotrace: bad range '%s', want LO-HI with "
                        "LO < HI\n", value);
                goto fail;
            }
            g_array_append_val(ranges, r);
        } else if (key && value && !strcmp(key, "out")) {
            g_free(path);
            path = g_strdup(value);
        } else if (key && value && !strcmp(key, "limit")) {
            if (!parse_u64(value, &limit)) {
                fprintf(stderr, "iotrace: bad limit '%s'\n", value);
                goto fail;
            }
        } else {
            fprintf(stderr, "iotrace: unknown option '%s'\n", argv[i]);
            goto fail;
        }
    }
    if (!ranges->len) {
        fprintf(stderr, "iotrace: at least one range=LO-HI is required\n");
        goto fail;
    }
    out = fopen(path, "w");
    if (!out) {
        fprintf(stderr, "iotrace: cannot write %s: %s\n", path,
                strerror(errno));
        goto fail;
    }
    qemu_plugin_register_vcpu_tb_trans_cb(id, vcpu_tb_trans, NULL);
    qemu_plugin_register_atexit_cb(id, plugin_exit, NULL);
    return 0;

fail:
    g_array_free(ranges, TRUE);
    ranges = NULL;
    return -1;
}
