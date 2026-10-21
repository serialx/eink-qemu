"""The developer probe must inspect safely and validate real socket framing."""
import json
from pathlib import Path
import platform
import socket
import struct
import subprocess
import tempfile
from concurrent.futures import ThreadPoolExecutor
import unittest

SOURCE = Path(__file__).parents[1] / 'desktop/scripts/bridge/probe-macos.c'


@unittest.skipUnless(platform.system() == 'Darwin', 'macOS native APIs')
class MacProbe(unittest.TestCase):
    def setUp(self):
        self.assertTrue(SOURCE.exists(), 'macOS native probe is missing')
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.binary = str(Path(self.temp.name) / 'probe')
        subprocess.run(['clang', '-Wall', '-Wextra', '-Werror', '-framework', 'vmnet', str(SOURCE), '-o', self.binary], check=True)

    def test_inspection_does_not_claim_installation_or_open(self):
        result = subprocess.run([self.binary], check=True, capture_output=True, text=True)
        report = json.loads(result.stdout)
        self.assertEqual(report['inspect'], 'passed')
        self.assertEqual(report['install'], 'untested')
        self.assertEqual(report['openAsUser'], 'untested')
        self.assertIsInstance(report['adapters'], list)

    def test_ordinary_user_sends_and_receives_complete_qemu_frame(self):
        path = str(Path(self.temp.name) / 'socket')
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as server:
            server.bind(path)
            server.listen(2)
            server.settimeout(5)
            def echo():
                with server.accept()[0] as receiver, server.accept()[0] as sender:
                    sender.settimeout(5)
                    data = b''
                    while len(data) < 64:
                        chunk = sender.recv(64 - len(data))
                        if not chunk:
                            raise EOFError('probe closed before a complete frame')
                        data += chunk
                    self.assertEqual(struct.unpack('!I', data[:4])[0], 60)
                    # Split framing is valid on a stream; native probe must read fully.
                    receiver.sendall(data[:2])
                    receiver.sendall(data[2:])
            with ThreadPoolExecutor(max_workers=1) as executor:
                exchange = executor.submit(echo)
                result = subprocess.run([self.binary, 'open', path], capture_output=True, text=True, timeout=10)
                exchange.result(timeout=10)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout)['openAsUser'], 'passed')

    def test_identity_drop_cannot_be_requested_by_an_ordinary_process(self):
        result = subprocess.run([self.binary, 'deny-as', 'nobody', 'staff', '/absent'], capture_output=True, text=True)
        self.assertEqual(result.returncode, 77)

    def test_missing_socket_is_failure_not_ready(self):
        result = subprocess.run([self.binary, 'open', str(Path(self.temp.name) / 'absent')], capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0)


if __name__ == '__main__':
    unittest.main()
