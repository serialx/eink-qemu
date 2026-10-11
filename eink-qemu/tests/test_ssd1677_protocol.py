"""Exercise the actual SSD1677 protocol core without firmware or a QEMU build.

Like test_panel_ink, compile the device functions with only console/timer/IRQ
plumbing replaced. Pixel, command, RAM, and serial behavior remain production C.
"""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
PRELUDE = r'''
#include <assert.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
typedef struct { bool cs; } SSIPeripheral;
typedef int QemuConsole;
typedef int QEMUTimer;
typedef int *qemu_irq;
#define QEMU_CLOCK_VIRTUAL 0
#define SSD1677(p) ((Ssd1677State *)(p))
#define ARRAY_SIZE(a) (sizeof(a) / sizeof((a)[0]))
typedef struct Ssd1677State Ssd1677State;
static void qemu_set_irq(qemu_irq irq, int level) { if (irq) *irq = level; }
static int64_t qemu_clock_get_ms(int clock) { return 0; }
static void timer_mod(QEMUTimer *timer, int64_t deadline) {}
static void timer_del(QEMUTimer *timer) {}
static uint8_t revbit8(uint8_t v) {
    uint8_t result = 0;
    for (int i = 0; i < 8; i++) { result = result * 2 + (v & 1); v >>= 1; }
    return result;
}
'''

