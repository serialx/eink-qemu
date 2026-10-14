/*
 * Record every ESP-IDF heap allocation and free with its call stack.
 *
 *   -plugin contrib/plugins/libheaptrace.so,elf=firmware.elf[,out=FILE][,depth=N]
 *
 * Allocations are caught in poison_allocated_region(), which ESP-IDF heap
 * poisoning (the default "light" level) calls with every new block from
 * malloc, calloc and realloc; frees at the entry of multi_heap_free(). A
 * realloc is the allocation poison_allocated_region() makes when called
 * straight from multi_heap_realloc(), whose entry noted the old pointer: a
 * failed realloc leaves the old block alone, as it should.
 *
 * At each event the plugin unwinds the guest stack, from the ELF's
 * .debug_frame and .eh_frame on RISC-V and from the register windows and
 * base save areas on Xtensa, and appends a record to FILE (default
 * /tmp/eink-qemu-heaptrace.bin). The file starts with "HTRC" and u32 version
 * 2; each record is u16 kind (1 alloc, 2 free, 3 realloc), u16 frame count,
 * u32 user pointer, u32 requested size (0 for a free), u32 old pointer (a
 * realloc's source, else 0), u64 host time in microseconds since the epoch,
 * then that many u32 return addresses, all little endian.
 * eink-qemu/heapview.py reads it to show where blocks came from and to
 * replay the heap at any moment.
 *
 * SPDX-License-Identifier: GPL-2.0-or-later
 */
#include <fcntl.h>
#include <stddef.h>
#include <glib.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

#include <qemu-plugin.h>

QEMU_PLUGIN_EXPORT int qemu_plugin_version = QEMU_PLUGIN_VERSION;

#define MAX_DEPTH 64
#define MAX_VCPUS 16

enum { EV_ALLOC = 1, EV_FREE = 2, EV_REALLOC = 3 };

typedef struct {
    uint16_t kind, n;
    uint32_t ptr, size, old;
    uint64_t time;
    uint32_t frames[MAX_DEPTH];
} Record;
/* Naturally aligned, so the in-memory layout is the file layout. */
G_STATIC_ASSERT(offsetof(Record, time) == 16 && offsetof(Record, frames) == 24);

static bool xtensa;
static uint64_t alloc_vaddr, free_vaddr, realloc_vaddr;
static uint32_t realloc_lo, realloc_hi;
/* The pointer each vCPU's multi_heap_realloc() is moving, if any. */
static uint32_t realloc_old[MAX_VCPUS];
static unsigned depth = 24;
static int out_fd = -1;
static GMutex out_lock;

/* Register handles are the same for every vCPU. */
static struct qemu_plugin_register *rv_a0, *rv_a1, *rv_a2, *rv_sp, *rv_ra, *rv_s0;
static struct qemu_plugin_register *xt_a[5], *xt_ar[64], *xt_wb, *xt_ws;
static unsigned nareg;
static bool regs_found;

/* ELF */

static gchar *elf;
static gsize elf_len;

static uint32_t rd32(const uint8_t *p)
{
    return p[0] | p[1] << 8 | p[2] << 16 | (uint32_t)p[3] << 24;
}

static uint16_t rd16(const uint8_t *p)
{
    return p[0] | p[1] << 8;
}

static const uint8_t *section(const char *want, uint32_t *size, uint32_t *addr)
{
    const uint8_t *e = (const uint8_t *)elf;
    uint32_t shoff = rd32(e + 0x20);
    unsigned shentsize = rd16(e + 0x2e), shnum = rd16(e + 0x30);
    const uint8_t *shstr = e + shoff + rd16(e + 0x32) * shentsize;

    for (unsigned i = 0; i < shnum; i++) {
        const uint8_t *sh = e + shoff + i * shentsize;
        const char *name = elf + rd32(shstr + 16) + rd32(sh);
        if (!strcmp(name, want)) {
            *size = rd32(sh + 20);
            if (addr) {
                *addr = rd32(sh + 12);
            }
            return e + rd32(sh + 16);
        }
    }
    return NULL;
}

