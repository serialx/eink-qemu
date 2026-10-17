"""Wi-Fi DMA and frame bounds, without firmware or external network traffic."""
import importlib
import os
from pathlib import Path
import socket
import struct
import time
import unittest
from unittest.mock import patch

boot = importlib.import_module('eink-qemu.tests.test_x4pro_boot')
WIFI = 0x60033000
DESC, BUFFER = 0x3FC90000, 0x3FC91000


class WifiHarness(boot.StockBootHarness):
    extra_qemu_args = ['-nic', 'user,model=esp32_wifi', '-d', 'guest_errors']

    def post(self, packet):
        self.command(f'write {BUFFER:#x} {len(packet)} 0x{packet.hex()}')
        self.write(DESC, (1 << 31) | (len(packet) << 12) | len(packet))
        self.write(DESC + 4, BUFFER)
        self.write(DESC + 8, 0)
        self.write(WIFI + 0xd08, (1 << 31) | (DESC & 0xfffff))
        self.assertEqual(self.read(WIFI + 0xd14), 1)
        self.assertIsNone(self.proc.poll())

    def errors(self):
        self.err.seek(0)
        return self.err.read().decode(errors='replace')


class WifiDMA(WifiHarness):
    def test_oversized_tx_descriptor_is_dropped(self):
        self.post(bytes(4095))
        self.assertIn('Wi-Fi TX frame too long', self.errors())
        self.assertEqual(self.read(WIFI + 0xc3c) & 0x80, 0x80)

    def test_short_management_frame_is_dropped(self):
        self.post(b'\xb0' + bytes(23))  # authentication without fixed fields
        self.assertIn('Wi-Fi malformed frame', self.errors())

    def test_action_without_complete_fcs_is_dropped(self):
        self.post(b'\xd0' + bytes(23 + 3))
        self.assertIn('Wi-Fi malformed frame', self.errors())

    def test_truncated_ssid_is_dropped(self):
        self.post(b'\x80' + bytes(23 + 12) + b'\x00\x20x')
        self.assertIn('Wi-Fi malformed frame', self.errors())

    def test_short_dhcp_offer_is_dropped(self):
        self.post(b'\x10' + bytes(23 + 6 + 4))  # association response
        self.post(b'\x08' + bytes(23 + 8 + 4))
        self.assertIn('Wi-Fi short DHCP frame', self.errors())

    def test_oversized_ssid_is_dropped(self):
        packet = b'\x80' + bytes(23 + 12) + bytes([0, 255]) + b'x' * 255
        self.post(packet)
        self.assertIn('Wi-Fi malformed frame', self.errors())

    def test_action_at_frame_capacity_does_not_overrun_ethernet_buffer(self):
        self.post(b'\xd0\x00\x00\x00' + b'\xff' * 6 + bytes(2330))
        self.assertIn('Wi-Fi Ethernet payload too long', self.errors())

    def test_rx_descriptor_capacity_is_respected(self):
        rx_desc, rx_buffer = DESC + 0x100, BUFFER + 0x2000
        self.write(rx_desc, (1 << 31) | 16)
        self.write(rx_desc + 4, rx_buffer)
        self.write(rx_desc + 8, 0)
        sentinel = bytes([0xa5]) * 512
        self.command(f'write {rx_buffer:#x} 512 0x{sentinel.hex()}')
        self.write(WIFI + 0x88, rx_desc)
        # A guest SoftAP beacon elicits a probe request from the fake station.
        self.post(b'\x80' + bytes(23 + 12) + b'\x00\x04test' + bytes(4))
        self.command('clock_step 10000000')
        actual = bytes.fromhex(self.command(f'read {rx_buffer:#x} 512')[0][2:])
        self.assertEqual(actual, sentinel)
        self.assertEqual(self.read(rx_desc), (1 << 31) | 16)
        self.assertIn('Wi-Fi RX buffer too small', self.errors())

    def test_valid_beacon_gets_a_bounded_probe_reply(self):
        rx_desc, rx_buffer = DESC + 0x100, BUFFER + 0x2000
        self.write(rx_desc, (1 << 31) | 512)
        self.write(rx_desc + 4, rx_buffer)
        self.write(rx_desc + 8, 0)
        self.write(rx_buffer + 512, 0xa5a5a5a5)
        self.write(WIFI + 0x88, rx_desc)
        self.post(b'\x80' + bytes(23 + 12) + b'\x00\x20' + b'x' * 32 + bytes(4))
        self.command('clock_step 10000000')
        length = (self.read(rx_desc) >> 12) & 0xfff
        self.assertGreater(length, 32)
        self.assertLessEqual(length, 512)
        self.assertEqual(self.read(rx_buffer + 512), 0xa5a5a5a5)
        self.assertEqual(self.read(WIFI + 0xc3c) & 0x1004000, 0x1004000)
        self.assertNotIn('Wi-Fi malformed', self.errors())


