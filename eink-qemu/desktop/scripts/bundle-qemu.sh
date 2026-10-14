#!/bin/sh
# Stage native QEMU for the app bundle in src-tauri/qemu:
#   qemu/bin/qemu-system-{riscv32,xtensa}   from <repo>/build (or $EINK_QEMU_BUILD)
#   qemu/lib/*.dylib                        every non-system library they load
#   qemu/share/esp32c3-rom.bin, esp32s3_rev0_rom.bin   (the app passes -L qemu/share)
#   qemu/COPYING                            QEMU's licence
# Library install names are rewritten to @loader_path so the copy runs from any
# directory without Homebrew, then everything is ad-hoc signed again.
# `tauri build --config src-tauri/tauri.bundle.conf.json` copies qemu/ into the
# app's resource directory. macOS only for now; see README.md, Packaging.
set -eu
desktop=$(cd "$(dirname "$0")/.." && pwd)
root=$(cd "$desktop/../.." && pwd)
build=${EINK_QEMU_BUILD:-$root/build}
out=$desktop/src-tauri/qemu

case "$(uname -s)" in
    Darwin) ;;
    *) echo "bundle-qemu.sh: only macOS is supported so far (see README.md, Packaging)" >&2; exit 2 ;;
esac
for name in qemu-system-riscv32 qemu-system-xtensa; do
    [ -x "$build/$name" ] || { echo "bundle-qemu.sh: $build/$name is missing; build QEMU first" >&2; exit 1; }
done

rm -rf "$out"
mkdir -p "$out/bin" "$out/lib" "$out/share"
cp "$build/qemu-system-riscv32" "$build/qemu-system-xtensa" "$out/bin/"
# The X3/X4 (ESP32-C3) and X4 Pro/Picco (ESP32-S3) mask ROMs, from qemu_find_file().
cp "$root/pc-bios/esp32c3-rom.bin" "$root/pc-bios/esp32s3_rev0_rom.bin" "$out/share/"
cp "$root/COPYING" "$out/COPYING"

python3 - "$out" <<'PY'
import os, re, shutil, subprocess, sys

out = sys.argv[1]
bindir, libdir = os.path.join(out, 'bin'), os.path.join(out, 'lib')
SYSTEM = ('/usr/lib/', '/System/')
HOMEBREW_LIB = '/opt/homebrew/lib'
# Libraries opened with dlopen() that otool cannot see: (Homebrew name, bundled
# name). QEMU links SDL2 even with -display eink, and Homebrew's sdl2-compat
# opens SDL3 from its load-time initializer, trying @loader_path/libSDL3.dylib
# first; if that fails it blocks on a modal "Failed loading SDL3" alert.
DLOPENED = {'libSDL2-2.0.0.dylib': [('libSDL3.0.dylib', 'libSDL3.dylib')]}

def run(*args):
    return subprocess.run(args, check=True, capture_output=True, text=True).stdout

def deps(path):
    lines = run('otool', '-L', path).splitlines()[1:]
    names = [line.strip().split(' (compatibility')[0] for line in lines]
    own = run('otool', '-D', path).splitlines()[1:]  # a dylib lists its own id first
    return [name for name in names if name not in own]

def rpaths(path):
    text = run('otool', '-l', path)
    return re.findall(r'cmd LC_RPATH\n\s+cmdsize \d+\n\s+path (.+?) \(offset', text)

def resolve(name, referrer):
    """Host path of an install name as referenced from `referrer` (a real path)."""
    here = os.path.dirname(referrer)
    if name.startswith('@loader_path/') or name.startswith('@executable_path/'):
        candidates = [os.path.join(here, name.split('/', 1)[1])]
    elif name.startswith('@rpath/'):
        rest = name[len('@rpath/'):]
        candidates = [os.path.join(rp.replace('@loader_path', here).replace('@executable_path', here), rest)
                      for rp in rpaths(referrer)] + [os.path.join(HOMEBREW_LIB, rest)]
    else:
        candidates = [name]
    for candidate in candidates:
        if os.path.exists(candidate):
            return os.path.realpath(candidate)
    raise SystemExit(f'bundle-qemu.sh: cannot resolve {name} (from {referrer})')

# Breadth-first over the binaries and every library they pull in. Each bundled
# library is named after the last component of its install name.
bundled = {}   # basename -> host real path
sources = {}   # bundled file -> host real path it was copied from
queue = []
for name in sorted(os.listdir(bindir)):
    path = os.path.join(bindir, name)
    sources[path] = os.path.realpath(path)
    queue.append(path)
changes = {}   # bundled file -> [(old install name, basename)]
while queue:
    path = queue.pop(0)
    changes[path] = []
    for dep in deps(sources[path]):
        if dep.startswith(SYSTEM):
            continue
        base = os.path.basename(dep)
        changes[path].append((dep, base))
        if base not in bundled:
            real = resolve(dep, sources[path])
            bundled[base] = real
            target = os.path.join(libdir, base)
            shutil.copyfile(real, target)
            os.chmod(target, 0o755)
            sources[target] = real
            queue.append(target)
            for source, extra in DLOPENED.get(base, []):
                if extra not in bundled:
                    real_extra = resolve(os.path.join(HOMEBREW_LIB, source), real)
                    bundled[extra] = real_extra
                    extra_target = os.path.join(libdir, extra)
                    shutil.copyfile(real_extra, extra_target)
                    os.chmod(extra_target, 0o755)
                    sources[extra_target] = real_extra
                    queue.append(extra_target)

for path, refs in changes.items():
    in_lib = os.path.dirname(path) == libdir
    args = ['install_name_tool']
    if in_lib:
        args += ['-id', '@loader_path/' + os.path.basename(path)]
    for old, base in refs:
        args += ['-change', old, ('@loader_path/' if in_lib else '@loader_path/../lib/') + base]
    # Drop search paths into the build machine (Homebrew, the build tree).
    for rp in rpaths(path):
        if not rp.startswith('@'):
            args += ['-delete_rpath', rp]
    if len(args) > 1:
        subprocess.run(args + [path], check=True, capture_output=True)

# install_name_tool invalidates signatures; arm64 refuses unsigned code.
for path in sorted(changes, key=lambda p: os.path.dirname(p) != libdir):
    subprocess.run(['codesign', '--force', '-s', '-', path], check=True, capture_output=True)

# Nothing may still point outside the bundle or the system.
for path in changes:
    for dep in deps(path):
        if not (dep.startswith(SYSTEM) or dep.startswith('@loader_path/')):
            raise SystemExit(f'bundle-qemu.sh: {path} still references {dep}')
print(f'bundled {len(bundled)} libraries into {libdir}')
PY

du -sh "$out" | sed 's/^/qemu bundle: /'