static uint32_t symbol(const char *want, uint32_t *size)
{
    uint32_t symsz, strsz;
    const uint8_t *sym = section(".symtab", &symsz, NULL);
    const uint8_t *str = section(".strtab", &strsz, NULL);

    for (uint32_t off = 0; sym && str && off + 16 <= symsz; off += 16) {
        if (!strcmp((const char *)str + rd32(sym + off), want)) {
            if (size) {
                *size = rd32(sym + off + 8);
            }
            return rd32(sym + off + 4);
        }
    }
    return 0;
}

/*
 * RISC-V call frame information: .debug_frame for code built with -g, and
 * .eh_frame for prebuilt C++ runtime code such as operator new.
 */

typedef struct {
    uint32_t lo, hi;
    const uint8_t *cie_insns, *cie_end, *insns, *end;
    uint32_t code_align;
    int32_t data_align;
} Fde;

static GArray *fdes;

enum { RULE_SAME, RULE_OFFSET, RULE_UNDEF };
#define NREGS 33

typedef struct {
    int cfa_reg;
    int32_t cfa_off;
    uint8_t rule[NREGS];
    int32_t off[NREGS];
} Row;

static uint32_t uleb(const uint8_t **p)
{
    uint32_t v = 0;
    unsigned shift = 0;
    uint8_t b;

    do {
        b = *(*p)++;
        v |= (uint32_t)(b & 0x7f) << shift;
        shift += 7;
    } while (b & 0x80);
    return v;
}

static int32_t sleb(const uint8_t **p)
{
    int32_t v = 0;
    unsigned shift = 0;
    uint8_t b;

    do {
        b = *(*p)++;
        v |= (int32_t)(b & 0x7f) << shift;
        shift += 7;
    } while (b & 0x80);
    if (shift < 32 && (b & 0x40)) {
        v |= -(1 << shift);
    }
    return v;
}

static int fde_cmp(gconstpointer a, gconstpointer b)
{
    const Fde *x = a, *y = b;
    return x->lo < y->lo ? -1 : x->lo > y->lo;
}

/* A pointer in .eh_frame encoding; only the 4-byte forms GCC emits here. */
static uint32_t read_enc(const uint8_t **p, uint8_t enc, uint32_t field_addr)
{
    uint32_t v;

    if ((enc & 0x0f) == 0x02 || (enc & 0x0f) == 0x0a) {
        v = rd16(*p);
        *p += 2;
    } else {
        v = rd32(*p);
        *p += 4;
    }
    return (enc & 0x70) == 0x10 ? v + field_addr : v;
}

static bool parse_cie(const uint8_t *cie, bool eh, uint32_t sec_addr,
                      const uint8_t *base, Fde *f, uint8_t *fde_enc, bool *aug_z)
{
    const uint8_t *aug = cie + 9, *c;
    uint8_t version = cie[8];

    f->cie_end = cie + 4 + rd32(cie);
    c = aug + strlen((const char *)aug) + 1;
    if (version >= 4) {
        c += 2;                              /* address and segment size */
    }
    f->code_align = uleb(&c);
    f->data_align = sleb(&c);
    if (version == 1) {
        c++;
    } else {
        uleb(&c);
    }
    *fde_enc = 0;
    *aug_z = aug[0] == 'z';
    if (*aug_z) {
        uint32_t len = uleb(&c);
        const uint8_t *data = c;
        for (const char *a = (const char *)aug + 1; *a; a++) {
            if (*a == 'R') {
                *fde_enc = *c++;
            } else if (*a == 'L') {
                c++;
            } else if (*a == 'P') {
                uint8_t enc = *c++;
                read_enc(&c, enc, sec_addr + (c - base));
            } else {
                return false;
            }
        }
        c = data + len;
    } else if (aug[0]) {
        return false;
    }
    f->cie_insns = c;
    return true;
}

static void load_frames(const char *name, bool eh)
{
    uint32_t size, addr;
    const uint8_t *base = section(name, &size, &addr);

    for (const uint8_t *p = base; p && p + 8 <= base + size;) {
        uint32_t len = rd32(p), id = rd32(p + 4);
        const uint8_t *end = p + 4 + len, *q = p + 8;
        Fde f = { .end = end };
        uint8_t enc;
        bool aug_z;

        if (len == 0 || len == 0xffffffff || end > base + size) {
            break;
        }
        if (id != (eh ? 0 : 0xffffffff) &&
            parse_cie(eh ? p + 4 - id : base + id, eh, addr, base, &f, &enc, &aug_z)) {
            f.lo = read_enc(&q, enc, addr + (q - base));
            f.hi = f.lo + read_enc(&q, enc & 0x0f, 0);
            if (aug_z) {
                uint32_t skip = uleb(&q);
                q += skip;
            }
            f.insns = q;
            if (f.hi > f.lo) {
                g_array_append_val(fdes, f);
            }
        }
        p = end;
    }
}

