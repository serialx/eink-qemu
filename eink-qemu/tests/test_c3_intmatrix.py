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
typedef struct { bool level; unsigned calls; } Line;
typedef Line *qemu_irq;
static int ctz64(uint64_t n) { return __builtin_ctzll(n); }
static void qemu_irq_raise(qemu_irq line) { line->level = true; line->calls++; }
static void qemu_irq_lower(qemu_irq line) { line->level = false; line->calls++; }
static void riscv_csr_write_i64(int *env, int csr, uint64_t value) {}
'''

CASES = r'''
static Line lines[32];
static EspRISCVCPU cpu;
static ESP32C3IntMatrixState s;

static void write_reg(unsigned offset, uint32_t value)
{
    esp32c3_intmatrix_write(&s, offset, value, 4);
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
        bool active = false;
        for (unsigned input = 0; input < 62; input++) {
            if (s.irq_map[input] == line && (s.irq_levels & BIT_ULL(input))) {
                active = true;
            }
        }
        bool expected = active && (s.irq_enabled & BIT(line)) &&
                        s.irq_prio[line] >= s.irq_thres;
        assert(lines[line].level == expected);
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

int main(void)
{
    s.cpu = &cpu;
    for (unsigned i = 0; i < 32; i++) {
        s.out_irqs[i] = &lines[i];
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
            write_reg(0x118 + 4 * (value % 30), value >> 8);
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
