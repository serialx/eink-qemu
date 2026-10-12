#!/usr/bin/env python3
"""Run frozen probe commands in QEMU or on an exclusively owned serial port.

This tool never flashes hardware. The caller must release any serial monitor.
Commands are sent once; a missing completion marker fails without retrying input.
"""
import argparse
import json
import os
from pathlib import Path
import select
import re
import subprocess
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--qemu', type=Path)
    parser.add_argument('--flash', type=Path)
    parser.add_argument('--sd', type=Path)
    parser.add_argument('--port')
    parser.add_argument('--shift', default='0')
    parser.add_argument('--extra', action='append', default=[])
    parser.add_argument('--log', type=Path, required=True)
    parser.add_argument('--timeout', type=float, default=240)
    parser.add_argument('commands', nargs='+', choices=['info', 'cpu', 'psram', 'sd', 'panel', 'matrix', 'vendor', 'spi', 'light', 'deep'])
    args = parser.parse_args()
    if bool(args.qemu) == bool(args.port):
        parser.error('choose exactly one of --qemu and --port')
    proc = ser = None
    root = Path(__file__).resolve().parents[2]
    args.log.parent.mkdir(parents=True, exist_ok=True)
    try:
        if args.qemu:
            if not args.flash or not args.sd:
                parser.error('--qemu requires disposable --flash and --sd images')
            command = [str(args.qemu.resolve()), '-L', str(root / 'pc-bios'),
                       '-machine', 'x4pro,panel=uc8279-68',
                       '-icount', f'shift={args.shift},sleep=off',
                       '-drive', f'file={args.flash.resolve()},if=mtd,format=raw',
                       '-drive', f'file={args.sd.resolve()},if=sd,format=raw',
                       '-display', 'none', '-monitor', 'none', '-serial', 'none', '-nic', 'none',
                       '-chardev', 'stdio,id=cdc', '-global',
                       'driver=misc.esp32s3.usb_serial_jtag,property=chardev,value=cdc', *args.extra]
            args.log.with_suffix('.command.json').write_text(json.dumps(command, indent=2) + '\n')
            proc = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                    stderr=subprocess.STDOUT, bufsize=0)
            def receive():
                if select.select([proc.stdout], [], [], 0.2)[0]:
                    data = os.read(proc.stdout.fileno(), 65536)
                    if not data:
                        raise RuntimeError(f'QEMU exited ({proc.poll()})')
                    return data
                return b''
            def send(data):
                proc.stdin.write(data); proc.stdin.flush()
        else:
            import serial
            ser = serial.Serial(port=None, baudrate=115200, timeout=0.2)
            ser.dtr = False; ser.rts = False; ser.port = args.port; ser.open()
            receive = lambda: ser.read(ser.in_waiting or 1)
            send = ser.write
        with args.log.open('wb') as log:
            pending = b''
            def until(marker):
                nonlocal pending
                end = time.monotonic() + args.timeout
                next_info = time.monotonic() + 1
                info_retries = 0
                while time.monotonic() < end:
                    while b'\n' in pending:
                        line, pending = pending.split(b'\n', 1)
                        text = line.decode(errors='replace').strip()
                        if '[CAL] ERROR' in text or 'Guru Meditation' in text:
                            raise RuntimeError(text)
                        if '[CAL]' in text:
                            fields = dict(re.findall(r'(\w+)=(\S+)', text))
                            for key in ('err', 'errors', 'mismatch', 'cleanup', 'timeout'):
                                if key in fields and int(fields[key], 16 if key == 'err' else 10):
                                    raise RuntimeError(text)
                        if marker in text:
                            print(text, flush=True)
                            return
                    if args.port and marker == '[CAL] DONE light' and time.monotonic() >= next_info and info_retries < 4:
                        send(b'info\n'); info_retries += 1; next_info = time.monotonic() + 3
                    data = receive()
                    if data:
                        log.write(data); log.flush(); pending += data
                raise TimeoutError(f'{marker}: see {args.log}')
            if args.port:
                # Explicit INFO completion distinguishes its reply from a boot greeting.
                send(b'info\n')
                until('[CAL] DONE info')
            else:
                until('[CAL] READY')
            for command in args.commands:
                send((command + '\n').encode())
                if command == 'deep':
                    until('[CAL] ENTER deep')
                    until('[CAL] READY')
                else:
                    until(f'[CAL] DONE {command}')

    finally:
        if ser:
            ser.close()
        if proc:
            proc.terminate()
            try:
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                proc.kill(); proc.wait()


if __name__ == '__main__':
    main()
