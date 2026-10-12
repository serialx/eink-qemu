#!/bin/sh
# Boot an X4 Pro, X4 or X3 flash image in eink-qemu. USB-CDC console (the firmware log) -> stdout.
# usage: eink-qemu/run.sh flash.bin [sd.img] [extra qemu args...]
# The machine follows the image's chip (byte 12 of the bootloader header): ESP32-S3 -> x4pro,
# ESP32-C3 -> x3. X4MACHINE=x3|x4|x4pro|picco overrides.
# X4PANEL=uc8279-68 selects the measured X4 Pro panel revision (default: uc8179).
# Wi-Fi: the fake AP "PICSimLabWifi" is bridged to a QEMU NIC.
#   X4BR=br0: on your LAN (router DHCP); needs "allow br0" in /usr/local/etc/qemu/bridge.conf, run as root.
#   default: QEMU NAT (10.0.2.15), host :8080 -> device :80. X4NET overrides both.
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
[ -n "$X4BR" ] && : "${X4NET=-nic bridge,br=$X4BR,helper=$(dirname "$qemu")/qemu-bridge-helper,model=esp32_wifi}"
[ -n "$X4BR" ] && echo "eink-qemu: Wi-Fi bridged to $X4BR (LAN DHCP)" >&2 || echo "eink-qemu: Wi-Fi on QEMU NAT (10.0.2.15); X4BR=br0 for your LAN" >&2
net=${X4NET--nic user,model=esp32_wifi,hostfwd=tcp::8080-:80}
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
  $net "$@"
