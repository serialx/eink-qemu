"""Exercise the actual C3 matrix with counted CPU-line callbacks."""
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
#include <string.h>
#define ESP_CPU_INT_LINES 31
#define BIT(n) (1U << (n))
#define BIT_ULL(n) (1ULL << (n))
#define OBJECT_CHECK(type, obj, name) ((type *)(obj))
#define CSR_MIE 0
typedef int SysBusDevice;
typedef int MemoryRegion;
typedef void Object;
typedef int ResetType;
typedef uint64_t hwaddr;
typedef struct { int env; } RISCVCPU;
typedef struct { RISCVCPU parent_obj; } EspRISCVCPU;
typedef struct { bool level; unsigned calls; unsigned id; } Line;
typedef Line *qemu_irq;
static int ctz64(uint64_t n) { return __builtin_ctzll(n); }
static int ctz32(uint32_t n) { return __builtin_ctz(n); }
static bool record_events;
static unsigned events[32], event_count;
static void notify(qemu_irq line, bool level)
{
    line->level = level;
    line->calls++;
    if (record_events) {
        assert(event_count < 32);
        events[event_count++] = line->id * 2 + level;
    }
}
static void qemu_irq_raise(qemu_irq line) { notify(line, true); }
static void qemu_irq_lower(qemu_irq line) { notify(line, false); }
static void riscv_csr_write_i64(int *env, int csr, uint64_t value) {}
'''

CASES = r'''
static Line lines[32];
static EspRISCVCPU cpu;
static ESP32C3IntMatrixState s;

/* Deliberately scan all sources and lines, independently of the fast path. */
static bool expected_level(unsigned line, unsigned threshold)
{
    bool active = false;
    for (unsigned input = 0; input < 62; input++) {
        if (s.irq_map[input] == line && (s.irq_levels & BIT_ULL(input))) {
            active = true;
        }
    }
    return active && (s.irq_enabled & BIT(line)) &&
           s.irq_prio[line] >= threshold;
}

static void write_reg(unsigned offset, uint32_t value)
{
    unsigned expected[32], count = 0;
    if (offset == 0x194) {
        for (unsigned line = 1; line < 32; line++) {
            bool level = expected_level(line, value & 15);
            if (lines[line].level != level) {
                expected[count++] = line * 2 + level;
            }
        }
        event_count = 0;
        record_events = true;
    }
    esp32c3_intmatrix_write(&s, offset, value, 4);
    if (offset == 0x194) {
        record_events = false;
        assert(event_count == count);
        assert(!memcmp(events, expected, count * sizeof(*events)));
    }
}

static unsigned notifications(void)
{
    unsigned total = 0;
    for (unsigned i = 1; i < 32; i++) {
        total += lines[i].calls;
    }
    return total;
}

static void check_outputs(void)
{
    for (unsigned line = 1; line < 32; line++) {
        assert(lines[line].level == expected_level(line, s.irq_thres));
    }
}

static uint32_t random_value(void)
{
    static uint32_t seed = 0x12345678;
    seed ^= seed << 13;
    seed ^= seed >> 17;
    seed ^= seed << 5;
    return seed;
}

static void check_all_threshold_pairs(void)
{
    for (unsigned old = 0; old < 16; old++) {
        for (unsigned next = 0; next < 16; next++) {
            write_reg(0x194, old);
            write_reg(0x194, next);
            check_outputs();
            /* Repeating the same predicate must produce no callbacks. */
            write_reg(0x194, next | 0xfffffff0u);
            assert(event_count == 0);
        }
    }
}

static void exhaustive_thresholds(void)
{
    /* Every priority, threshold pair, enable state and shared-source state. */
    for (unsigned priority = 0; priority < 16; priority++) {
        for (unsigned enabled = 0; enabled < 2; enabled++) {
            for (unsigned pending = 0; pending < 4; pending++) {
                esp32c3_intmatrix_reset_hold(&s, 0);
                write_reg(0, 5);
                write_reg(61 * 4, 5);
                write_reg(0x118 + 4 * (5 - 1), priority);
                write_reg(0x104, enabled ? BIT(5) : 0);
                esp32c3_intmatrix_irq_handler(&s, 0, pending & 1);
                esp32c3_intmatrix_irq_handler(&s, 61, pending >> 1);
                check_all_threshold_pairs();
            }
        }
    }
    /* Mixed simultaneous transitions must retain ascending callback order. */
    const uint32_t masks[] = {0, 0xffffffffu, 0xaaaaaaaau, 0x55555555u,
                              BIT(31) | BIT(0)};
    for (unsigned seed = 0; seed < 4; seed++) {
        for (unsigned mask = 0; mask < sizeof(masks) / sizeof(*masks); mask++) {
            esp32c3_intmatrix_reset_hold(&s, 0);
            for (unsigned line = 1; line < 32; line++) {
                write_reg((line - 1) * 4, line);
                write_reg(0x118 + 4 * (line - 1), (line * 7 + seed) & 15);
                esp32c3_intmatrix_irq_handler(&s, line - 1,
                                              seed < 2 || (line & 1));
            }
            /* Also exercise two peripheral sources sharing line 31. */
            write_reg(61 * 4, 31);
            esp32c3_intmatrix_irq_handler(&s, 61, 1);
            write_reg(0x104, masks[mask]);
            check_all_threshold_pairs();
        }
    }
    esp32c3_intmatrix_reset_hold(&s, 0);
}

