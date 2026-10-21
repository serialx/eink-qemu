"""Run the actual Windows stdio reader with deterministic host-side OS stubs."""
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


class WindowsStdioEof(unittest.TestCase):
    @unittest.skipUnless(shutil.which('cc'), 'C compiler required')
    def test_empty_input_stops_without_closing_the_process_handle(self):
        source = (Path(__file__).parents[2] / 'chardev/char-win-stdio.c').read_text()
        function = source.split('static DWORD WINAPI win_stdio_thread(LPVOID param)', 1)[1]
        function = function.split('static void win_stdio_thread_wait_func', 1)[0]
        harness = r'''
#include <assert.h>
#include <stdint.h>
#include <stdlib.h>
#include <stddef.h>
typedef unsigned long DWORD;
typedef void *LPVOID;
#define WINAPI
#define INFINITE 0xffffffff
#define WAIT_OBJECT_0 0
typedef struct {
    void *hStdIn, *hInputReadyEvent, *hInputDoneEvent;
    uint8_t win_stdio_buf;
} WinStdioChardev;
#define WIN_STDIO_CHARDEV(p) ((WinStdioChardev *)(p))
static int reads, signalled, removed, mode;
static int ReadFile(void *handle, uint8_t *byte, int count, DWORD *size, void *overlapped)
{
    assert(handle == (void *)1 && count == 1 && overlapped == NULL);
    /* Fail promptly if EOF is retried instead of spinning the test runner. */
    assert(++reads <= (mode == 2 ? 2 : 1));
    *size = mode == 2 && reads == 1 ? 1 : 0;
    *byte = 'x';
    return mode != 1;
}
static int SetEvent(void *event) { assert(event == (void *)2); ++signalled; return 1; }
static DWORD WaitForSingleObject(void *event, DWORD timeout)
{ assert(event == (void *)3 && timeout == INFINITE); return WAIT_OBJECT_0; }
static void qemu_del_wait_object(void *event, void *callback, void *opaque)
{ assert(event == (void *)2 && callback == NULL && opaque == NULL); ++removed; }
/* No CloseHandle stub: closing a borrowed standard handle must not compile. */
static DWORD WINAPI win_stdio_thread(LPVOID param)
''' + function + r'''
int main(void)
{
    WinStdioChardev stdio = {(void *)1, (void *)2, (void *)3, 0};
    for (mode = 0; mode < 3; ++mode) {
        reads = signalled = removed = 0;
        assert(win_stdio_thread(&stdio) == 0);
        assert(reads == (mode == 2 ? 2 : 1));
        assert(signalled == (mode == 2 ? 1 : 0));
        assert(removed == 1);
    }
    return 0;
}
'''
        with tempfile.TemporaryDirectory() as directory:
            test = Path(directory) / 'stdio.c'
            binary = Path(directory) / 'stdio'
            test.write_text(harness)
            subprocess.run(['cc', '-Wall', '-Wextra', '-Werror', str(test), '-o', str(binary)], check=True)
            subprocess.run([str(binary)], check=True, timeout=5)


if __name__ == '__main__':
    unittest.main()
