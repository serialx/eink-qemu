#!/usr/bin/env bash
# Headless Node launcher. Ctrl-a c switches between USB-CDC and the HMP monitor.
# Accurate timing is the default; TURBO=1 opts into fast, non-accurate timing.
# HMP screendump paths use /host followed by the absolute host path.
# X4 Pro defaults to measured UC8279 revision 0x68; X4PANEL=uc8179 selects the legacy panel.
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
case "${WASM64_MODE:-32}" in
    64) build="$ROOT/build-wasm" ;;
    32) build="$ROOT/build-wasm-32limit" ;;
    *) echo 'WASM64_MODE must be 64 or 32' >&2; exit 2 ;;
esac
case "${RUN_NODE_SLEEP:-on}" in
    on|off) ;;
    *) echo 'RUN_NODE_SLEEP must be on or off' >&2; exit 2 ;;
esac
if [ "$#" -lt 2 ]; then
    echo "usage: $0 flash.bin sd.img [qemu args...]" >&2
    exit 2
fi
flash=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
sd=$(cd "$(dirname "$2")" && pwd)/$(basename "$2")
shift 2
chip=$(od -An -tu1 -j12 -N1 "$flash" | tr -d ' ')
machine=${X4MACHINE:-$([ "$chip" = 5 ] && echo x3 || echo x4pro)}
case "$machine" in
    x3|x4) arch=riscv32; shift_bits=0; accel=tcg,tb-size=64 ;;
    x4pro) arch=xtensa; shift_bits=0; accel=tcg,tb-size=64 ;;
    picco) arch=xtensa; shift_bits=2; accel=tcg,tb-size=64 ;;
    *) echo "unsupported machine: $machine" >&2; exit 2 ;;
esac
case "${TURBO:-0}" in
    0|1) ;;
    *) echo 'TURBO must be 0 or 1' >&2; exit 2 ;;
esac
cp "$flash" "$flash.run"
chmod u+w "$flash.run"
timing=(-icount "shift=$shift_bits,sleep=${RUN_NODE_SLEEP:-on}")
# Keep the radio hardware present even without a host network adapter.
network=(-nic hubport,hubid=0,model=esp32_wifi)
if [ -n "${WIFI_ASSETS:-}" ]; then
    network=(-nic browser,model=esp32_wifi)
fi
if [ "${TURBO:-0}" = 1 ]; then
    timing=(-icount "shift=$shift_bits,sleep=off")
    while IFS= read -r arg; do
        timing+=("$arg")
    done < <(sh "$ROOT/eink-qemu/turbo-args.sh" "$machine")
fi
machine_args=$machine
if [ "$machine" = x4pro ] && [ -n "${X4PANEL:-}" ]; then
    machine_args="$machine,panel=$X4PANEL"
fi
exec "${NODE:-node}" "${WASM_BUILD_DIR:-$build}/qemu-system-$arch.js" \
    -L "/host$ROOT/pc-bios" \
    -accel "${WASM_ACCEL:-$accel}" \
    -machine "$machine_args" \
    "${timing[@]}" \
    -drive "file=/host${flash//,/,,}.run,if=mtd,format=raw,cache.direct=off" \
    -drive "file=/host${sd//,/,,},if=sd,format=raw,cache.direct=off" \
    -chardev stdio,id=cdc,mux=on -serial null \
    -global driver=misc.esp32s3.usb_serial_jtag,property=chardev,value=cdc \
    -mon chardev=cdc,mode=readline -display none "${network[@]}" "$@"
