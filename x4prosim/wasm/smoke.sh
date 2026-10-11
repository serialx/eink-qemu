#!/usr/bin/env bash
# smoke.sh [--turbo] flash.bin sd.img or [--turbo] --rom-only.
# Optional TIMEOUT, EVIDENCE_DIR, NODE, WASM64_MODE, NATIVE_QEMU, REFERENCE_PPM.
# RUN_NODE overrides the launcher for comparisons; REQUIRE_JIT=0 permits TCI.
# WEB_URL opts into smoke.mjs followed by explorer-smoke.mjs using an already
# running Chrome (CDP_PORT) and packaged server. Use a picker URL without flash/sd
# queries, PICKER_FLASH=X3 release app, EXPLORER_EPUB=demian.epub and
# EXPLORER_SMALL_EPUB=another.epub; mtools and fsck.fat must be on PATH.
# Browser tests inherit existing smoke.mjs env vars; the explorer always uses the
# default blank card. INPUT_SD defaults to the supplied source SD for smoke.mjs.
# The legacy X3 browser check requires a card that causes guest writes; set
# PICKER_SD and INPUT_SD to that fixture (the explorer ignores PICKER_SD).
# The page defaults to turbo; append ?turbo=0 to WEB_URL for accurate timing.
# Omit WEB_URL to retain the existing Node/native-only flow (including CI).
# Images are copied; logs, PPM screenshots and measurements remain in evidence/.
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
exec python3 - "$ROOT" "$@" <<'PY'
import json
import os
from pathlib import Path
import re
import selectors
import shutil
import subprocess
import sys
import time

root = Path(sys.argv[1])
args = sys.argv[2:]
turbo = bool(args and args[0] == '--turbo')
sleep = os.environ.get('RUN_NODE_SLEEP') or 'on'
if sleep not in ('on', 'off'):
    sys.exit('RUN_NODE_SLEEP must be on or off')
if turbo:
    sleep = 'off'
if turbo:
    args = args[1:]
rom_only = args == ['--rom-only']
if not rom_only and (len(args) != 2 or any(arg.startswith('--') for arg in args)):
    sys.exit('usage: smoke.sh [--turbo] flash.bin sd.img | [--turbo] --rom-only')
evidence = Path(os.environ.get('EVIDENCE_DIR', root / 'build-wasm/evidence/smoke')).resolve()
evidence.mkdir(parents=True, exist_ok=True)
if rom_only:
    subprocess.run([sys.executable, str(root / 'x4prosim/mksd.py'),
                    str(evidence / 'sd.img'), '64', '--cluster-size', '512'], check=True)
else:
    for src, dest in zip(args, ('flash.bin', 'sd.img')):
        shutil.copyfile(src, evidence / dest)
(evidence / 'blank.bin').write_bytes(bytes(16 * 1024 * 1024))
timeout = float(os.environ.get('TIMEOUT', '600'))
# CrossPoint X3 and X4 Pro draw Home by the third completed refresh.
# The optional [HOME] thumbnail warning disappears once its cache exists.
firmware_machine = None
if not rom_only:
    image = (evidence / 'flash.bin').read_bytes()
    chip = next((int.from_bytes(image[offset + 12:offset + 14], 'little')
                 for offset in (0, 0x10000)
                 if image[offset:offset + 1] == b'\xe9'), None)
    if chip not in (5, 9):
        sys.exit('Expected a composed ESP32-C3 or ESP32-S3 firmware image')
    firmware_machine = 'x3' if chip == 5 else 'x4pro'
measurements = {}
reference = os.environ.get('REFERENCE_PPM')
# Turbo compares image content and boot milestones, never guest timestamps.
# Generate a fresh native turbo reference unless the caller supplies one.
if turbo and not rom_only and not os.environ.get('NATIVE_QEMU') and not reference:
    native_evidence = evidence / 'native-turbo'
    subprocess.run([str(root / 'x4prosim/wasm/smoke.sh'), '--turbo', *args],
        env={**os.environ, 'NATIVE_QEMU': str(root / 'build'),
             'EVIDENCE_DIR': str(native_evidence)}, check=True)
    reference = str(native_evidence / (firmware_machine + '.ppm'))


