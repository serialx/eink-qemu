"""Exercise the managed helper's user-scoped policy without network privileges."""
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class ManagedBridgeACL(unittest.TestCase):
    def test_only_exact_user_and_bridge_grants_are_accepted(self):
        compiler = shutil.which('cc')
        if not compiler:
            self.skipTest('C compiler unavailable')
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / 'probe.c'
            source.write_text('''
#include "qemu-bridge-helper-acl.h"
int main(int argc, char **argv) {
    return eink_bridge_acl_allowed(stdin, (uid_t)strtoul(argv[1], NULL, 10), argv[2]) ? 0 : 1;
}
''')
            binary = root / 'probe'
            build = subprocess.run([compiler, '-I', str(ROOT), str(source), '-o', str(binary)], capture_output=True, text=True)
            self.assertEqual(build.returncode, 0, build.stderr)
            cases = [
                ('allow 1000 br-test\n', '1000', 'br-test', True),
                ('allow 1000 br-test\n', '1001', 'br-test', False),
                ('allow 1000 br-test\n', '1000', 'br-other', False),
                ('allow 1000 br-test\n', '0', 'br-test', False),
                ('# Generated grants\nallow 1001 other\nallow 1000 br-test\n', '1000', 'br-test', True),
                ('allow br-test\n', '1000', 'br-test', False),
                ('allow 1000 all\n', '1000', 'br-test', False),
                ('include /tmp/allow-all\n', '1000', 'br-test', False),
                ('allow 1000 br-test\ninvalid\n', '1000', 'br-test', False),
                ('allow 1000 br-test extra\n', '1000', 'br-test', False),
                ('allow -1 br-test\n', '4294967295', 'br-test', False),
                ('allow 4294968296 br-test\n', '1000', 'br-test', False),
                ('allow 1000 br-test\x00hidden\n', '1000', 'br-test', False),
                ('allow 1000 br-test\n' + '#' * 70000, '1000', 'br-test', False),
                ('', '1000', 'br-test', False),
            ]
            for policy, uid, bridge, allowed in cases:
                with self.subTest(policy=policy[:80], uid=uid, bridge=bridge):
                    result = subprocess.run([str(binary), uid, bridge], input=policy, text=True)
                    self.assertEqual(result.returncode == 0, allowed)


if __name__ == '__main__':
    unittest.main()
