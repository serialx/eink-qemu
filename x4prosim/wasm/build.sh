#!/usr/bin/env bash
# Build the X3 and X4 Pro system emulators with Emscripten and the wasm64 TCG backend.
# Run build-deps.sh first. Build artifacts and compiler caches stay in the repo.
set -euo pipefail
WEB=0
if [ "${1:-}" = --web ]; then
    WEB=1
    shift
fi
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
SYSROOT="${WASM_SYSROOT:-$ROOT/build-wasm-deps/sysroot-wasm64}"
if [[ ! -d "$SYSROOT/lib/pkgconfig" ]]; then
    echo "Run $ROOT/x4prosim/wasm/build-deps.sh first" >&2
    exit 1
fi
SYSROOT=$(cd "$SYSROOT" && pwd)
WASM_SYSROOT="$SYSROOT" "$ROOT/x4prosim/wasm/build-deps.sh" --prepare-paths
JOBS=${JOBS:-$(getconf _NPROCESSORS_ONLN 2>/dev/null || echo 2)}
export EM_CACHE="${EM_CACHE:-$ROOT/build-wasm-deps/em-cache}"
export PKG_CONFIG_PATH="$SYSROOT/lib/pkgconfig"
export PKG_CONFIG_LIBDIR="$PKG_CONFIG_PATH"
export EM_PKG_CONFIG_PATH="$PKG_CONFIG_PATH"
export EM_PKG_CONFIG_LIBDIR="$PKG_CONFIG_LIBDIR"
export CPATH="$SYSROOT/include"
# The 32-bit memory mode had the lower Node boot median and needs no Memory64.
case "${WASM64_MODE:-32}" in
    64) MEMORY64=1; BUILD="$ROOT/build-wasm"; MODE_FLAG=--disable-wasm64-32bit-address-limit ;;
    32) MEMORY64=2; BUILD="$ROOT/build-wasm-32limit"; MODE_FLAG=--enable-wasm64-32bit-address-limit ;;
    *) echo 'WASM64_MODE must be 64 or 32' >&2; exit 1 ;;
esac
# MMIO under icount exits through longjmp frequently. Keep those exits in Wasm
# instead of crossing into JavaScript exception handlers on every access.
export CFLAGS="-O3 -pthread -DWASM_BIGINT -sMEMORY64=$MEMORY64 -sSUPPORT_LONGJMP=wasm"
export CXXFLAGS="$CFLAGS"
export LDFLAGS="-pthread -sWASM_BIGINT -sASYNCIFY=1 -sMEMORY64=$MEMORY64 -sSUPPORT_LONGJMP=wasm -L$SYSROOT/lib"
command -v emcc >/dev/null
pkg-config --print-errors --exists glib-2.0 pixman-1 zlib libffi libgcrypt || {
    echo "Run $ROOT/x4prosim/wasm/build-deps.sh first" >&2
    exit 1
}

# Retain the TCI port's pthread, fiber/libffi and filesystem runtime settings.
# Both address modes share the wasm64 dependency ABI.
LINK_FLAGS="-O3 -g2 $LDFLAGS -sPROXY_TO_PTHREAD=1 -sFORCE_FILESYSTEM=1"
LINK_FLAGS+=" -sALLOW_TABLE_GROWTH=1"
# Fixed memory avoids a shared-heap view check at each JS heap access.
# Larger workloads can raise the initial size or opt back into growth.
LINK_FLAGS+=" -sALLOW_MEMORY_GROWTH=${WASM_MEMORY_GROWTH:-0}"
LINK_FLAGS+=" -sINITIAL_MEMORY=${WASM_INITIAL_MEMORY:-268435456}"
LINK_FLAGS+=" -sMAXIMUM_MEMORY=2147483648"
LINK_FLAGS+=" -sSTACK_SIZE=8388608 -sASYNCIFY_STACK_SIZE=1048576"
LINK_FLAGS+=" -sEXIT_RUNTIME=1 -sEXPORT_ES6=0 -sENVIRONMENT=web,worker,node -sASYNCIFY_IMPORTS=ffi_call_js"
LINK_FLAGS+=" -sEXPORTED_RUNTIME_METHODS=addFunction,removeFunction,TTY,FS"
LINK_FLAGS+=" -lnodefs.js"
LINK_FLAGS+=" --js-library $ROOT/x4prosim/wasm/clock-origin.js"
LINK_FLAGS+=" --pre-js $ROOT/x4prosim/wasm/net.js"
if [[ -f "$ROOT/x4prosim/wasm/node-fs.js" ]]; then
    LINK_FLAGS+=" --pre-js $ROOT/x4prosim/wasm/node-fs.js"
fi
mkdir -p "$BUILD"
cd "$BUILD"
emconfigure ../configure \
    --static --cpu=wasm64 --disable-tools "$MODE_FLAG" \
    --target-list=xtensa-softmmu,riscv32-softmmu \
    --enable-sdl --disable-sdl-image --disable-opengl \
    --enable-gcrypt --disable-slirp --disable-vnc --disable-gtk \
    --disable-docs --disable-user --disable-capstone --disable-werror \
    --disable-gnutls --disable-nettle --disable-rust --disable-plugins \
    --disable-strip --disable-pie --disable-debug-info \
    -Doptimization=3 -Dc_args="$CFLAGS" -Dcpp_args="$CXXFLAGS" \
    -Dc_link_args="$LINK_FLAGS" -Dcpp_link_args="$LINK_FLAGS" \
    "$@"
# Fail early if a caller accidentally switched this into an interpreter build.
grep -q '^#define HOST_WASM64 1' config-host.h
if grep -q '^#define CONFIG_TCG_INTERPRETER 1' config-host.h; then
    echo 'Expected the wasm64 TCG backend, but configure selected TCI' >&2
    exit 1
fi
ninja -j"$JOBS" qemu-system-riscv32.js qemu-system-xtensa.js

if [ "$WEB" = 1 ]; then
    WASM_BUILD_DIR="$BUILD" "$ROOT/x4prosim/wasm/package-web.sh"
fi
