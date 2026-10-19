#!/bin/sh
# Boot an X4 Pro, X4 or X3 flash image in eink-qemu. USB-CDC console (the firmware log) -> stdout.
# usage: eink-qemu/run.sh flash.bin [sd.img] [extra qemu args...]
# The machine follows the image's chip (byte 12 of the bootloader header): ESP32-S3 -> x4pro,
# ESP32-C3 -> x3. X4MACHINE=x3|x4|x4pro|picco overrides.
# X4 Pro defaults to measured UC8279 revision 0x68; X4PANEL=uc8179 selects the legacy panel.
# Wi-Fi: "PICSimLabWifi" uses NAT, forwarding host port 8080 to guest port 80.
#   X4WIFI_MODE=bridge opts into LAN bridging; X4BR selects the interface.
#   Bridge defaults: Linux br0, macOS en0, Windows TAP adapter eink-qemu.
#   X4NET overrides networking; see EINK-QEMU.md for bridge host setup.
# Monitor: unix socket /tmp/eink-qemu-mon.sock (HMP). GDB: add "-s -S".
set -e
here=$(cd "$(dirname "$0")/.." && pwd)
img=$1; sd=${2:-sd.img}; shift; [ $# -gt 0 ] && shift
[ -f "$sd" ] || python3 "$here/eink-qemu/mksd.py" "$sd"
# QEMU writes to the flash image (NVS, OTA data); run a copy to keep the source clean.
cp "$img" "$img.run"
chip=$(od -An -tu1 -j12 -N1 "$img" | tr -d ' ')
: "${X4MACHINE:=$([ "$chip" = 5 ] && echo x3 || echo x4pro)}"
if [ "$X4MACHINE" = x3 ] || [ "$X4MACHINE" = x4 ]; then
  qemu=$here/build/qemu-system-riscv32
  icount_shift=0
  echo "eink-qemu: $X4MACHINE (ESP32-C3); keys: arrows, Enter, Backspace, P" >&2
else
  qemu=$here/build/qemu-system-xtensa
  icount_shift=2
  [ "$X4MACHINE" = x4pro ] && icount_shift=0
fi
# release archives have the binaries in bin/ instead of build/
[ -x "$qemu" ] || qemu=$here/bin/${qemu##*/}
if [ "${X4NET+x}" = x ]; then
  set -- $X4NET "$@"
  echo "eink-qemu: Wi-Fi configured by X4NET" >&2
else
  mode=${X4WIFI_MODE:-nat}
  # Preserve the original X4BR opt-in; an explicit mode takes precedence.
  if [ -z "${X4WIFI_MODE+x}" ] && [ -n "${X4BR:-}" ]; then mode=bridge; fi
  case "$mode" in
  nat)
    nic='user,model=esp32_wifi,hostfwd=tcp::8080-:80'
    echo 'eink-qemu: Wi-Fi NAT (guest port 80 at localhost:8080)' >&2
    ;;
  bridge)
    # Commas inside QEMU option values are doubled; spaces stay in one argument.
    case "$(uname -s)" in
      Darwin)
        bridge=${X4BR:-en0}
        nic="vmnet-bridged,ifname=$(printf '%s' "$bridge" | sed 's/,/,,/g'),model=esp32_wifi"
        ;;
      MINGW*|MSYS*|CYGWIN*)
        bridge=${X4BR:-eink-qemu}
        nic="tap,ifname=$(printf '%s' "$bridge" | sed 's/,/,,/g'),model=esp32_wifi"
        ;;
      *)
        bridge=${X4BR:-br0}
        nic="bridge,br=$(printf '%s' "$bridge" | sed 's/,/,,/g'),model=esp32_wifi"
        helper=$(dirname "$qemu")/qemu-bridge-helper
        # Build trees keep the helper beside QEMU; installed QEMU locates libexec itself.
        if [ -x "$helper" ]; then
          # QEMU treats a helper path containing spaces/tabs as a shell command.
          case "$helper" in
            *' '*|*'	'*) helper="'$(printf '%s' "$helper" | sed "s/'/'\\\\''/g")'" ;;
          esac
          nic="$nic,helper=$(printf '%s' "$helper" | sed 's/,/,,/g')"
        fi
        ;;
    esac
    echo "eink-qemu: Wi-Fi bridged through $bridge (LAN DHCP)" >&2
    ;;
  *) echo 'X4WIFI_MODE must be nat or bridge' >&2; exit 2 ;;
  esac
  set -- -nic "$nic" "$@"
fi
# Calibrated X3/X4/X4 Pro costs use 1 ns ticks; Picco keeps 4 ns ticks.
case "${X4TURBO:-0}" in
  0) set -- -icount shift=$icount_shift,sleep=on "$@" ;;
  1)
    # The helper emits whitespace-free arguments, one per line.
    set -- -icount shift=$icount_shift,sleep=off \
      $(sh "$here/eink-qemu/turbo-args.sh" "$X4MACHINE") "$@"
    ;;
  *) echo 'X4TURBO must be 0 or 1' >&2; exit 2 ;;
esac
machine_args=$X4MACHINE
if [ "$X4MACHINE" = x4pro ] && [ -n "${X4PANEL:-}" ]; then
  machine_args="$machine_args,panel=$X4PANEL"
fi
exec "$qemu" -machine "$machine_args" \
  -drive file="$img.run",if=mtd,format=raw \
  -drive file="$sd",if=sd,format=raw \
  -chardev stdio,id=cdc,mux=off -serial null \
  -global driver=misc.esp32s3.usb_serial_jtag,property=chardev,value=cdc \
  -monitor unix:/tmp/eink-qemu-mon.sock,server,nowait -display sdl,show-cursor=on \
  "$@"
