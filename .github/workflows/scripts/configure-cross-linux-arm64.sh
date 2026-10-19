#!/usr/bin/env bash

set -euo pipefail

TARGET=${TARGET:-xtensa-softmmu}
VERSION=${VERSION:-dev}

echo DBG
./configure --help

./configure \
    --bindir=bin \
    --cross-prefix=aarch64-linux-gnu- \
    --datadir=share/qemu \
    --enable-gcrypt \
    --enable-sdl \
    --enable-pixman \
    --enable-plugins \
    --enable-png \
    --enable-slirp \
    --enable-tools \
    --enable-stack-protector \
    --enable-werror \
    --prefix=${PWD}/install/qemu \
    --target-list=${TARGET} \
    --with-pkgversion="${VERSION}" \
    --with-suffix="" \
    --without-default-features \
|| { cat build/meson-logs/meson-log.txt 2>/dev/null || true; exit 1; }
