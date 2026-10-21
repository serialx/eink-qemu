#!/usr/bin/env python3
"""Stage only pinned bridge payloads and notices; never execute archive contents."""
from __future__ import annotations

import argparse
import hashlib
import io
import json
from pathlib import Path, PurePosixPath
import shutil
import stat
import tarfile
import tempfile
import zipfile

ROOT = Path(__file__).resolve().parent
MANIFEST = ROOT / "dependencies.json"


def checked(data: bytes, expected: str, label: str) -> bytes:
    if hashlib.sha256(data).hexdigest() != expected:
        raise ValueError(f"Checksum mismatch: {label}")
    return data


def relative(name: str) -> str:
    path = PurePosixPath(name)
    if not name or path.is_absolute() or ".." in path.parts or "\\" in name or ":" in name:
        raise ValueError(f"Unsafe payload path: {name}")
    return str(path)


def stage(target: str, archive: Path, output: Path) -> None:
    manifest = json.loads(MANIFEST.read_text())
    if manifest["version"] != 1 or target not in manifest["targets"]:
        raise ValueError("Unsupported bridge dependency target")
    entry = manifest["targets"][target]
    if archive.is_symlink() or not archive.is_file() or archive.stat().st_size > 32 * 1024 * 1024:
        raise ValueError("Expected a regular bridge release archive")
    data = checked(archive.read_bytes(), entry["archive"]["sha256"], archive.name)
    payloads = {}
    if entry["archive"]["format"] == "zip":
        with zipfile.ZipFile(io.BytesIO(data)) as source:
            members = source.infolist()
            if len({member.filename for member in members}) != len(members):
                raise ValueError("Duplicate archive members")
            for file in entry["files"]:
                member = source.getinfo(file["member"])
                mode = member.external_attr >> 16
                if member.is_dir() or stat.S_ISLNK(mode) or member.file_size != file["size"]:
                    raise ValueError("Unexpected archive payload type or size")
                payloads[relative(file["path"])] = (checked(source.read(member), file["sha256"], member.filename), int(file["mode"], 8))
    elif entry["archive"]["format"] == "tar.gz":
        with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as source:
            members = source.getmembers()
            if len({member.name for member in members}) != len(members):
                raise ValueError("Duplicate archive members")
            for file in entry["files"]:
                member = source.getmember(file["member"])
                if not member.isfile() or member.size != file["size"]:
                    raise ValueError("Unexpected archive payload type or size")
                payloads[relative(file["path"])] = (checked(source.extractfile(member).read(), file["sha256"], member.name), int(file["mode"], 8))
    else:
        raise ValueError("Unsupported archive format")
    for notice in entry.get("notices", []):
        payloads[relative(notice["path"])] = (checked((ROOT / relative(notice["source"])).read_bytes(), notice["sha256"], notice["source"]), 0o644)
    if any(mode not in (0o644, 0o755) for _, mode in payloads.values()):
        raise ValueError("Dependency staging cannot grant special permissions")
    if output.is_symlink() or (output.exists() and not output.is_dir()):
        raise ValueError("Expected a real staging directory")
    output.parent.mkdir(parents=True, exist_ok=True)
    staging = Path(tempfile.mkdtemp(prefix=f".{output.name}-stage-", dir=output.parent))
    backup = None
    published = False
    try:
        for name, (contents, mode) in payloads.items():
            path = staging / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(contents)
            path.chmod(mode)
        (staging / "dependency.json").write_text(json.dumps({"target": target, **entry}, indent=2) + "\n")
        if output.exists():
            backup = Path(tempfile.mkdtemp(prefix=f".{output.name}-previous-", dir=output.parent))
            backup.rmdir()
            output.rename(backup)
        try:
            staging.rename(output)
            published = True
        except BaseException:
            if backup is not None:
                backup.rename(output)
                backup = None
            raise
    finally:
        if staging.exists():
            shutil.rmtree(staging)
        if published and backup is not None:
            shutil.rmtree(backup)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("target")
    parser.add_argument("archive", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    stage(args.target, args.archive, args.output)