static bool load_cfi(void)
{
    fdes = g_array_new(false, false, sizeof(Fde));
    load_frames(".debug_frame", false);
    load_frames(".eh_frame", true);
    g_array_sort(fdes, fde_cmp);
    return fdes->len > 0;
}

static const Fde *find_fde(uint32_t pc)
{
    int lo = 0, hi = fdes->len - 1;

    while (lo <= hi) {
        int mid = (lo + hi) / 2;
        const Fde *f = &g_array_index(fdes, Fde, mid);
        if (pc < f->lo) {
            hi = mid - 1;
        } else if (pc >= f->hi) {
            lo = mid + 1;
        } else {
            return f;
        }
    }
    return NULL;
}

/* Run CFA instructions until the row covering pc; false if unsupported. */
static bool run_cfa(const Fde *f, const uint8_t *p, const uint8_t *end,
                    uint32_t pc, uint32_t *loc, Row *row, const Row *init)
{
    Row stack[8];
    unsigned sp = 0;

    while (p < end) {
        uint8_t op = *p++, low = op & 0x3f;
        uint32_t reg, delta = 0;

        switch (op >> 6) {
        case 1:
            delta = low;
            break;
        case 2:
            if (low < NREGS) {
                row->rule[low] = RULE_OFFSET;
                row->off[low] = uleb(&p) * f->data_align;
            } else {
                uleb(&p);
            }
            continue;
        case 3:
            if (low < NREGS) {
                row->rule[low] = init->rule[low];
                row->off[low] = init->off[low];
            }
            continue;
        default:
            switch (op) {
            case 0x00:                       /* nop */
                continue;
            case 0x01:                       /* set_loc */
                *loc = rd32(p);
                p += 4;
                if (*loc > pc) {
                    return true;
                }
                continue;
            case 0x02:
                delta = *p++;
                break;
            case 0x03:
                delta = rd16(p);
                p += 2;
                break;
            case 0x04:
                delta = rd32(p);
                p += 4;
                break;
            case 0x05:                       /* offset_extended */
            case 0x11:                       /* offset_extended_sf */
                reg = uleb(&p);
                {
                    int32_t off = op == 0x05 ? (int32_t)uleb(&p) : sleb(&p);
                    if (reg < NREGS) {
                        row->rule[reg] = RULE_OFFSET;
                        row->off[reg] = off * f->data_align;
                    }
                }
                continue;
            case 0x06:                       /* restore_extended */
                reg = uleb(&p);
                if (reg < NREGS) {
                    row->rule[reg] = init->rule[reg];
                    row->off[reg] = init->off[reg];
                }
                continue;
            case 0x07:                       /* undefined */
            case 0x08:                       /* same_value */
                reg = uleb(&p);
                if (reg < NREGS) {
                    row->rule[reg] = op == 0x07 ? RULE_UNDEF : RULE_SAME;
                }
                continue;
            case 0x0a:                       /* remember_state */
                if (sp == G_N_ELEMENTS(stack)) {
                    return false;
                }
                stack[sp++] = *row;
                continue;
            case 0x0b:                       /* restore_state */
                if (sp == 0) {
                    return false;
                }
                *row = stack[--sp];          /* GCC expects the CFA restored too */
                continue;
            case 0x0c:                       /* def_cfa */
                row->cfa_reg = uleb(&p);
                row->cfa_off = uleb(&p);
                continue;
            case 0x0d:                       /* def_cfa_register */
                row->cfa_reg = uleb(&p);
                continue;
            case 0x0e:                       /* def_cfa_offset */
                row->cfa_off = uleb(&p);
                continue;
            case 0x12:                       /* def_cfa_sf */
                row->cfa_reg = uleb(&p);
                row->cfa_off = sleb(&p) * f->data_align;
                continue;
            case 0x13:                       /* def_cfa_offset_sf */
                row->cfa_off = sleb(&p) * f->data_align;
                continue;
            case 0x2e:                       /* GNU_args_size */
                uleb(&p);
                continue;
            default:                         /* expressions etc. */
                return false;
            }
        }
        *loc += delta * f->code_align;
        if (*loc > pc) {
            return true;
        }
    }
    return true;
}

