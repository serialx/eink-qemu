#!/usr/bin/env python3
"""Capture one emulator run through DONE and Home, with a bounded timeout."""
import os
from pathlib import Path
import re
import selectors
import shlex
import signal
import subprocess
import sys
import time

sim, out = map(Path, sys.argv[1:])
env = dict(os.environ, X4MACHINE='x3', X4NET='-nic user,model=esp32_wifi')
# The upstream launcher hardcodes a shared monitor socket. Use a local copy
# with no monitor or GUI so this run cannot disturb another emulator.
launcher = (sim / 'eink-qemu/run.sh').read_text()
for old, new in [
    ('here=$(cd "$(dirname "$0")/.." && pwd)', 'here=' + shlex.quote(str(sim))),
    ('-monitor unix:/tmp/eink-qemu-mon.sock,server,nowait', '-monitor none'),
    ('-display sdl,show-cursor=on', '-display none'),
]:
    if launcher.count(old) != 1:
        raise SystemExit('Unrecognized simulator launcher: ' + old)
    launcher = launcher.replace(old, new)
(out / 'run.sh').write_text(launcher)
command = ['sh', str(out / 'run.sh'), str(out / 'flash.bin'), str(out / 'sd.img')] + shlex.split(os.environ.get('SD_TIMING_EXTRA', ''))
process = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                           env=env, start_new_session=True)
selector = selectors.DefaultSelector()
selector.register(process.stdout, selectors.EVENT_READ)
data = bytearray()
deadline = time.monotonic() + 600
success = False
try:
    with (out / 'console.log').open('wb') as log:
        while time.monotonic() < deadline:
            if not selector.select(timeout=1):
                if process.poll() is not None:
                    break
                continue
            chunk = os.read(process.stdout.fileno(), 65536)
            if not chunk:
                break
            data.extend(chunk)
            log.write(chunk)
            log.flush()
            if b'[SDP] SKIP' in data:
                break
            if re.search(rb'\[SDP\] DONE groups=60 ops=2880 errors=0\b', data) and b'Entering activity: Home' in data:
                success = True
                break
finally:
    if process.poll() is None:
        os.killpg(process.pid, signal.SIGTERM)
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            process.wait()
    selector.close()
if not success:
    print(bytes(data[-4000:]).decode(errors='replace'), file=sys.stderr)
    raise SystemExit('Missing successful DONE or Home; see console.log')
print('Verified DONE groups=60 ops=2880 errors=0 and Entering activity: Home')
