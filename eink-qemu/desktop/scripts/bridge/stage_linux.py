#!/usr/bin/env python3
"""Stage a Debian desktop bundle without changing an existing successful bundle."""
from __future__ import annotations

import argparse
import json
from pathlib import Path
import re
import shutil
import struct
import subprocess
import tempfile

MACHINES = {"x86_64-unknown-linux-gnu": 62, "aarch64-unknown-linux-gnu": 183}


def regular(path: Path) -> None:
    if path.is_symlink() or not path.is_file():
        raise ValueError(f"Missing regular payload file: {path}")


def elf(path: Path, target: str, readelf: str) -> dict:
    regular(path)
    with path.open("rb") as source:
        header = source.read(64)
    if (len(header) != 64 or header[:6] != b"\x7fELF\x02\x01"
            or struct.unpack_from("<H", header, 18)[0] != MACHINES[target]):
        raise ValueError(f"Payload does not match {target}: {path}")
    # Inspect metadata; never execute a payload or use ldd on staged inputs.
    result = subprocess.run([readelf, "--wide", "--dynamic", "--version-info", str(path)],
                            check=True, capture_output=True, text=True)
    versions = {tuple(map(int, value.split("."))) for value in re.findall(r"\bGLIBC_(\d+(?:\.\d+)+)\b", result.stdout)}
    if any(value > (2, 35) for value in versions):
        raise ValueError(f"Payload requires a newer libc than Ubuntu 22.04: {path}")
    if re.search(r"\((?:RPATH|RUNPATH)\)", result.stdout):
        raise ValueError(f"Payload contains an unexpected library search path: {path}")
    return {"needed": sorted(set(re.findall(r"\(NEEDED\).*?\[([^\]]+)\]", result.stdout))),
            "glibc": ".".join(map(str, max(versions))) if versions else None}


def stage(source: Path, output: Path, repo: Path, target: str, cli: Path, helper: Path,
          readelf: str = "readelf") -> None:
    if target not in MACHINES:
        raise ValueError("Linux desktop packaging requires x64 or ARM64 GNU/Linux")
    if output.is_symlink() or (output.exists() and not output.is_dir()):
        raise ValueError("Bundle destination must be a real directory")
    binaries = {
        "qemu/bin/qemu-system-riscv32": source / "bin/qemu-system-riscv32",
        "qemu/bin/qemu-system-xtensa": source / "bin/qemu-system-xtensa",
        "qemu/plugins/libheaptrace.so": source / "plugins/libheaptrace.so",
        "bridge/eink-qemu-bridge-helper": source / "libexec/eink-qemu-bridge-helper",
        "bridge/eink-bridge-helper": helper,
        f"binaries/eink-qemu-{target}": cli,
    }
    data = {
        "qemu/share/esp32c3-rom.bin": repo / "pc-bios/esp32c3-rom.bin",
        "qemu/share/esp32s3_rev0_rom.bin": repo / "pc-bios/esp32s3_rev0_rom.bin",
        "qemu/COPYING": repo / "COPYING",
        "bridge/COPYING": repo / "COPYING",
    }
    # Every prerequisite is checked before creating or renaming the output.
    metadata = {name: elf(path, target, readelf) for name, path in binaries.items()}
    for path in data.values():
        regular(path)
    output.parent.mkdir(parents=True, exist_ok=True)
    staging = Path(tempfile.mkdtemp(prefix=f".{output.name}-stage-", dir=output.parent))
    backup = None
    installed = False
    try:
        for name, path in {**binaries, **data}.items():
            dest = staging / name
            dest.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(path, dest)
            # Staging never grants privileges. The package lifecycle installs
            # only its fixed dedicated packet helper with the required mode.
            dest.chmod(0o755 if name in binaries else 0o644)
        (staging / "native-dependencies.json").write_text(json.dumps(
            {"target": target, "baseline": "Ubuntu 22.04", "files": metadata}, indent=2) + "\n")
        if output.exists():
            backup = Path(tempfile.mkdtemp(prefix=f".{output.name}-previous-", dir=output.parent))
            backup.rmdir()
            output.rename(backup)
        try:
            staging.rename(output)
            installed = True
        except BaseException:
            if backup is not None:
                backup.rename(output)
                backup = None
            raise
    finally:
        if staging.exists():
            shutil.rmtree(staging)
        if backup is not None and installed:
            shutil.rmtree(backup)


def main() -> None:
    desktop = Path(__file__).resolve().parents[2]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path)
    parser.add_argument("target", choices=MACHINES)
    parser.add_argument("--cli", required=True, type=Path)
    parser.add_argument("--helper", required=True, type=Path)
    parser.add_argument("--output", type=Path, default=desktop / "src-tauri/linux")
    args = parser.parse_args()
    stage(args.source, args.output, desktop.parents[1], args.target, args.cli, args.helper)


if __name__ == "__main__":
    main()