/* Guest access from the hook callback */

static __thread GByteArray *scratch;

static uint32_t reg32(struct qemu_plugin_register *r)
{
    g_byte_array_set_size(scratch, 0);
    if (!r || !qemu_plugin_read_register(r, scratch) || scratch->len < 4) {
        return 0;
    }
    return rd32(scratch->data);
}

static bool mem32(uint32_t addr, uint32_t *val)
{
    g_byte_array_set_size(scratch, 0);
    if (addr & 3 || !qemu_plugin_read_memory_vaddr(addr, scratch, 4)) {
        return false;
    }
    *val = rd32(scratch->data);
    return true;
}

static unsigned unwind_riscv(uint32_t pc, uint32_t *frames)
{
    uint32_t sp = reg32(rv_sp), ra = reg32(rv_ra);
    uint32_t s0 = reg32(rv_s0), lookup = pc;
    unsigned n = 0;

    while (n < depth) {
        const Fde *f = find_fde(lookup);
        Row init = { .cfa_reg = 2 }, row;
        uint32_t loc, cfa, next_ra = ra, next_s0 = s0;

        if (!f) {
            break;
        }
        loc = f->lo;
        if (!run_cfa(f, f->cie_insns, f->cie_end, UINT32_MAX, &loc, &init, &init)) {
            break;
        }
        row = init;
        loc = f->lo;
        if (!run_cfa(f, f->insns, f->end, lookup, &loc, &row, &init)) {
            break;
        }
        if (row.cfa_reg != 2 && row.cfa_reg != 8) {
            break;
        }
        cfa = (row.cfa_reg == 2 ? sp : s0) + row.cfa_off;
        if (row.rule[1] == RULE_UNDEF) {
            break;                           /* outermost frame */
        }
        if (row.rule[1] == RULE_OFFSET) {
            if (!mem32(cfa + row.off[1], &next_ra)) {
                break;
            }
        } else if (n > 0) {
            break;                           /* only the hooked frame keeps ra live */
        }
        if (row.rule[8] == RULE_OFFSET && !mem32(cfa + row.off[8], &next_s0)) {
            break;
        }
        if (!next_ra || (n > 0 && cfa <= sp)) {
            break;
        }
        frames[n++] = next_ra;
        pc = next_ra;
        lookup = pc - 1;                     /* inside the call instruction */
        sp = cfa;
        s0 = next_s0;
        ra = 0;
    }
    return n;
}

static unsigned unwind_xtensa(uint32_t pc, uint32_t *frames)
{
    uint32_t wb = reg32(xt_wb), ws = reg32(xt_ws);
    uint32_t a0 = reg32(xt_a[0]), sp = reg32(xt_a[1]);
    unsigned windows = nareg / 4, cwb = wb, n = 0;
    bool live = true;

    while (n < depth) {
        unsigned callinc = a0 >> 30, caller;
        uint32_t ret = (a0 & 0x3fffffff) | (pc & 0xc0000000), next_sp;

        if (!callinc || !sp) {
            break;                           /* call0, exception or task entry */
        }
        frames[n++] = ret;
        caller = (cwb + windows - callinc) % windows;
        if (live && ws >> caller & 1) {
            /* The caller's window is still in the register file. */
            a0 = reg32(xt_ar[caller * 4]);
            next_sp = reg32(xt_ar[caller * 4 + 1]);
        } else {
            /* Spilled: its a0/a1 sit in the base save area under our sp. */
            live = false;
            if (!mem32(sp - 16, &a0) || !mem32(sp - 12, &next_sp)) {
                break;
            }
        }
        if (next_sp <= sp) {
            break;
        }
        cwb = caller;
        pc = ret;
        sp = next_sp;
    }
    return n;
}

