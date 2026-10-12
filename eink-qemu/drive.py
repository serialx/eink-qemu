#!/usr/bin/env python3
"""Boot an X4 Pro, X4 or X3 image headless and run a script of steps against it.

usage: drive.py flash.bin sd.img log.txt STEP...
steps:  wait:SECONDS     press:KEY[:MS]     shot:FILE.png
        hmp:COMMAND      (any monitor command, output printed)
KEY: up|down|power on the X4 Pro; back|confirm|left|right|up|down|power on the X3/X4.
The machine follows the image's chip (X4MACHINE=x3|x4|x4pro|picco overrides).
X4PANEL=uc8279-68 selects the measured X4 Pro panel revision.
The firmware log (USB-CDC) goes to log.txt."""
import os, socket, subprocess, sys, time

here = os.path.dirname(os.path.abspath(__file__))
img, sd, log, steps = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4:]
with open(img, "rb") as f:
    chip = f.read(13)[12]    # bootloader image header: 5 = ESP32-C3, 9 = ESP32-S3
machine = os.environ.get("X4MACHINE") or ("x3" if chip == 5 else "x4pro")
qemu = os.path.join(here, "..", "build", "qemu-system-riscv32" if machine in ("x3", "x4") else "qemu-system-xtensa")
if not os.path.exists(qemu):    # release archives have the binaries in bin/
    qemu = os.path.join(here, "..", "bin", os.path.basename(qemu))
keys = "x3-keys" if machine in ("x3", "x4") else "picco-keys" if machine == "picco" else "x4pro-keys"
sock = f"/tmp/eink-qemu-{os.getpid()}.sock"
if not os.path.exists(sd):
    subprocess.run([sys.executable, os.path.join(here, "mksd.py"), sd], check=True)
subprocess.run(["cp", img, img + ".run"], check=True)
# X3 regional instruction costs are nanoseconds; Xtensa keeps 4 ns ticks.
icount_shift = 0 if machine in ("x3", "x4", "x4pro") else 2
machine_args = machine
if machine == "x4pro" and os.environ.get("X4PANEL"):
    machine_args += ",panel=" + os.environ["X4PANEL"]
p = subprocess.Popen([qemu, "-machine", machine_args,
                      "-icount", f"shift={icount_shift},sleep=on",
                      "-display", "none", "-serial", "null",
                      "-drive", f"file={img}.run,if=mtd,format=raw", "-drive", f"file={sd},if=sd,format=raw",
                      "-chardev", f"file,id=cdc,path={log}",
                      "-global", "driver=misc.esp32s3.usb_serial_jtag,property=chardev,value=cdc",
                      "-monitor", f"unix:{sock},server,nowait"] + os.environ.get("QEMU_EXTRA", "").split(),
                     stdout=subprocess.DEVNULL, stderr=sys.stderr)
for _ in range(50):
    if os.path.exists(sock):
        break
    time.sleep(0.1)
s = socket.socket(socket.AF_UNIX)
s.connect(sock)
s.settimeout(0.3)

def hmp(cmd):
    s.sendall(cmd.encode() + b"\n")
    out = b""
    try:
        while True:
            chunk = s.recv(65536)
            if not chunk:
                break
            out += chunk
    except socket.timeout:
        pass
    # HMP echoes the line with terminal escapes; keep only the reply lines.
    lines = out.decode(errors="replace").replace("\r", "").split("\n")
    return "\n".join(l for l in lines[1:] if not l.startswith("(qemu)")).strip()

hmp("")
try:
    for step in steps:
        kind, _, arg = step.partition(":")
        if kind == "wait":
            time.sleep(float(arg))
        elif kind == "press":
            key, _, ms = arg.partition(":")
            hmp(f"qom-set /machine/{keys} {key} true")
            time.sleep(int(ms or 150) / 1000)
            hmp(f"qom-set /machine/{keys} {key} false")
        elif kind == "shot":
            print(hmp(f"screendump {os.path.abspath(arg)} -f png panel") or f"shot {arg}")
        elif kind == "hmp":
            print(hmp(arg))
        else:
            sys.exit(f"unknown step {step}")
finally:
    p.kill()
    os.unlink(sock)