static void check_peripheral_status(void)
{
    /* Status reports raw sources even while unmapped, disabled or masked. */
    esp32c3_intmatrix_reset_hold(&s, 0);
    esp32c3_intmatrix_irq_handler(&s, 0, 1);
    esp32c3_intmatrix_irq_handler(&s, 31, 1);
    esp32c3_intmatrix_irq_handler(&s, 32, 1);
    esp32c3_intmatrix_irq_handler(&s, 61, 1);
    assert(esp32c3_intmatrix_read(&s, 0xf8, 4) == 0x80000001u);
    assert(esp32c3_intmatrix_read(&s, 0xfc, 4) == 0x20000001u);
    write_reg(0xf8, 0);
    write_reg(0xfc, 0xffffffffu);
    assert(esp32c3_intmatrix_read(&s, 0xf8, 4) == 0x80000001u);
    assert(esp32c3_intmatrix_read(&s, 0xfc, 4) == 0x20000001u);

    /* esp-hal routes GDMA to CPU interrupt 31 at priority 15. */
    write_reg(61 * 4, 31);
    write_reg(0x190, 0xff);
    assert(esp32c3_intmatrix_read(&s, 0x190, 4) == 15);
    write_reg(0x104, BIT(31));
    write_reg(0x194, 15);
    assert(lines[31].level);
    write_reg(0x190, 14);
    assert(!lines[31].level);
    assert(esp32c3_intmatrix_read(&s, 0xfc, 4) == 0x20000001u);
    write_reg(0x190, 15);
    assert(lines[31].level);
    esp32c3_intmatrix_irq_handler(&s, 61, 0);
    assert(!lines[31].level);
    assert(esp32c3_intmatrix_read(&s, 0xfc, 4) == 1);
    esp32c3_intmatrix_irq_handler(&s, 31, 0);
    assert(esp32c3_intmatrix_read(&s, 0xf8, 4) == 1);
    esp32c3_intmatrix_reset_hold(&s, 0);
    assert(esp32c3_intmatrix_read(&s, 0xf8, 4) == 0);
    assert(esp32c3_intmatrix_read(&s, 0xfc, 4) == 0);
    assert(esp32c3_intmatrix_read(&s, 0x190, 4) == 0);
}

int main(void)
{
    s.cpu = &cpu;
    for (unsigned i = 0; i < 32; i++) {
        s.out_irqs[i] = &lines[i];
        lines[i].id = i;
    }
    esp32c3_intmatrix_reset_hold(&s, 0);
    write_reg(21 * 4, 1);
    write_reg(22 * 4, 1);
    write_reg(0x118, 3);
    write_reg(0x11c, 3);
    write_reg(0x104, BIT(1) | BIT(2));
    esp32c3_intmatrix_irq_handler(&s, 21, 1);
    unsigned before = notifications();
    esp32c3_intmatrix_irq_handler(&s, 22, 1);
    esp32c3_intmatrix_irq_handler(&s, 21, 0);
    assert(lines[1].level); /* A shared source still holds the line. */
    write_reg(0x194, 2);
    write_reg(0x118, 3);
    assert(notifications() == before); /* No redundant CPU callbacks. */

    write_reg(22 * 4, 2);
    assert(!lines[1].level && lines[2].level);
    assert(notifications() == before + 2);
    write_reg(0x194, 4);
    assert(!lines[2].level);
    write_reg(0x194, 3);
    assert(lines[2].level);
    write_reg(0x104, 0);
    assert(!lines[2].level);
    write_reg(0x104, BIT(2));
    assert(lines[2].level);
    esp32c3_intmatrix_reset_hold(&s, 0);
    check_outputs();

    exhaustive_thresholds();
    check_peripheral_status();

    for (unsigned i = 0; i < 50000; i++) {
        uint32_t value = random_value();
        switch (random_value() % 6) {
        case 0:
            esp32c3_intmatrix_irq_handler(&s, value % 62, (value >> 8) & 1);
            break;
        case 1:
            write_reg(4 * (value % 62), value >> 8);
            break;
        case 2:
            write_reg(0x118 + 4 * (value % 31), value >> 8);
            break;
        case 3:
            write_reg(0x194, value);
            break;
        case 4:
            write_reg(0x104, value);
            break;
        case 5:
            if (i % 100 == 0) {
                esp32c3_intmatrix_reset_hold(&s, 0);
            }
            break;
        }
        check_outputs();
    }
    return 0;
}
'''


@unittest.skipUnless(CC, 'requires a C compiler')
class C3InterruptMatrix(unittest.TestCase):
    def test_shared_routing_thresholds_reset_and_notifications(self):
        header = (ROOT / 'include/hw/riscv/esp32c3_intmatrix.h').read_text()
        header = header[header.index('#define ESP32C3_INT_MATRIX_INPUTS'):
                        header.index('typedef enum')]
        source = (ROOT / 'hw/riscv/esp32c3_intmatrix.c').read_text()
        functions = source[source.index('#define INTMATRIX_DEBUG'):
                           source.index('static const MemoryRegionOps')]
        reset = source[source.index('static void esp32c3_intmatrix_reset_hold'):
                       source.index('static void esp32c3_intmatrix_realize')]
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory)
            fixture = path / 'matrix.c'
            fixture.write_text(PRELUDE + header + functions + reset + CASES)
            binary = path / 'matrix'
            subprocess.run([CC, '-std=c11', '-O2', '-fsanitize=undefined',
                            '-fno-sanitize-recover=all', str(fixture), '-o', str(binary)],
                           check=True, capture_output=True)
            result = subprocess.run([str(binary)], capture_output=True)
            self.assertEqual(result.returncode, 0, result.stderr.decode())


if __name__ == '__main__':
    unittest.main()