/* Argument i of the hooked function; on Xtensa they are in a2.. after ENTRY. */
static uint32_t arg(unsigned i)
{
    if (xtensa) {
        return reg32(xt_a[2 + i]);
    }
    return reg32(i == 0 ? rv_a0 : i == 1 ? rv_a1 : rv_a2);
}

static void unwind(Record *r, uint64_t pc)
{
    r->n = xtensa ? unwind_xtensa(pc, r->frames) : unwind_riscv(pc, r->frames);
}

static void emit(Record *r)
{
    /* Stamped under the lock so the file stays in time order across vCPUs. */
    g_mutex_lock(&out_lock);
    r->time = g_get_real_time();
    if (write(out_fd, r, offsetof(Record, frames) + r->n * 4) < 0) {
        /* Nothing useful to do; heapview treats a short file as truncated. */
    }
    g_mutex_unlock(&out_lock);
}

static void on_alloc(unsigned int vcpu_index, void *udata)
{
    Record r = { .kind = EV_ALLOC };
    uint32_t *old = &realloc_old[vcpu_index % MAX_VCPUS];

    if (!scratch) {
        scratch = g_byte_array_new();
    }
    /* poison_allocated_region(poison_head_t *head, size_t alloc_size); the
     * caller gets head + 8. */
    r.ptr = arg(0) + 8;
    r.size = arg(1);
    unwind(&r, alloc_vaddr);
    if (r.n && r.frames[0] >= realloc_lo && r.frames[0] < realloc_hi && *old) {
        r.kind = EV_REALLOC;
        r.old = *old;
        *old = 0;
    }
    emit(&r);
}

static void on_free(unsigned int vcpu_index, void *udata)
{
    Record r = { .kind = EV_FREE };

    if (!scratch) {
        scratch = g_byte_array_new();
    }
    r.ptr = arg(1);                     /* multi_heap_free(heap, p) */
    if (r.ptr) {
        unwind(&r, free_vaddr);
        emit(&r);
    }
}

static void on_realloc(unsigned int vcpu_index, void *udata)
{
    uint32_t p, size;

    if (!scratch) {
        scratch = g_byte_array_new();
    }
    /* multi_heap_realloc(heap, p, size): NULL and 0 become malloc and free. */
    p = arg(1);
    size = arg(2);
    realloc_old[vcpu_index % MAX_VCPUS] = p && size ? p : 0;
}

static void tb_trans(struct qemu_plugin_tb *tb, void *udata)
{
    for (size_t i = 0; i < qemu_plugin_tb_n_insns(tb); i++) {
        struct qemu_plugin_insn *insn = qemu_plugin_tb_get_insn(tb, i);
        uint64_t pc = qemu_plugin_insn_vaddr(insn);
        qemu_plugin_vcpu_udata_cb_t cb = pc == alloc_vaddr ? on_alloc :
                                         pc == free_vaddr ? on_free :
                                         pc == realloc_vaddr ? on_realloc : NULL;
        if (cb) {
            qemu_plugin_register_vcpu_insn_exec_cb(insn, cb,
                                                   QEMU_PLUGIN_CB_R_REGS, NULL);
        }
    }
}

static struct qemu_plugin_register *find_reg(GArray *regs, const char *name)
{
    for (guint i = 0; i < regs->len; i++) {
        qemu_plugin_reg_descriptor *d =
            &g_array_index(regs, qemu_plugin_reg_descriptor, i);
        if (!strcmp(d->name, name)) {
            return d->handle;
        }
    }
    return NULL;
}

static void vcpu_init(unsigned int vcpu_index, void *udata)
{
    g_autoptr(GArray) regs = qemu_plugin_get_registers();

    if (regs_found) {
        return;
    }
    regs_found = true;
    if (xtensa) {
        for (unsigned i = 0; i < 5; i++) {
            g_autofree char *name = g_strdup_printf("a%u", i);
            xt_a[i] = find_reg(regs, name);
        }
        xt_wb = find_reg(regs, "windowbase");
        xt_ws = find_reg(regs, "windowstart");
        for (nareg = 0; nareg < 64; nareg++) {
            g_autofree char *name = g_strdup_printf("ar%u", nareg);
            if (!(xt_ar[nareg] = find_reg(regs, name))) {
                break;
            }
        }
        if (!xt_a[4] || !xt_ws || nareg < 16) {
            qemu_plugin_outs("heaptrace: this QEMU does not expose Xtensa "
                             "window registers to plugins\n");
        }
    } else {
        rv_a0 = find_reg(regs, "a0");
        rv_a1 = find_reg(regs, "a1");
        rv_a2 = find_reg(regs, "a2");
        rv_sp = find_reg(regs, "sp");
        rv_ra = find_reg(regs, "ra");
        rv_s0 = find_reg(regs, "fp");
        if (!rv_sp || !rv_ra) {
            qemu_plugin_outs("heaptrace: missing RISC-V registers\n");
        }
    }
}

