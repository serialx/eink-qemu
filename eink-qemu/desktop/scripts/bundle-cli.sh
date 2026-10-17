#!/bin/sh
# Tauri removes the target suffix when installing this sidecar beside the app.
set -eu
desktop=$(cd "$(dirname "$0")/.." && pwd)
cd "$desktop"
host=$(rustc -vV | sed -n 's/^host: //p')
target=${1:-$host}
ext=
case "$target" in
    *-windows-*)
        ext=.exe
        export CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS="${CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS:--C target-feature=+crt-static}"
        ;;
esac
if [ -n "${EINK_QEMU_CLI:-}" ]; then
    binary=$EINK_QEMU_CLI
else
    if [ "$target" = x86_64-pc-windows-msvc ] && [ "$host" != "$target" ]; then
        cargo xwin build --locked --release -p eink-qemu-cli --target "$target"
    else
        cargo build --locked --release -p eink-qemu-cli --target "$target"
    fi
    binary=${CARGO_TARGET_DIR:-target}/$target/release/eink-qemu$ext
fi
mkdir -p src-tauri/binaries
cp "$binary" "src-tauri/binaries/eink-qemu-$target$ext"
