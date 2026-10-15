#!/usr/bin/env bash

set -euo pipefail
# eink-qemu: no -Werror on Windows: newer MinGW GCC warns in upstream test code (libqtest.c)
# eink-qemu: a dynamic build, since QEMU loads TCG plugins (the heap view's
# heaptrace) only into one; bundle-win-dlls.sh ships the DLLs.

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
    --enable-plugins \
    --enable-png \
    --enable-slirp \
    --enable-stack-protector \
    --disable-werror \
    --prefix=${PWD}/install/qemu \
    --target-list=${TARGET} \
    --with-pkgversion="${VERSION}" \
    --with-suffix="" \
    --without-default-features \
|| { cat build/meson-logs/meson-log.txt 2>/dev/null || true; exit 1; }
