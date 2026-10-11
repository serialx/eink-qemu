#!/usr/bin/env bash
# A separate wasm32 lwIP + mbedTLS module, usable by both QEMU address modes.
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
DEPS="$ROOT/build-wasm-deps"
WORK="$DEPS/browser-net"
BUILD="$WORK/build"
# Do not inherit the QEMU wasm64 ABI flags when called from build.sh.
unset CFLAGS CXXFLAGS CPPFLAGS LDFLAGS CPATH
export EM_CACHE="${EM_CACHE:-$DEPS/em-cache}"
mkdir -p "$DEPS/src" "$WORK" "$BUILD"
# CMake caches record both directories and cannot be reused after copying a
# worktree. package-web.sh calls this entry point as well as standalone builds.
python3 - "$ROOT/eink-qemu/wasm/browser-net" "$BUILD" <<'PY'
from pathlib import Path
import shutil
import sys

source, build = map(lambda p: Path(p).resolve(), sys.argv[1:])
cache = build / 'CMakeCache.txt'
if cache.exists():
    entries = dict(line.split('=', 1) for line in cache.read_text().splitlines()
                   if '=' in line and not line.startswith(('#', '//')))
    if (entries.get('CMAKE_HOME_DIRECTORY:INTERNAL') != str(source) or
            entries.get('CMAKE_CACHEFILE_DIR:INTERNAL') != str(build)):
        print('Discarding browser-net CMake state from another directory')
        cache.unlink()
        shutil.rmtree(build / 'CMakeFiles', ignore_errors=True)
PY
fetch() {
    local file=$1 url=$2 sha=$3
    if [ ! -f "$DEPS/src/$file" ]; then
        curl -fL --retry 3 "$url" -o "$DEPS/src/$file.part"
        mv "$DEPS/src/$file.part" "$DEPS/src/$file"
    fi
    printf '%s  %s\n' "$sha" "$DEPS/src/$file" | shasum -a 256 -c -
}
fetch lwip-2.2.1.tar.gz https://github.com/lwip-tcpip/lwip/archive/refs/tags/STABLE-2_2_1_RELEASE.tar.gz ce0b7461c0ad9602c376f0bf07c5eb7253b48c7bf66f011c6bf3e2a96731c539
fetch mbedtls-3.6.7.tar.bz2 https://github.com/Mbed-TLS/mbedtls/releases/download/mbedtls-3.6.7/mbedtls-3.6.7.tar.bz2 a7e8bcbec0e6f761b4af24f25677626b35f762f68eef79c08677a363212d11f6
[ -d "$WORK/lwip-STABLE-2_2_1_RELEASE" ] || tar -xf "$DEPS/src/lwip-2.2.1.tar.gz" -C "$WORK"
[ -d "$WORK/mbedtls-3.6.7" ] || tar -xf "$DEPS/src/mbedtls-3.6.7.tar.bz2" -C "$WORK"
# This self-signed certificate is confined to the emulated network. Its private
# key is intentionally included in the module; it is never a trusted origin key.
if [ ! -f "$BUILD/certificate.h" ]; then
    openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes \
        -keyout "$BUILD/local-key.pem" -out "$BUILD/local-cert.pem" \
        -subj /CN=browser-emulator.invalid -days 3650
    python3 - "$BUILD" <<'PY'
from pathlib import Path
import json, sys
p = Path(sys.argv[1])
(p / 'certificate.h').write_text('\n'.join(
    f'static const char {name}[] = {json.dumps((p / file).read_text())};'
    for name, file in [('local_key', 'local-key.pem'), ('local_cert', 'local-cert.pem')]))
PY
fi
emcmake cmake -S "$ROOT/eink-qemu/wasm/browser-net" -B "$BUILD" -G Ninja \
    -DLWIP_DIR="$WORK/lwip-STABLE-2_2_1_RELEASE" -DMBEDTLS_DIR="$WORK/mbedtls-3.6.7"
cmake --build "$BUILD" -j "${JOBS:-8}"
cp "$ROOT/eink-qemu/wasm/web/browser-network.mjs" "$BUILD/"
cp "$WORK/lwip-STABLE-2_2_1_RELEASE/COPYING" "$BUILD/lwip.LICENSE.txt"
cp "$WORK/mbedtls-3.6.7/LICENSE" "$BUILD/mbedtls.LICENSE.txt"
