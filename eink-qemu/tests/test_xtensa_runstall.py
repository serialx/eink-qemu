"""A stall released before the next CPU timeslice must cancel the pending halt."""
from pathlib import Path
import subprocess
import tempfile
import unittest

class XtensaRunstall(unittest.TestCase):
    def test_release_cancels_pending_halt(self):
        source = (Path(__file__).resolve().parents[2] / 'target/xtensa/helper.c').read_text()
        start = source.index('void xtensa_runstall(')
        function = source[start:source.index('\n}', start)+2]
        harness = '''
#include <stdbool.h>
#include <assert.h>
#define CPU_INTERRUPT_HALT 32
typedef struct { bool halted; unsigned interrupt_request; } CPUState;
typedef struct { bool runstall; CPUState cpu; } CPUXtensaState;
static CPUState *env_cpu(CPUXtensaState *e) { return &e->cpu; }
static void cpu_interrupt(CPUState *s, unsigned m) { s->interrupt_request |= m; }
static void cpu_reset_interrupt(CPUState *s, unsigned m) { s->interrupt_request &= ~m; }
static void qemu_cpu_kick(CPUState *s) {}
'''+function+'''
int main(void) {
    CPUXtensaState e = {0};
    e.cpu.interrupt_request = 1;
    xtensa_runstall(&e, true);
    assert(e.cpu.halted && (e.cpu.interrupt_request & CPU_INTERRUPT_HALT));
    xtensa_runstall(&e, false);
    assert(!e.cpu.halted && !e.runstall);
    assert(e.cpu.interrupt_request == 1);
}
'''
        with tempfile.TemporaryDirectory() as d:
            src = Path(d)/'test.c'; exe = Path(d)/'test';src.write_text(harness)
            subprocess.run(['cc',str(src),'-o',str(exe)],check=True,capture_output=True)
            r = subprocess.run([str(exe)],capture_output=True,text=True)
            self.assertEqual(r.returncode,0,r.stderr)