class WifiActionFrames(WifiHarness):
    qemu = boot.QEMU

    def setUp(self):
        binary = patch.object(boot, 'QEMU', self.qemu)
        binary.start()
        self.addCleanup(binary.stop)
        with socket.socket() as listener:
            listener.settimeout(5)
            listener.bind(('127.0.0.1', 0))
            listener.listen(1)
            port = listener.getsockname()[1]
            self.extra_qemu_args = [
                '-nic', f'socket,model=esp32_wifi,connect=127.0.0.1:{port}',
                '-d', 'guest_errors',
            ]
            super().setUp()
            self.net, _ = listener.accept()
        self.net.settimeout(5)
        self.addCleanup(self.net.close)

    def recv_exact(self, size):
        data = b''
        while len(data) < size:
            chunk = self.net.recv(size - len(data))
            self.assertTrue(chunk, 'network connection closed')
            data += chunk
        return data

    def inject(self, packet, *, error=None):
        desc, buf = DESC + 0x100, BUFFER + 0x2000
        self.write(desc, (1 << 31) | 2048)
        self.write(desc + 4, buf)
        self.write(desc + 8, 0)
        sentinel = bytes([0xa5]) * 2048
        self.command(f'write {buf:#x} 2048 0x{sentinel.hex()}')
        self.write(WIFI + 0x88, desc)
        self.net.sendall(struct.pack('!I', len(packet)) + packet)
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            self.command('clock_step 1000000')
            length = (self.read(desc) >> 12) & 0xfff
            if length or (error and error in self.errors()):
                break
            time.sleep(0.005)
        else:
            self.fail('network frame was not processed')
        data = bytes.fromhex(self.command(f'read {buf:#x} 2048')[0][2:])
        self.assertEqual(data[length:], sentinel[length:])
        if error:
            self.assertEqual(self.read(desc), (1 << 31) | 2048)
            self.assertIn(error, self.errors())
        return data[:length]

    def test_action_roundtrip_has_one_fcs(self):
        for size in (1, 32, 1500):
            for flags in (0, 0x40):
                with self.subTest(size=size, flags=flags):
                    payload = bytes(i % 251 for i in range(size))
                    fcs = b'\x12\x34\x56\x78'
                    frame = (bytes([0xd0, flags, 0, 0]) + b'\xff' * 6 +
                             bytes(14) + payload + fcs)
                    self.post(frame)
                    size_on_wire = struct.unpack('!I', self.recv_exact(4))[0]
                    ethernet = self.recv_exact(size_on_wire)
                    self.assertEqual(ethernet[14:], payload + fcs)
                    # Receive the broadcast as if sent by another station.
                    ethernet = (ethernet[:6] + b'\x12\x34\x56\x78\x9a\xbc' +
                                ethernet[12:])
                    received = self.inject(ethernet)
                    self.assertEqual(len(received), 48 + len(frame))
                    self.assertEqual(received[48:50], bytes([0xd0, flags]))
                    # RX metadata precedes the WLAN header; insertion replaces FCS.
                    self.assertEqual(received[48 + 24:], payload + bytes(4))

    def test_encapsulated_action_without_fcs_is_dropped(self):
        header = b'\xff' * 6 + b'\x12\x34\x56\x78\x9a\xbc' + b'\xa0\x0d'
        self.inject(header + bytes(3), error='Wi-Fi short encapsulated action frame')


@unittest.skipUnless(Path(os.environ.get('QEMU_RISCV32',
                     boot.ROOT / 'build/qemu-system-riscv32')).exists(),
                     'build qemu-system-riscv32 first')
class WifiActionFramesC3(WifiActionFrames):
    machine = 'x3'
    qemu = Path(os.environ.get('QEMU_RISCV32',
                              boot.ROOT / 'build/qemu-system-riscv32'))


if __name__ == '__main__':
    unittest.main()
