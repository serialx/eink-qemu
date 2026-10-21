#!/bin/sh
# Stage native QEMU for the app bundle in src-tauri/qemu:
#   qemu/bin/qemu-system-{riscv32,xtensa}   from <repo>/build (or $EINK_QEMU_BUILD)
#   qemu/plugins/libheaptrace.dylib         the heap view's allocation tracer, if built
#   qemu/lib/*.dylib                        every non-system library they load
#   qemu/share/esp32c3-rom.bin, esp32s3_rev0_rom.bin   (the app passes -L qemu/share)
#   qemu/COPYING                            QEMU's licence
# Library install names are rewritten to @loader_path so the copy runs from any
# directory without Homebrew, then everything is signed again. Set
# APPLE_SIGNING_IDENTITY for an Apple-issued signature; development defaults
# to ad-hoc signing. Signed QEMU processes receive only the JIT entitlement.
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
    case "$("$build/$name" -machine none -netdev help)" in
        *vmnet-bridged*) ;;
        *) echo "bundle-qemu.sh: $name needs --enable-vmnet for bridged Wi-Fi" >&2; exit 1 ;;
    esac
done

# The plugin is in contrib/plugins of a build tree, or plugins/ beside the bin/
# of an installed release archive.
plugin=
for candidate in "$build/contrib/plugins/libheaptrace.dylib" "$build/../plugins/libheaptrace.dylib"; do
    [ -f "$candidate" ] && { plugin=$candidate; break; }
done

rm -rf "$out"
mkdir -p "$out/bin" "$out/lib" "$out/share" "$out/plugins"
cp "$build/qemu-system-riscv32" "$build/qemu-system-xtensa" "$out/bin/"
if [ -n "$plugin" ]; then
    cp "$plugin" "$out/plugins/"
else
    echo "bundle-qemu.sh: no libheaptrace.dylib (QEMU built without --enable-plugins?); the heap view will not record allocation stacks" >&2
fi
# The X3/X4 (ESP32-C3) and X4 Pro/Picco (ESP32-S3) mask ROMs, from qemu_find_file().
cp "$root/pc-bios/esp32c3-rom.bin" "$root/pc-bios/esp32s3_rev0_rom.bin" "$out/share/"
cp "$root/COPYING" "$out/COPYING"

python3 - "$out" "$build" "$plugin" "$desktop/scripts/bridge/macos/qemu-entitlements.plist" <<'PY'
import os, re, shutil, subprocess, sys

out, build, plugin, entitlements = sys.argv[1:]
bindir, libdir, plugindir = os.path.join(out, 'bin'), os.path.join(out, 'lib'), os.path.join(out, 'plugins')
SYSTEM = ('/usr/lib/', '/System/')

def homebrew_lib():
    """Homebrew's lib directory: /opt/homebrew on Apple silicon, /usr/local on Intel."""
    try:
        prefix = subprocess.run(['brew', '--prefix'], check=True, capture_output=True, text=True).stdout.strip()
        if prefix:
            return os.path.join(prefix, 'lib')
    except (OSError, subprocess.CalledProcessError):
        pass
    return '/opt/homebrew/lib' if os.path.isdir('/opt/homebrew/lib') else '/usr/local/lib'

HOMEBREW_LIB = homebrew_lib()
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

def resolve(name, referrer, executable):
    """Host path of an install name as referenced from `referrer` (a real
    path) in a process started from `executable` (the original binary)."""
    here, exe = os.path.dirname(referrer), os.path.dirname(executable)
    if name.startswith('@loader_path/'):
        candidates = [os.path.join(here, name.split('/', 1)[1])]
    elif name.startswith('@executable_path/'):
        candidates = [os.path.join(exe, name.split('/', 1)[1])]
    elif name.startswith('@rpath/'):
        rest = name[len('@rpath/'):]
        candidates = [os.path.join(rp.replace('@loader_path', here).replace('@executable_path', exe), rest)
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
executables = {}  # bundled file -> the original binary whose process loads it
queue = []
for name in sorted(os.listdir(bindir)):
    path = os.path.join(bindir, name)
    # Resolve loader-relative names from the original build binary, not the copy.
    sources[path] = os.path.realpath(os.path.join(build, name))
    executables[path] = sources[path]
    queue.append(path)
if plugin:
    # QEMU loads the plugin; its own references resolve like a binary's.
    path = os.path.join(plugindir, os.path.basename(plugin))
    sources[path] = os.path.realpath(plugin)
    executables[path] = sources[os.path.join(bindir, 'qemu-system-xtensa')]
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
            real = resolve(dep, sources[path], executables[path])
            bundled[base] = real
            target = os.path.join(libdir, base)
            shutil.copyfile(real, target)
            os.chmod(target, 0o755)
            sources[target] = real
            executables[target] = executables[path]
            queue.append(target)
            for source, extra in DLOPENED.get(base, []):
                if extra not in bundled:
                    # Beside the library that opens it first, then Homebrew's lib.
                    beside = os.path.join(os.path.dirname(real), source)
                    real_extra = resolve(beside if os.path.exists(beside) else os.path.join(HOMEBREW_LIB, source), real, executables[path])
                    bundled[extra] = real_extra
                    extra_target = os.path.join(libdir, extra)
                    shutil.copyfile(real_extra, extra_target)
                    os.chmod(extra_target, 0o755)
                    sources[extra_target] = real_extra
                    executables[extra_target] = executables[path]
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
identity = os.environ.get('APPLE_SIGNING_IDENTITY') or '-'
for path in sorted(changes, key=lambda p: os.path.dirname(p) != libdir):
    args = ['codesign', '--force', '-s', identity]
    if identity != '-' and os.path.dirname(path) == bindir:
        args += ['--options', 'runtime', '--entitlements', entitlements]
    subprocess.run(args + [path], check=True, capture_output=True)
    subprocess.run(['codesign', '--verify', '--strict', path], check=True, capture_output=True)

# Nothing may still point outside the bundle or the system.
for path in changes:
    for dep in deps(path):
        if not (dep.startswith(SYSTEM) or dep.startswith('@loader_path/')):
            raise SystemExit(f'bundle-qemu.sh: {path} still references {dep}')
print(f'bundled {len(bundled)} libraries into {libdir}' + (', and the heaptrace plugin' if plugin else ''))
PY

du -sh "$out" | sed 's/^/qemu bundle: /'

sh "$desktop/scripts/bundle-cli.sh"