def run(machine, flash, firmware=False):
    milestone = re.compile(os.environ.get('HOME_MILESTONE',
        r'Wait complete:\s+(?:8279|X3)_DRF' if machine == 'x3' else
        r'Wait complete:\s+8179_DRF'))
    milestone_count = int(os.environ.get('HOME_MILESTONE_COUNT', '3'))
    log_path = evidence / (machine + '.log')
    shot = evidence / (machine + '.ppm')
    shot.unlink(missing_ok=True)
    native = os.environ.get('NATIVE_QEMU')
    require_jit = not native and os.environ.get('REQUIRE_JIT', '1') != '0'
    if native:
        arch = 'riscv32' if machine == 'x3' else 'xtensa'
        timing = ['-icount', 'shift=' + ('0' if machine == 'x3' else '2') +
                  ',sleep=' + sleep]
        if turbo:
            timing += subprocess.check_output(
                ['sh', str(root / 'x4prosim/turbo-args.sh'), machine],
                text=True).splitlines()
        command = [str(Path(native) / ('qemu-system-' + arch)),
            '-L', str(root / 'pc-bios'), '-machine', machine,
            *timing,
            '-drive', f'file={str(flash).replace(",", ",,")},if=mtd,format=raw',
            '-drive', f'file={str(evidence / "sd.img").replace(",", ",,")},if=sd,format=raw',
            '-chardev', 'stdio,id=cdc,mux=on', '-serial', 'null',
            '-global', 'driver=misc.esp32s3.usb_serial_jtag,property=chardev,value=cdc',
            '-mon', 'chardev=cdc,mode=readline', '-display', 'none', '-nic', 'none']
    else:
        command = [os.environ.get('RUN_NODE', str(root / 'x4prosim/wasm/run-node.sh')),
                   str(flash), str(evidence / 'sd.img')]
    start = time.monotonic()
    p = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                         stderr=subprocess.STDOUT, env={**os.environ, 'X4MACHINE': machine,
                         'WASM_JIT_STATS': '1', 'TURBO': '1' if turbo else '0'})
    sel = selectors.DefaultSelector()
    sel.register(p.stdout, selectors.EVENT_READ)
    output = bytearray()
    boot = None
    ready = None
    requested = False
    settled_time = None
    home_compiled = 0
    max_rss = 0
    sample_at = start
    try:
        with log_path.open('wb') as log:
            while time.monotonic() - start < timeout:
                now = time.monotonic()
                if now >= sample_at:
                    rss = subprocess.run(['ps', '-o', 'rss=', '-p', str(p.pid)],
                                         capture_output=True, text=True)
                    if rss.returncode == 0 and rss.stdout.strip():
                        max_rss = max(max_rss, int(rss.stdout.strip()))
                    sample_at = now + 1
                for key, _ in sel.select(.05):
                    chunk = os.read(key.fd, 65536)
                    if chunk:
                        log.write(chunk)
                        log.flush()
                        output.extend(chunk)
                text = output.decode(errors='replace')
                if any(failure in text for failure in ('Guru Meditation', 'RuntimeError:', 'Aborted(')):
                    raise RuntimeError(f'{machine}: guest or runtime crash; see {log_path}')
                if boot is None and 'ESP-ROM:' in text:
                    boot = time.monotonic() - start
                if ready is None:
                    matched = (len(milestone.findall(text)) >= milestone_count
                               if firmware else
                               text.count('invalid header: 0x00000000') >= 2)
                    if matched:
                        ready = time.monotonic() - start
                        counts = re.findall(r'WASM_JIT compiled_tbs=(\d+)', text)
                        home_compiled = int(counts[-1]) if counts else 0
                        print(f'{machine}: milestone at {ready:.3f} s', flush=True)
                # Allow the panel update / main loop to settle, then capture it.
                matches = list(milestone.finditer(text))
                settled = (ready is not None and re.search(r'\[MEM\][^\n]*\n', text[matches[milestone_count - 1].end():])
                           if firmware else
                           ready is not None and now - start > ready + 2)
                if ready is not None and not requested and settled:
                    settled_time = time.monotonic() - start
                    guest_shot = str(shot) if native else '/host' + str(shot)
                    p.stdin.write(f'\x01cscreendump "{guest_shot}" panel\n'.encode())
                    p.stdin.flush()
                    requested = True
                if shot.exists() and shot.stat().st_size > 1000:
                    header = shot.read_bytes().split(b'\n', 3)
                    if header[0] != b'P6':
                        raise RuntimeError('screendump is not a binary PPM')
                    width, height = map(int, header[1].split())
                    if len(header[3]) != width * height * 3:
                        continue
                    expected = (528, 792) if machine == 'x3' else (480, 800)
                    if (width, height) != expected:
                        raise RuntimeError(f'Unexpected panel size: {width}x{height}')
                    if firmware and len(set(header[3])) < 2:
                        raise RuntimeError(f'{machine} panel is blank')
                    counts = re.findall(r'WASM_JIT compiled_tbs=(\d+)', text)
                    compiled = int(counts[-1]) if counts else 0
                    if require_jit and firmware and compiled == 0:
                        raise RuntimeError('Home reached without any JIT-compiled TBs')
                    if firmware and reference and shot.read_bytes() != Path(reference).read_bytes():
                        raise RuntimeError(f'Panel differs from native reference {reference}')
                    screenshot_time = time.monotonic() - start
                    # Like drive.py, stop the emulator after capturing evidence.
                    # The guest remains running through the screenshot checkpoint.
                    if p.poll() is not None:
                        raise RuntimeError(f'Node exited {p.returncode}')
                    p.terminate()
                    deadline = time.monotonic() + 10
                    while True:
                        waited, status, usage = os.wait4(p.pid, os.WNOHANG)
                        if waited:
                            p.returncode = os.waitstatus_to_exitcode(status)
                            break
                        if time.monotonic() > deadline:
                            p.kill()
                        time.sleep(.01)
                    peak_rss = usage.ru_maxrss / (1024 if sys.platform == 'darwin' else 1)
                    measurements[machine] = dict(turbo=turbo, sleep=sleep, argv=command,
                        firmware=firmware, startup_to_rom_s=boot,
                        milestone_s=ready, settled_mem_s=settled_time, screenshot_s=screenshot_time,
                        jit_compiled_tbs=compiled, home_jit_compiled_tbs=home_compiled,
                        peak_rss_kib=peak_rss,
                        sampled_peak_rss_kib=max_rss,
                        screenshot=str(shot), log=str(log_path))
                    print(f'{machine}: PASS {shot}', flush=True)
                    return
                if p.poll() is not None:
                    raise RuntimeError(f'Node exited {p.returncode}: {text[-2000:]}')
            raise RuntimeError(f'{machine}: timed out after {timeout} s; see {log_path}')
    finally:
        if p.poll() is None:
            p.kill()
        p.wait()
        sel.close()
        p.stdin.close()
        p.stdout.close()


try:
    if not rom_only:
        run(firmware_machine, evidence / 'flash.bin', firmware=True)
    if firmware_machine != 'x4pro':
        run('x4pro', evidence / 'blank.bin')
except Exception as exc:
    sys.exit(str(exc))
(evidence / 'measurements.json').write_text(json.dumps(measurements, indent=2) + '\n')
print('PASS: X4 Pro blank-flash ROM and panel screendump' if rom_only else
      f'PASS: {firmware_machine} Home refresh and settled panel screendump' +
      ('; X4 Pro blank-flash ROM also passed' if firmware_machine == 'x3' else ''))
if os.environ.get('WEB_URL') and not os.environ.get('NATIVE_QEMU'):
    if rom_only:
        sys.exit('WEB_URL requires the X3 firmware and SD arguments, not --rom-only')
    browser_env = {**os.environ, 'EVIDENCE_DIR': str(evidence)}
    browser_env.setdefault('INPUT_SD', str(Path(args[1]).resolve()))
    for script in ('smoke.mjs', 'explorer-smoke.mjs'):
        subprocess.run([os.environ.get('NODE', 'node'),
                        str(root / 'x4prosim/wasm/web' / script), os.environ['WEB_URL']],
                       env=browser_env, check=True)
PY