QEMU_PLUGIN_EXPORT int qemu_plugin_install(qemu_plugin_id_t id,
                                           const qemu_info_t *info,
                                           int argc, char **argv)
{
    const char *elf_path = NULL, *out = "/tmp/eink-qemu-heaptrace.bin";
    g_autoptr(GError) err = NULL;
    uint32_t hook, free_fn, realloc_fn, realloc_size = 0;
    static const char magic[8] = { 'H', 'T', 'R', 'C', 2, 0, 0, 0 };

    for (int i = 0; i < argc; i++) {
        g_auto(GStrv) kv = g_strsplit(argv[i], "=", 2);
        if (!kv[1]) {
            fprintf(stderr, "heaptrace: bad option %s\n", argv[i]);
            return -1;
        } else if (!strcmp(kv[0], "elf")) {
            elf_path = g_strdup(kv[1]);
        } else if (!strcmp(kv[0], "out")) {
            out = g_strdup(kv[1]);
        } else if (!strcmp(kv[0], "depth")) {
            depth = MIN(MAX(atoi(kv[1]), 1), MAX_DEPTH);
        } else {
            fprintf(stderr, "heaptrace: unknown option %s\n", kv[0]);
            return -1;
        }
    }
    if (!elf_path) {
        fprintf(stderr, "heaptrace: elf=firmware.elf is required\n");
        return -1;
    }
    if (!g_file_get_contents(elf_path, &elf, &elf_len, &err)) {
        fprintf(stderr, "heaptrace: %s\n", err->message);
        return -1;
    }
    if (elf_len < 52 || memcmp(elf, "\x7f" "ELF\x01\x01", 6)) {
        fprintf(stderr, "heaptrace: %s is not a 32-bit little-endian ELF\n", elf_path);
        return -1;
    }
    xtensa = !strcmp(info->target_name, "xtensa");
    hook = symbol("poison_allocated_region", NULL);
    free_fn = symbol("multi_heap_free", NULL);
    realloc_fn = symbol("multi_heap_realloc", &realloc_size);
    if (!hook) {
        fprintf(stderr, "heaptrace: %s has no poison_allocated_region; "
                "the firmware needs CONFIG_HEAP_POISONING_LIGHT or "
                "COMPREHENSIVE\n", elf_path);
        return -1;
    }
    if (!free_fn || !realloc_fn) {
        fprintf(stderr, "heaptrace: %s has no multi_heap_free or "
                "multi_heap_realloc\n", elf_path);
        return -1;
    }
    /* Xtensa: hook after the 3-byte ENTRY, once the window has rotated. */
    alloc_vaddr = xtensa ? hook + 3 : hook;
    free_vaddr = xtensa ? free_fn + 3 : free_fn;
    realloc_vaddr = xtensa ? realloc_fn + 3 : realloc_fn;
    realloc_lo = realloc_fn;
    realloc_hi = realloc_fn + realloc_size;
    if (!xtensa && !load_cfi()) {
        fprintf(stderr, "heaptrace: %s has no .debug_frame or .eh_frame\n", elf_path);
        return -1;
    }
    out_fd = open(out, O_WRONLY | O_CREAT | O_TRUNC | O_APPEND, 0644);
    if (out_fd < 0) {
        perror(out);
        return -1;
    }
    if (write(out_fd, magic, sizeof(magic)) != sizeof(magic)) {
        perror(out);
        return -1;
    }
    qemu_plugin_register_vcpu_init_cb(id, vcpu_init, NULL);
    qemu_plugin_register_vcpu_tb_trans_cb(id, tb_trans, NULL);
    return 0;
}
