#!/usr/bin/env python3
"""Build and sign the fixed macOS bridge helper from a verified dependency."""
from __future__ import annotations

import argparse
import os
from pathlib import Path
import plistlib
import re
import shutil
import subprocess
import sys
import tempfile

from stage_dependency import stage

DESKTOP = Path(__file__).resolve().parents[2]
LABEL = "net.serialx.eink-qemu.bridge-helper"
ARCHITECTURES = {"aarch64-apple-darwin": "arm64", "x86_64-apple-darwin": "x86_64"}


def requirement(identifier: str, team: str) -> str:
    if not re.fullmatch(r"[A-Z0-9]{10}", team):
        raise ValueError("Expected a ten-character Apple Team ID")
    return f'anchor apple generic and identifier "{identifier}" and certificate leaf[subject.OU] = "{team}"'


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("target", choices=ARCHITECTURES)
    parser.add_argument("archive", type=Path)
    args = parser.parse_args()
    if sys.platform != "darwin":
        parser.error("macOS helper staging requires macOS and its signing tools")
    identity = os.environ.get("APPLE_SIGNING_IDENTITY", "")
    team = os.environ.get("EINK_BRIDGE_MACOS_TEAM_ID", "")
    if not identity or identity == "-":
        parser.error("Set APPLE_SIGNING_IDENTITY to an Apple-issued signing identity")
    expected = requirement(LABEL, team)
    output = DESKTOP / "src-tauri/macos/bridge"
    if output.is_symlink() or (output.exists() and not output.is_dir()):
        raise ValueError("Expected a real bridge staging directory")
    output.parent.mkdir(parents=True, exist_ok=True)
    # Keep a previously verified bundle intact until compilation, architecture,
    # signature, Team ID and dependency checks have all passed.
    with tempfile.TemporaryDirectory(prefix=".bridge-build-", dir=output.parent) as temporary:
        staged = Path(temporary) / "payload"
        stage(args.target, args.archive.resolve(), staged)
        environment = dict(os.environ, EINK_BRIDGE_MACOS_TEAM_ID=team,
                           EINK_BRIDGE_MACOS_PAYLOAD=str(staged / "socket_vmnet"))
        subprocess.run(["cargo", "build", "--locked", "--release", "--target", args.target,
                        "-p", "eink-bridge", "--bin", "eink-bridge-helper"],
                       cwd=DESKTOP, env=environment, check=True)
        target_root = Path(environment.get("CARGO_TARGET_DIR", DESKTOP / "target"))
        if not target_root.is_absolute():
            target_root = DESKTOP / target_root
        helper = staged / LABEL
        shutil.copyfile(target_root / args.target / "release/eink-bridge-helper", helper)
        helper.chmod(0o755)
        architecture = subprocess.check_output(["lipo", "-archs", str(helper)], text=True).strip()
        if architecture != ARCHITECTURES[args.target]:
            raise ValueError("Helper architecture differs from the pinned dependency")
        subprocess.run(["codesign", "--force", "--options", "runtime", "--sign", identity,
                        "--identifier", LABEL, str(helper)], check=True)
        subprocess.run(["codesign", "--verify", "--strict", "-R", "=" + expected, str(helper)], check=True)
        (staged / "Info.plist").write_bytes(plistlib.dumps({"SMPrivilegedExecutables": {LABEL: expected}}))
        # The executable is embedded in the helper. Only its notice and manifest
        # need to be separate bundle resources.
        (staged / "socket_vmnet").unlink()
        backup = Path(temporary) / "previous"
        if output.exists():
            output.rename(backup)
        try:
            staged.rename(output)
        except BaseException:
            if backup.exists():
                backup.rename(output)
            raise
    print(f"Verified signed bridge helper staged at {output}")


if __name__ == "__main__":
    main()
