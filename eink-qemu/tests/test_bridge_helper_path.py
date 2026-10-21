"""The managed privileged helper must never relocate its ACL beside a binary."""
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class BridgeHelperPath(unittest.TestCase):
    def test_managed_acl_ignores_executable_relocation_but_legacy_keeps_it(self):
        compiler = shutil.which('cc')
        if not compiler:
            self.skipTest('C compiler unavailable')
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / 'probe.c'
            source.write_text('''
#include <stdlib.h>
#include <string.h>
#include <stdio.h>
#define g_strdup strdup
#define DEFAULT_ACL_FILE "/legacy/etc/qemu/bridge.conf"
static int relocated;
static char *get_relocated_path(const char *path) {
    (void)path;
    relocated++;
    return strdup("/user-controlled/bridge.conf");
}
#include "qemu-bridge-helper-path.h"
int main(void) {
    char *path = bridge_acl_path();
    printf("%s\\n%d\\n", path, relocated);
    free(path);
    return 0;
}
''')
            for managed in (False, True):
                with self.subTest(managed=managed):
                    binary = root / ('managed' if managed else 'legacy')
                    args = [compiler, '-I', str(ROOT), str(source), '-o', str(binary)]
                    if managed:
                        args.append('-DEINK_BRIDGE_ACL_FILE="/etc/eink-qemu/bridge/bridge.conf"')
                    build = subprocess.run(args, capture_output=True, text=True)
                    self.assertEqual(build.returncode, 0, build.stderr)
                    result = subprocess.check_output([str(binary)], text=True).splitlines()
                    self.assertEqual(result, ['/etc/eink-qemu/bridge/bridge.conf', '0'] if managed else ['/user-controlled/bridge.conf', '1'])


if __name__ == '__main__':
    unittest.main()
