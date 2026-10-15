#!/usr/bin/env bash

set -euo pipefail

TARGET=${TARGET:-xtensa-softmmu}
VERSION=${VERSION:-dev}

echo DBG
./configure --help

# Use Meson's werror option on Linux so feature probes do not inherit -Werror.
# Defining --bindir, --datadir, etc - to have the same directory tree on Linux and Windows
#   also adding --with-suffix="" to avoid doubled "qemu/qemu" path.
#
#   MinGW build ref: https://github.com/msys2/MINGW-packages/blob/master/mingw-w64-qemu/PKGBUILD

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
    --enable-werror \
    --prefix=${PWD}/install/qemu \
    --target-list=${TARGET} \
    --with-pkgversion="${VERSION}" \
    --with-suffix="" \
    --without-default-features \
|| { cat build/meson-logs/meson-log.txt 2>/dev/null || true; exit 1; }
