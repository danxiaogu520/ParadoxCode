#!/usr/bin/env python3
"""Validate a complete pdx-ls release directory before publication."""

from __future__ import annotations

import argparse
import hashlib
from pathlib import Path
import tarfile
import zipfile

from server_release_contract import (
    load_server_artifacts,
    load_server_limits,
    validate_release_version,
)

TARGETS = load_server_artifacts()
LIMITS = load_server_limits()


def arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--version", required=True)
    parser.add_argument("--directory", type=Path, required=True)
    return parser.parse_args()


def fail(message: str) -> None:
    raise SystemExit(message)


def is_regular_file(path: Path) -> bool:
    return path.is_file() and not path.is_symlink()


def verify_archive(path: Path, extension: str, binary: str) -> None:
    if extension == "zip":
        with zipfile.ZipFile(path) as archive:
            if archive.namelist() != [binary]:
                fail(f"{path.name}: expected only {binary}")
            member = archive.getinfo(binary)
            unix_type = (member.external_attr >> 16) & 0o170000
            if member.is_dir() or unix_type == 0o120000:
                fail(f"{path.name}: executable is not a regular file")
            if member.file_size > LIMITS.executable_bytes:
                fail(f"{path.name}: executable exceeds the distribution size limit")
            if not archive.read(binary):
                fail(f"{path.name}: executable is empty")
        return
    with tarfile.open(path, "r:gz") as archive:
        members = archive.getmembers()
        if [member.name for member in members] != [binary]:
            fail(f"{path.name}: expected only {binary}")
        if not members[0].isfile():
            fail(f"{path.name}: executable is not a regular file")
        if members[0].mode != 0o755:
            fail(f"{path.name}: executable mode is not 0755")
        if members[0].size > LIMITS.executable_bytes:
            fail(f"{path.name}: executable exceeds the distribution size limit")
        extracted = archive.extractfile(members[0])
        if extracted is None or not extracted.read():
            fail(f"{path.name}: executable is empty")


def main() -> None:
    options = arguments()
    try:
        validate_release_version(options.version)
    except ValueError as error:
        fail(str(error))
    expected_files = set()
    for target, artifact in TARGETS.items():
        extension = artifact.archive_kind
        binary = artifact.binary
        name = artifact.archive_name(options.version)
        archive = options.directory / name
        sidecar = options.directory / artifact.checksum_name(options.version)
        expected_files.update((name, sidecar.name))
        if not is_regular_file(archive) or not is_regular_file(sidecar):
            fail(f"{target}: archive or checksum is missing")
        if archive.stat().st_size > LIMITS.archive_bytes:
            fail(f"{name}: archive exceeds the distribution size limit")
        if sidecar.stat().st_size > LIMITS.checksum_bytes:
            fail(f"{sidecar.name}: checksum sidecar exceeds the distribution size limit")
        line = sidecar.read_text(encoding="utf-8").strip()
        digest, separator, checksum_name = line.partition("  ")
        if separator != "  " or checksum_name != name:
            fail(f"{sidecar.name}: malformed checksum line")
        if digest != hashlib.sha256(archive.read_bytes()).hexdigest():
            fail(f"{sidecar.name}: checksum mismatch")
        verify_archive(archive, extension, binary)
    actual_entries = {path.name for path in options.directory.iterdir()}
    if actual_entries != expected_files:
        fail(f"unexpected release files: {sorted(actual_entries - expected_files)}")
    print("Complete server release matrix verified.")


if __name__ == "__main__":
    main()
