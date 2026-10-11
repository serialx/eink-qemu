#!/usr/bin/env bash

set -euo pipefail
# eink-qemu: no -Werror on Windows: newer MinGW GCC warns in upstream test code (libqtest.c)

TARGET=${TARGET:-xtensa-softmmu}
VERSION=${VERSION:-dev}

echo DBG
./configure --help

./configure \
    --bindir=bin \
    --datadir=share/qemu \
    --enable-gcrypt \
    --enable-sdl \
    --enable-pixman \
    --enable-png \
    --enable-slirp \
    --enable-stack-protector \
    --disable-werror \
    --prefix=${PWD}/install/qemu \
    --static \
    --target-list=${TARGET} \
    --with-pkgversion="${VERSION}" \
    --with-suffix="" \
    --without-default-features \
|| { cat build/meson-logs/meson-log.txt 2>/dev/null || true; exit 1; }


# Fix: pkg-config for libgcrypt outputs incorrect paths for libiconv and libintl:
# - Unix-style paths (/ucrt64/lib/...) instead of Windows paths (D:/a/_temp/msys64/ucrt64/lib/...)
# - Dynamic import libraries (.dll.a) instead of static libraries (.a)
# We need to fix both issues in build.ninja for the static build to work correctly.
MSYS_BASE=$(cygpath -w / | sed 's/\\/\//g')
sed -i "s|/ucrt64/lib/libintl.dll.a|${MSYS_BASE}/ucrt64/lib/libintl.a|g; s|/ucrt64/lib/libiconv.dll.a|${MSYS_BASE}/ucrt64/lib/libiconv.a|g" build/build.ninja
