"""Check the actual Wasm live-word shortcut's rejection and I/O boundaries."""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
CC = shutil.which(os.environ.get('CC', 'cc'))

PRELUDE = r'''
#include <assert.h>
#include <stdbool.h>
#include <stdint.h>
#include <stddef.h>
#include <setjmp.h>
typedef uint64_t vaddr;
typedef uint64_t hwaddr;
typedef unsigned MemOp;
typedef unsigned MemOpIdx;
#define MMU_DATA_LOAD 0
#define MO_BSWAP 16
#define MO_AMASK 224
#define MO_ALIGN 224
#define MO_ALIGN_TLB_ONLY 256
#define TARGET_PAGE_MASK (~(vaddr)4095)
#define TLB_FORCE_SLOW 256
#define TLB_MMIO 16
#define qatomic_read(p) (*(p))
typedef struct MemoryRegion {
    bool lockless_io, disable_reentrancy_guard, flush_coalesced_mmio;
    const uint32_t *readonly_word;
} MemoryRegion;
typedef struct { MemoryRegion *mr; } MemoryRegionSection;
typedef struct {
    MemoryRegionSection *section;
    hwaddr xlat_offset;
    MemoryRegion *readonly_word_mr;
    vaddr readonly_word_addr;
    struct { bool memory, debug; } attrs;
    unsigned slow_flags[3];
} CPUTLBEntryFull;
typedef struct { uintptr_t addr_read; } CPUTLBEntry;
typedef struct {
    uintptr_t mem_io_pc;
    struct {
        bool can_do_io;
        struct { struct { CPUTLBEntryFull *fulltlb; } d[32]; } tlb;
    } neg;
    CPUTLBEntry entry;
} CPUState;
static bool plugin, trace;
static jmp_buf recompile;
static unsigned io_exits;
static unsigned get_mmuidx(MemOpIdx oi) { return oi & 31; }
static MemOp get_memop(MemOpIdx oi) { return oi >> 5; }
static unsigned memop_alignment_bits(MemOp op) {
    if (op & MO_ALIGN_TLB_ONLY) { return 0; }
    return (op & MO_AMASK) == MO_ALIGN ? op & 7 : (op & MO_AMASK) >> 5;
}
static size_t tlb_index(CPUState *cpu, unsigned mmu, vaddr addr) { return 0; }
static CPUTLBEntry *tlb_entry(CPUState *cpu, unsigned mmu, vaddr addr) {
    return &cpu->entry;
}
static bool cpu_plugin_mem_cbs_enabled(CPUState *cpu) { return plugin; }
#define TRACE_MEMORY_REGION_OPS_READ 0
static bool trace_event_get_state_backends(int event) { return trace; }
static void cpu_io_recompile(CPUState *cpu, uintptr_t ra) {
    io_exits++;
    longjmp(recompile, 1);
}
'''

CASES = r'''
int main(void)
{
    uint32_t word = 0x12345678, next_word = 0xaabbccdd, result = 0;
    MemoryRegion mr = { true, true, false, &word };
    MemoryRegion replacement = { true, true, false, &next_word };
    MemoryRegionSection section = { &mr };
    CPUTLBEntryFull full = {
        .section = &section, .readonly_word_mr = &mr,
        .readonly_word_addr = 0x60024000, .slow_flags = { TLB_MMIO },
    };
    static CPUState cpu = { .neg.can_do_io = true,
                    .entry.addr_read = 0x60024000 | TLB_FORCE_SLOW };
    cpu.neg.tlb.d[0].fulltlb = &full;
    const vaddr addr = 0x60024000;
#define READ(a, op) do_ld4_mmu_readonly_word(&cpu, (a), (op) << 5, 0xbeef, &result)
#define HIT(value) do { assert(READ(addr, 2)); assert(result == (value)); } while (0)
#define MISS() do { result = 0xfeed; cpu.mem_io_pc = 0; assert(!READ(addr, 2)); \
                    assert(result == 0xfeed); assert(!cpu.mem_io_pc); } while (0)
    HIT(word);
    assert(cpu.mem_io_pc == 0xbeef);
    word = 0x89abcdef;
    HIT(word); /* Cache holds the live source, never the previous value. */
    assert(!READ(addr + 1, 2));
    assert(!READ(addr + 4, 2));
    assert(!READ(addr, 2 | MO_BSWAP));
    full.readonly_word_addr = addr + 4;
    assert(!READ(addr + 4, 2 | (3 << 5))); /* Requested 8-byte alignment. */
    full.readonly_word_addr = addr;
    cpu.entry.addr_read = -1; MISS(); /* TLB flush. */
    cpu.entry.addr_read = addr + 4096 + TLB_FORCE_SLOW; MISS();
    cpu.entry.addr_read = addr | TLB_FORCE_SLOW | 64; MISS(); /* Invalid. */
    cpu.entry.addr_read = addr; MISS(); /* RAM, not MMIO. */
    cpu.entry.addr_read = addr | TLB_FORCE_SLOW;
    for (unsigned bit = 1; bit < 16; bit <<= 1) {
        full.slow_flags[0] = TLB_MMIO | bit; MISS();
    }
    full.slow_flags[0] = TLB_MMIO;
    full.readonly_word_mr = NULL; MISS();
    full.readonly_word_mr = &mr;
    full.attrs.memory = true; MISS(); full.attrs.memory = false;
    full.attrs.debug = true; MISS(); full.attrs.debug = false;
    plugin = true; MISS(); plugin = false;
    trace = true; MISS(); trace = false;
    mr.lockless_io = false; MISS(); mr.lockless_io = true;
    mr.disable_reentrancy_guard = false; MISS(); mr.disable_reentrancy_guard = true;
    mr.flush_coalesced_mmio = true; MISS(); mr.flush_coalesced_mmio = false;
    /* A replacement mapping starts uncached and then uses its own source. */
    section.mr = &replacement;
    full.readonly_word_mr = NULL; MISS();
    full.readonly_word_mr = &replacement;
    HIT(next_word);
    /* A hit inside an unbounded icount TB must exit before touching the word. */
    replacement.readonly_word = NULL;
    cpu.neg.can_do_io = false;
    if (!setjmp(recompile)) {
        READ(addr, 2);
        assert(!"missing I/O recompile");
    }
    assert(io_exits == 1 && cpu.mem_io_pc == 0xbeef);
    return 0;
}
'''


def function(source, signature):
    start = source.index(signature)
    opening = source.index('{', start)
    depth = 1
    end = opening + 1
    while depth:
        depth += (source[end] == '{') - (source[end] == '}')
        end += 1
    return source[start:end] + '\n'


@unittest.skipUnless(CC, 'requires a C compiler')
class MMIOWordGuards(unittest.TestCase):
    def test_live_source_invalidation_guards_and_icount_exit(self):
        source = (ROOT / 'accel/tcg/cputlb.c').read_text()
        memory = (ROOT / 'system/memory.c').read_text()
        fixture = PRELUDE + function(memory, 'bool memory_region_readonly_word_enabled(')
        fixture += function(source, 'static MemoryRegionSection *\nio_prepare(')
        fixture += function(source, 'static inline bool do_ld4_mmu_readonly_word(')
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory)
            (path / 'word.c').write_text(fixture + CASES)
            subprocess.run([CC, '-std=c11', '-O2', '-Werror', str(path / 'word.c'),
                            '-o', str(path / 'word')], check=True, capture_output=True)
            subprocess.run([str(path / 'word')], check=True, timeout=10)


if __name__ == '__main__':
    unittest.main()
