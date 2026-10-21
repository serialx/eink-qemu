#!/bin/sh
# Stage the native QEMU install tree, CLI, and service for the Debian package.
set -eu
desktop=$(cd "$(dirname "$0")/.." && pwd)
source=${1:?usage: bundle-qemu-linux.sh <QEMU install tree> <Rust target>}
target=${2:?pass x86_64-unknown-linux-gnu or aarch64-unknown-linux-gnu}
: "${EINK_QEMU_CLI:?set EINK_QEMU_CLI to the matching CLI binary}"
: "${EINK_BRIDGE_HELPER:?set EINK_BRIDGE_HELPER to the matching bridge service binary}"
python3 "$desktop/scripts/bridge/stage_linux.py" "$source" "$target" \
    --cli "$EINK_QEMU_CLI" --helper "$EINK_BRIDGE_HELPER"
