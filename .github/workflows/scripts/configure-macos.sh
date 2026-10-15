#!/usr/bin/env bash

set -euo pipefail

TARGET=${TARGET:-xtensa-softmmu}
VERSION=${VERSION:-dev}

echo DBG
./configure --help

./configure \
    --bindir=bin \
    --datadir=share/qemu \
    --enable-fdt=internal \
    --enable-gcrypt \
    --enable-sdl \
    --enable-pixman \
    --enable-plugins \
    --enable-png \
    --enable-slirp \
    --enable-stack-protector \
    --prefix=$PWD/install/qemu \
    --python=python3 \
    --target-list=${TARGET} \
    --with-pkgversion="${VERSION}" \
    --with-suffix="" \
    --without-default-features \
|| { cat build/meson-logs/meson-log.txt 2>/dev/null || true; exit 1; }
