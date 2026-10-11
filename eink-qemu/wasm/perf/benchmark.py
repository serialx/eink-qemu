#!/usr/bin/env python3
"""Measure X3 boot and thumbnail phases using fresh image copies (stdlib only)."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import selectors
import shutil
import signal
import statistics
import subprocess
import time

ROOT = Path(__file__).resolve().parents[3]
HERE = Path(__file__).resolve().parent
WAIT = re.compile(rb'\[\d+\]\s+Wait complete:[^\r\n]+')
HOME = re.compile(rb'Wait complete:\s+X3_DRF')
MEM = re.compile(rb'^.*\[MEM\][^\r\n]*[\r\n]', re.M)


def copy_image(source, destination):
    if os.uname().sysname == 'Darwin':
        result = subprocess.run(['cp', '-c', str(source), str(destination)],
                                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        if result.returncode == 0:
            return
    shutil.copyfile(source, destination)


def sampler(pid, output):
    log = output.with_suffix('.stderr').open('w')
    proc = subprocess.Popen(['sample', str(pid), '600', '1', '-mayDie',
                             '-fullPaths', '-file', str(output)],
                            stdout=log, stderr=subprocess.STDOUT)
    return proc, log


def finish_sampler(pair):
    if pair:
        proc, log = pair
        proc.send_signal(signal.SIGINT)
        proc.wait(timeout=60)
        log.close()


def run(args, variant, number):
    out = args.output / f'{variant}-{number}'
    out.mkdir(parents=True, exist_ok=False)
    for source, name in ((args.flash, 'flash.bin'), (args.sd, 'sd.img')):
        copy_image(source, out / name)
    wasm = variant == 'wasm'
    prefix = '/host' if wasm else ''
    image = lambda name: (prefix + str(out / name)).replace(',', ',,')
    if wasm:
        command = [args.node]
        if args.profile == 'node':
            command += ['--cpu-prof', '--cpu-prof-interval=1000',
                        '--cpu-prof-dir=' + str(out), '--require', str(HERE / 'profile-hooks.cjs')]
        command += [str(args.wasm_build / 'qemu-system-riscv32.js')]
    else:
        command = [str(args.native_bin)]
    command += ['-L', prefix + str(ROOT / 'pc-bios'), '-machine', 'x3',
                '-icount', 'shift=0,sleep=' + args.sleep,
                '-drive', f'file={image("flash.bin")},if=mtd,format=raw,cache.direct=off',
                '-drive', f'file={image("sd.img")},if=sd,format=raw,cache.direct=off',
                '-chardev', 'stdio,id=cdc,mux=on', '-serial', 'null',
                '-global', 'driver=misc.esp32s3.usb_serial_jtag,property=chardev,value=cdc',
                '-mon', 'chardev=cdc,mode=readline', '-display', 'none', '-nic', 'none']
    if wasm:
        command += ['-accel', 'tcg,tb-size=64']
    if args.mode == 'turbo':
        command += subprocess.check_output(
            ['sh', str(ROOT / 'eink-qemu/turbo-args.sh'), 'x3'],
            text=True).splitlines()
    env = {**os.environ, 'WASM_JIT_STATS': '1', 'TURBO_PROFILE_DIR': str(out)}
    start = time.monotonic()
    p = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                         stderr=subprocess.STDOUT, env=env)
    sel = selectors.DefaultSelector()
    sel.register(p.stdout, selectors.EVENT_READ)
    samples = sampler(p.pid, out / 'native-home.sample') if args.profile == 'native' and not wasm and args.stop_at == 'home' else None
    pending_samples = []
    data = bytearray()
    row = {'variant': variant, 'mode': args.mode, 'pid': p.pid, 'argv': command, 'sleep': args.sleep,
           'start_us': start * 1e6, 'wall_offset_us': time.time() * 1e6 - time.monotonic() * 1e6, 'profile': args.profile, 'stop_at': args.stop_at}
    shot = out / 'x3.ppm'
    try:
        with (out / 'x3.log').open('wb') as log:
            while time.monotonic() - start < args.timeout:
                for key, _ in sel.select(.02):
                    chunk = os.read(key.fd, 65536)
                    data.extend(chunk)
                    log.write(chunk)
                    log.flush()
                now = time.monotonic()
                if 'home_s' not in row and len(HOME.findall(data)) >= 3:
                    row['home_s'] = now - start
                    row['home_us'] = now * 1e6
                    if samples:
                        samples[0].send_signal(signal.SIGINT)
                        pending_samples.append(samples)
                        samples = None
                    if args.profile == 'native' and not wasm and args.stop_at == 'settled':
                        samples = sampler(p.pid, out / 'native-settled.sample')
                    print(f'{variant}-{number}: Home {row["home_s"]:.3f}s', flush=True)
                    if args.stop_at == 'home':
                        break
                if 'home_s' in row and 'settled_s' not in row and MEM.search(data):
                    row['settled_s'] = now - start
                    row['settled_us'] = now * 1e6
                    row['home_to_settled_s'] = row['settled_s'] - row['home_s']
                    if samples:
                        samples[0].send_signal(signal.SIGINT)
                        pending_samples.append(samples)
                        samples = None
                    p.stdin.write(('\x01cscreendump ' + json.dumps(prefix + str(shot)) + ' panel\n').encode())
                    p.stdin.flush()
                if shot.exists():
                    raw = shot.read_bytes()
                    parts = raw.split(b'\n', 3)
                    if len(parts) == 4 and parts[0] == b'P6' and parts[1] == b'528 792' and len(parts[3]) == 528*792*3:
                        row['ppm_sha256'] = hashlib.sha256(raw).hexdigest()
                        break
                if p.poll() is not None:
                    raise RuntimeError(f'emulator exited {p.returncode}: {bytes(data[-2000:])!r}')
            else:
                raise RuntimeError('emulator milestone timeout')
        row['wait_lines'] = [x.decode().strip() for x in WAIT.findall(data)[:8]]
        row['jit_modules'] = [int(x) for x in re.findall(rb'WASM_JIT compiled_tbs=(\d+)', data)][-1:]
        if args.stop_at != 'home' and len(row['wait_lines']) != 8:
            raise RuntimeError('expected eight complete guest wait milestones')
        # Normal exit flushes Node CPU profiles for all pthread workers.
        p.stdin.write(b'\x01cquit\n' if args.stop_at == 'home' else b'quit\n')
        p.stdin.flush()
        try:
            tail, _ = p.communicate(timeout=30)
            with (out / 'x3.log').open('ab') as log:
                log.write(tail)
        except subprocess.TimeoutExpired:
            p.terminate()
            p.wait(timeout=10)
            if args.profile == 'node':
                raise RuntimeError('normal Node shutdown required to flush profiles')
        if p.returncode != 0:
            raise RuntimeError(f'emulator shutdown returned {p.returncode}')
        row['returncode'] = p.returncode
        (out / 'run.json').write_text(json.dumps(row, indent=2) + '\n')
        return row
    finally:
        if p.poll() is None:
            p.kill()
            p.wait()
        if samples:
            finish_sampler(samples)
        for proc, log in pending_samples:
            proc.wait(timeout=60)
            log.close()
        sel.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--flash', type=Path, default=os.environ.get('X3_FLASH'))
    parser.add_argument('--sd', type=Path, default=os.environ.get('X3_SD'))
    parser.add_argument('--native-bin', type=Path, default=ROOT / 'build/qemu-system-riscv32')
    parser.add_argument('--wasm-build', type=Path, default=ROOT / 'build-wasm-32limit')
    parser.add_argument('--node', default=os.environ.get('NODE', 'node'))
    parser.add_argument('--variants', nargs='+', choices=['native', 'wasm'], default=['native', 'wasm'])
    parser.add_argument('--runs', type=int, default=3)
    parser.add_argument('--sleep', choices=['on', 'off'], default='off')
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument('--turbo', dest='mode', action='store_const', const='turbo',
                      help='apply canonical X3 turbo settings (forces sleep=off)')
    mode.add_argument('--accurate', dest='mode', action='store_const', const='accurate',
                      help='use accurate timing, overriding TURBO=1')
    parser.add_argument('--profile', choices=['none', 'node', 'native'], default='none')
    parser.add_argument('--stop-at', choices=['home', 'settled'], default='settled')
    parser.add_argument('--timeout', type=float, default=1200)
    parser.add_argument('--output', type=Path, default=ROOT / 'build-wasm/evidence/profile/timing')
    args = parser.parse_args()
    if args.mode is None:
        if os.environ.get('TURBO', '0') not in ('0', '1'):
            parser.error('TURBO must be 0 or 1')
        args.mode = 'turbo' if os.environ.get('TURBO') == '1' else 'accurate'
    if args.mode == 'turbo':
        args.sleep = 'off'
    if args.runs < 1 or args.timeout <= 0:
        parser.error('--runs and --timeout must be positive')
    if args.profile != 'none':
        expected = ['wasm'] if args.profile == 'node' else ['native']
        if args.variants != expected:
            parser.error('--profile node requires --variants wasm; native requires --variants native')
    if not args.flash or not args.sd:
        parser.error('provide --flash and --sd (or X3_FLASH and X3_SD)')
    for name in ('flash', 'sd', 'native_bin', 'wasm_build', 'output'):
        setattr(args, name, getattr(args, name).expanduser().resolve())
    args.output.mkdir(parents=True, exist_ok=True)
    for previous in args.output.glob('*/run.json'):
        # Older harness versions always ran accurate mode, even with TURBO=1.
        if json.loads(previous.read_text()).get('mode', 'accurate') != args.mode:
            parser.error(f'refusing to mix {args.mode} with another mode in {previous}')
    rows = []
    for number in range(1, args.runs + 1):
        for variant in args.variants:  # interleave variants on shared hosts
            row = run(args, variant, number)
            rows.append(row)
            if row['mode'] != rows[0]['mode']:
                raise RuntimeError('refusing to compare runs of different modes')
            if args.stop_at != 'home':
                assert row['ppm_sha256'] == rows[0]['ppm_sha256'], 'native/variant screenshot differs'
                if args.sleep == 'off' and args.mode == 'accurate':
                    assert row['wait_lines'] == rows[0]['wait_lines'], 'guest timestamps differ'
            result = {'mode': args.mode, 'runs': rows, 'medians': {}}
            for name in args.variants:
                subset = [r for r in rows if r['variant'] == name]
                if subset:
                    result['medians'][name] = {k: statistics.median(r[k] for r in subset)
                        for k in ('home_s', 'settled_s', 'home_to_settled_s') if k in subset[0]}
            (args.output / 'results.json').write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps(result['medians'], indent=2))


if __name__ == '__main__':
    main()
