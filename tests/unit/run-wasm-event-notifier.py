#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-or-later
"""Build/run the Wasm notifier test against an existing QEMU Wasm build.

Example: python3 tests/unit/run-wasm-event-notifier.py
The build uses its existing compiler command and libqemuutil, without editing
Meson or the SDK. --no-run produces index.html plus JS/Wasm for browser tests;
serve the output directory with x4prosim/wasm/serve.py (COOP/COEP required).
"""
import argparse
import json
import os
from pathlib import Path
import shlex
import subprocess


def main():
    root = Path(__file__).resolve().parents[2]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--build-dir', type=Path,
                        default=root / 'build-wasm-32limit')
    parser.add_argument('--output-dir', type=Path)
    parser.add_argument('--node', default='node')
    parser.add_argument('--no-run', action='store_true')
    args = parser.parse_args()
    build = args.build_dir.resolve()
    output = (args.output_dir or build / 'evidence/event-notifier').resolve()
    output.mkdir(parents=True, exist_ok=True)
    commands = subprocess.check_output([
        'ninja', '-C', str(build), '-t', 'commands',
        'libqemuutil.a.p/util_event_notifier-posix.c.o'], text=True)
    command = next(line for line in reversed(commands.splitlines())
                   if ' -c ' in line and 'util/event_notifier-posix.c' in line)
    tokens = iter(shlex.split(command))
    compile_args = []
    for token in tokens:
        if token in ('-o', '-MQ', '-MF'):
            next(tokens)
        elif token not in ('-MD', '-c') and not token.endswith(
                'util/event_notifier-posix.c'):
            compile_args.append(token)
    sysroot = Path(os.environ.get(
        'WASM_SYSROOT', root / 'build-wasm-deps/sysroot-wasm64'))
    compile_args += [
        str(root / 'tests/unit/test-wasm-event-notifier.c'),
        'libqom.a.p/qom_object.c.o', 'libqom.a.p/qom_object_interfaces.c.o',
        'libqemuutil.a', '-L' + str(sysroot / 'lib'),
        '-lglib-2.0', '-lgthread-2.0', '-lpcre2-8',
        '-UNDEBUG', '-sASYNCIFY=1', '-sPROXY_TO_PTHREAD=1',
        '-sINITIAL_MEMORY=268435456',
        '-sALLOW_MEMORY_GROWTH=0', '-sSTACK_SIZE=8388608',
        '-sEXIT_RUNTIME=1', '-sFORCE_FILESYSTEM=1',
        '-o', str(output / 'test.js')]
    env = dict(os.environ)
    env.setdefault('EM_CACHE', str(root / 'build-wasm-deps/em-cache'))
    (output / 'command.json').write_text(json.dumps(compile_args, indent=2))
    with (output / 'build.log').open('w') as log:
        subprocess.run(compile_args, cwd=build, env=env, stdout=log,
                       stderr=subprocess.STDOUT, check=True)
    (output / 'index.html').write_text('''<!doctype html>
<meta charset="utf-8"><title>Wasm notifier test</title><pre id="result"></pre>
<script>
var Module = {
  print: s => { document.querySelector('#result').textContent += s + '\\n'; },
  printErr: console.error,
  onAbort: e => { document.querySelector('#result').textContent += 'FAIL ' + e; }
};
</script><script src="test.js"></script>
''')
    if not args.no_run:
        result = subprocess.run([args.node, str(output / 'test.js')],
                                text=True, capture_output=True, timeout=60)
        (output / 'run.log').write_text(result.stdout + result.stderr)
        result.check_returncode()
        if 'PASS queued=40000' not in result.stdout:
            raise RuntimeError('test did not report completion')
        print(result.stdout, end='')


if __name__ == '__main__':
    main()