CASES = r'''
static void command(Ssd1677State *s, uint8_t cmd,
                    const uint8_t *data, size_t count)
{
    ssd1677_set_dc(s, 0, 0);
    ssd1677_transfer(&s->parent_obj, cmd);
    ssd1677_set_dc(s, 0, 1);
    for (size_t i = 0; i < count; i++) {
        ssd1677_transfer(&s->parent_obj, data[i]);
    }
}

static void read_id(void)
{
    Ssd1677State *s = calloc(1, sizeof(*s));
    const uint8_t expected[] = {0xca,0xfe,0x00,0x16,0x77,0x00,0x08,0x01,0x00,0x4e};
    command(s, 0x2e, NULL, 0);
    for (size_t i = 0; i < sizeof(expected); i++) {
        assert(ssd1677_transfer(&s->parent_obj, 0xff) == expected[i]);
    }
    assert(ssd1677_transfer(&s->parent_obj, 0xff) == 0xff);
    command(s, 0x2e, NULL, 0);
    assert(ssd1677_transfer(&s->parent_obj, 0xff) == 0xca);
    free(s);
}

static void chip_select(Ssd1677State *s, bool level)
{
    ssd1677_set_cs(&s->parent_obj, level);
    s->parent_obj.cs = level; /* SSI calls the handler before updating cs. */
}

static void write_bits(Ssd1677State *s, uint8_t value, unsigned count)
{
    for (unsigned i = 0; i < count; i++) {
        ssd1677_set_sclk(s, 0, 0);
        ssd1677_set_sda(s, 0, (value >> (7 - i)) & 1);
        ssd1677_set_sclk(s, 0, 1);
        ssd1677_set_sclk(s, 0, 0);
    }
}

static void bitbang_id(void)
{
    Ssd1677State *s = calloc(1, sizeof(*s));
    int line = 1, busy = 0;
    s->sda_out = &line;
    s->busy = &busy;
    const uint8_t expected[] = {0xca,0xfe,0x00,0x16,0x77,0x00,0x08,0x01,0x00,0x4e};
    chip_select(s, true);
    /* Commands while deselected, and an aborted command, must not disturb
     * the following transaction's byte alignment. */
    write_bits(s, 0x10, 8);
    chip_select(s, false);
    write_bits(s, 0xff, 3);
    chip_select(s, true);
    chip_select(s, false);
    write_bits(s, 0x2e, 8);
    ssd1677_set_dc(s, 0, 1);
    for (size_t i = 0; i < sizeof(expected); i++) {
        uint8_t byte = 0;
        /* Stock firmware reapplies DC=1 and SCLK=0 for every byte. */
        ssd1677_set_dc(s, 0, 1);
        ssd1677_set_sclk(s, 0, 0);
        for (unsigned bit = 0; bit < 8; bit++) {
            int sampled = line;
            byte = byte * 2 + sampled;
            ssd1677_set_sclk(s, 0, 1);
            assert(line == sampled); /* also valid for rising-edge hosts */
            ssd1677_set_sclk(s, 0, 0);
        }
        assert(byte == expected[i]);
    }
    assert(line == 1); /* exhausted response releases SDA */
    chip_select(s, true);
    assert(line == 1);

    chip_select(s, false);
    ssd1677_set_dc(s, 0, 0);
    write_bits(s, 0x2e, 8);
    ssd1677_set_dc(s, 0, 1);
    for (unsigned i = 0; i < 2; i++) {
        ssd1677_set_sclk(s, 0, 1);
        ssd1677_set_sclk(s, 0, 0);
    }
    assert(line == 0); /* third bit of 0xca */
    ssd1677_set_rst(s, 0, 0);
    assert(line == 1);
    write_bits(s, 0xff, 8);
    ssd1677_set_rst(s, 0, 1);
    ssd1677_set_dc(s, 0, 0);
    write_bits(s, 0x2e, 8);
    ssd1677_set_dc(s, 0, 1);
    assert(ssd1677_transfer(&s->parent_obj, 0xff) == 0xca);
    /* Switch from GPIO identification to hardware SPI image writes. */
    command(s, 0x12, NULL, 0);
    command(s, 0x4f, (uint8_t[]){0xdf,1}, 2);
    command(s, 0x24, (uint8_t[]){0x7f}, 1);
    command(s, 0x20, NULL, 0);
    assert(s->shown[0][0] == 0x3f);
    assert(busy == 1);
    ssd1677_set_rst(s, 0, 0);
    assert(busy == 0);
    ssd1677_set_rst(s, 0, 1);
    assert(busy == 0);
    free(s);
}

static void lut_pixels(void)
{
    Ssd1677State *s = calloc(1, sizeof(*s));
    /* Actual waveform rows from Picco's DEPG0397 driver. Timing fields do
     * not affect this controller model, which renders only the final image. */
    const uint8_t mono_full[105] = {
        [0]=0xa0,[1]=0x50,[10]=0x50,[11]=0xa0,
        [20]=0xa0,[21]=0x50,[30]=0x50,[31]=0xa0,
    };
    const uint8_t mono_partial[105] = {[0]=0x04,[10]=0x80,[20]=0x40};
    const uint8_t gray[105] = {
        [0]=0xaa,[2]=0x55,[10]=0x64,[12]=0x98,
        [20]=0xa0,[22]=0x50,[30]=0x55,[32]=0xaa,
    };
    /* Four pixel codes: BW=0101, RED=0011. At RAM row479 they appear
     * at the top of the unrotated image. */
    command(s, 0x12, NULL, 0);
    command(s, 0x4f, (uint8_t[]){0xdf,1}, 2);
    command(s, 0x24, (uint8_t[]){0x50}, 1);
    command(s, 0x4e, (uint8_t[]){0,0}, 2);
    command(s, 0x26, (uint8_t[]){0x30}, 1);
    command(s, 0x32, mono_full, sizeof(mono_full));
    command(s, 0x20, NULL, 0);
    assert(s->shown[0][0] == 0x33);  /* black white black white */
    command(s, 0x32, gray, sizeof(gray));
    command(s, 0x20, NULL, 0);
    assert(s->shown[0][0] == 0x1b);  /* black dark-gray light-gray white */
    command(s, 0x32, mono_partial, sizeof(mono_partial));
    command(s, 0x20, NULL, 0);
    assert(s->shown[0][0] == 0x33);
    command(s, 0x32, gray, sizeof(gray));
    command(s, 0x21, (uint8_t[]){0x40}, 1); /* bypass RED RAM */
    command(s, 0x20, NULL, 0);
    assert(s->shown[0][0] == 0x33);
    command(s, 0x21, (uint8_t[]){0}, 1);
    command(s, 0x22, (uint8_t[]){0xf7}, 1); /* load monochrome OTP LUT */
    command(s, 0x20, NULL, 0);
    assert(s->shown[0][0] == 0x33);
    free(s);
}

int main(int argc, char **argv)
{
    assert(argc == 2);
    if (!strcmp(argv[1], "id")) read_id();
    else if (!strcmp(argv[1], "bitbang")) bitbang_id();
    else if (!strcmp(argv[1], "lut")) lut_pixels();
    else abort();
    return 0;
}
'''


class SSD1677Protocol(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.directory = tempfile.TemporaryDirectory()
        cls.addClassCleanup(cls.directory.cleanup)
        source = (ROOT / 'hw/display/ssd1677.c').read_text()
        core = source[source.index('#define W '):
                      source.index('static bool ssd1677_update_display')]
        path = Path(cls.directory.name) / 'protocol.c'
        cls.executable = Path(cls.directory.name) / 'protocol'
        path.write_text(PRELUDE + core + CASES)
        subprocess.run([os.environ.get('CC', 'cc'), '-std=c11',
                        str(path), '-o', str(cls.executable)], check=True,
                       capture_output=True, text=True)

    def check_case(self, case):
        result = subprocess.run([str(self.executable), case], capture_output=True,
                                text=True)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_panel_identification_stream_and_restart(self):
        self.check_case('id')

    def test_gpio_read_edges_chip_select_reset_and_switch_to_spi(self):
        self.check_case('bitbang')

    def test_mono_and_gray_luts_render_the_correct_pixel_levels(self):
        self.check_case('lut')
