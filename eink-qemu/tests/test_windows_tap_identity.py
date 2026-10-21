"""Managed TAP targets must resolve an exact GUID without friendly-name lookup."""
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


class WindowsTapIdentity(unittest.TestCase):
    @unittest.skipUnless(shutil.which('cc'), 'C compiler required')
    def test_guid_target_is_exact_and_bounded(self):
        root = Path(__file__).parents[2]
        source = r'''
#include <assert.h>
#include <stdbool.h>
#include <string.h>
#include "net/tap-win32-identity.h"
int main(void)
{
    struct { char guid[39]; char sentinel; } output = {{0}, 'x'};
    const char *valid = "guid:{12345678-1234-ABCD-1234-1234567890ab}";
    const char *invalid[] = {
        "Ethernet", "", "guid:", "guid:{12345678-1234-ABCD-1234-1234567890az}",
        "guid:{12345678-1234-ABCD-1234-1234567890ab}more",
        "guid:{12345678_1234-ABCD-1234-1234567890ab}",
        "guid:{12345678-1234-ABCD-1234-1234567890a}",
        "guid:12345678-1234-ABCD-1234-1234567890ab",
        "guid:{12345678-1234-ABCD-1234-1234567890ab}\\device"
    };
    assert(tap_win32_guid_target(valid, output.guid));
    assert(!strcmp(output.guid, valid + 5));
    assert(output.sentinel == 'x');
    assert(!tap_win32_guid_target(NULL, output.guid));
    for (unsigned i = 0; i < sizeof(invalid) / sizeof(invalid[0]); ++i) {
        assert(!tap_win32_guid_target(invalid[i], output.guid));
        assert(!strcmp(output.guid, valid + 5));
        assert(output.sentinel == 'x');
    }
    return 0;
}
'''
        with tempfile.TemporaryDirectory() as directory:
            test = Path(directory) / 'identity.c'
            binary = Path(directory) / 'identity'
            test.write_text(source)
            subprocess.run(['cc', '-Wall', '-Wextra', '-Werror', '-I', str(root), str(test), '-o', str(binary)], check=True)
            subprocess.run([str(binary)], check=True, timeout=5)


if __name__ == '__main__':
    unittest.main()
