#!/usr/bin/env python3
"""Verify signed bridge packaging and the deployment floor of every QEMU binary."""
import argparse
import hashlib
import json
from pathlib import Path
import plistlib
import re
import subprocess
import sys

from stage_macos import ARCHITECTURES, LABEL, requirement


def version(value: str) -> tuple[int, int, int]:
    parts = value.split(".")
    if not 1 <= len(parts) <= 3 or not all(part.isdigit() for part in parts):
        raise ValueError(f"Invalid macOS version: {value}")
    return tuple(int(part) for part in parts) + (0,) * (3 - len(parts))


def verify(bundle: Path, target: str, team: str) -> None:
    contents = bundle / "Contents"
    info = plistlib.loads((contents / "Info.plist").read_bytes())
    if info["CFBundleIdentifier"] != "net.serialx.eink-qemu":
        raise ValueError("Unexpected desktop application identifier")
    expected = requirement(LABEL, team)
    if info.get("SMPrivilegedExecutables") != {LABEL: expected}:
        raise ValueError("The desktop does not require the expected signed helper")
    helper = contents / "Library/LaunchServices" / LABEL
    for path, identity in ((bundle, requirement("net.serialx.eink-qemu", team)), (helper, expected)):
        subprocess.run(["codesign", "--verify", "--strict", "-R", "=" + identity, str(path)], check=True)
    subprocess.run(["codesign", "--verify", "--strict", "--deep", str(bundle)], check=True)
    manifest = json.loads((Path(__file__).parent / "dependencies.json").read_text())["targets"][target]
    resources = contents / "Resources"
    if json.loads((resources / "bridge/dependency.json").read_text()) != {"target": target, **manifest}:
        raise ValueError("The bundled bridge dependency manifest differs from the pinned source")
    notice = next(file for file in manifest["files"] if file["path"] == "LICENSE.socket_vmnet")
    if hashlib.sha256((resources / "bridge/LICENSE.socket_vmnet").read_bytes()).hexdigest() != notice["sha256"]:
        raise ValueError("The bundled socket_vmnet license differs from the pinned source")
    qemu = resources / "qemu"
    paths = [contents / "MacOS" / info["CFBundleExecutable"], contents / "MacOS/eink-qemu", helper]
    paths += [qemu / f"bin/qemu-system-{arch}" for arch in ("riscv32", "xtensa")]
    paths += sorted(qemu.rglob("*.dylib"))
    minimum = version(info["LSMinimumSystemVersion"])
    for path in paths:
        if path.is_symlink() or not path.is_file() or path.stat().st_mode & 0o6000:
            raise ValueError(f"Expected ordinary packaged executable: {path}")
        arch = subprocess.check_output(["lipo", "-archs", str(path)], text=True).strip()
        if arch != ARCHITECTURES[target]:
            raise ValueError(f"Unexpected packaged architecture: {path}: {arch}")
        commands = subprocess.check_output(["otool", "-l", str(path)], text=True)
        floors = re.findall(r"\bminos\s+(\d+(?:\.\d+){0,2})", commands)
        if not floors:  # Older LC_VERSION_MIN_MACOSX load command.
            floors = re.findall(r"LC_VERSION_MIN_MACOSX\s+cmdsize\s+\d+\s+version\s+(\d+(?:\.\d+){0,2})", commands)
        if not floors or any(version(floor) > minimum for floor in floors):
            raise ValueError(f"{path.name} requires macOS {floors}, newer than the app's declared {info['LSMinimumSystemVersion']}")
    for arch in ("riscv32", "xtensa"):
        path = qemu / f"bin/qemu-system-{arch}"
        data = subprocess.run(["codesign", "-d", "--entitlements", ":-", str(path)], capture_output=True, check=True).stdout
        if plistlib.loads(data).get("com.apple.security.cs.allow-jit") is not True:
            raise ValueError(f"Signed QEMU requires its JIT entitlement: {path}")
    print(f"Verified signed bridge bundle, {target}, macOS {info['LSMinimumSystemVersion']}+")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("bundle", type=Path)
    parser.add_argument("target", choices=ARCHITECTURES)
    parser.add_argument("team")
    args = parser.parse_args()
    if sys.platform != "darwin":
        parser.error("macOS package verification requires macOS signing tools")
    verify(args.bundle, args.target, args.team)
