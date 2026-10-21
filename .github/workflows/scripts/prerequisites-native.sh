#!/usr/bin/env bash

set -euo pipefail

export DEBIAN_FRONTEND="noninteractive"

apt-get update -y -q
apt-get install -y -q --no-install-recommends \
    build-essential \
    ca-certificates \
    git \
    libgcrypt20-dev \
    libglib2.0-dev \
    libpixman-1-dev \
    libpng-dev \
    libsdl2-dev \
    libslirp-dev \
    ninja-build \
    pkg-config \
    python3 \
    python3-venv \
    xz-utils \
    zlib1g-dev \
&& :

# QEMU configure installs its bundled Meson into build/pyvenv.
