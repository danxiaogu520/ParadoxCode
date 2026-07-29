#!/usr/bin/env python3
"""Create one deterministic pdx-ls release archive and SHA-256 sidecar."""

from __future__ import annotations

import argparse
import gzip
import hashlib
import io
import os
from pathlib import Path
import tarfile
import tempfile
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
    parser.add_argument("--target", choices=sorted(TARGETS), required=True)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    return parser.parse_args()


def tar_gz(binary: bytes, executable: str) -> bytes:
    output = io.BytesIO()
    with gzip.GzipFile(fileobj=output, mode="wb", filename="", mtime=0) as compressed:
        with tarfile.open(fileobj=compressed, mode="w", format=tarfile.USTAR_FORMAT) as archive:
            info = tarfile.TarInfo(executable)
            info.size = len(binary)
            info.mode = 0o755
            info.mtime = 0
            info.uid = 0
            info.gid = 0
            info.uname = ""
            info.gname = ""
            archive.addfile(info, io.BytesIO(binary))
    return output.getvalue()


def zip_archive(binary: bytes, executable: str) -> bytes:
    output = io.BytesIO()
    with zipfile.ZipFile(output, mode="w", compression=zipfile.ZIP_DEFLATED) as archive:
        info = zipfile.ZipInfo(executable, date_time=(1980, 1, 1, 0, 0, 0))
        info.compress_type = zipfile.ZIP_DEFLATED
        info.create_system = 3
        info.external_attr = 0o755 << 16
        archive.writestr(info, binary)
    return output.getvalue()


def atomic_write(path: Path, contents: bytes) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    descriptor, temporary = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    try:
        with os.fdopen(descriptor, "wb") as output:
            output.write(contents)
            output.flush()
            os.fsync(output.fileno())
        os.replace(temporary, path)
    except BaseException:
        try:
            os.unlink(temporary)
        except FileNotFoundError:
            pass
        raise


def main() -> None:
    options = arguments()
    try:
        validate_release_version(options.version)
    except ValueError as error:
        raise SystemExit(str(error)) from error
    if not options.binary.is_file():
        raise SystemExit(f"server binary does not exist: {options.binary}")
    if options.binary.stat().st_size > LIMITS.executable_bytes:
        raise SystemExit("server binary exceeds the distribution executable size limit")

    artifact = TARGETS[options.target]
    extension = artifact.archive_kind
    executable = artifact.binary
    name = artifact.archive_name(options.version)
    with options.binary.open("rb") as source:
        binary = source.read(LIMITS.executable_bytes + 1)
    if len(binary) > LIMITS.executable_bytes:
        raise SystemExit("server binary exceeds the distribution executable size limit")
    archive = (
        zip_archive(binary, executable)
        if extension == "zip"
        else tar_gz(binary, executable)
    )
    archive_path = options.output_dir / name
    if len(archive) > LIMITS.archive_bytes:
        raise SystemExit("server archive exceeds the distribution archive size limit")
    checksum = hashlib.sha256(archive).hexdigest()
    atomic_write(archive_path, archive)
    atomic_write(
        archive_path.with_name(artifact.checksum_name(options.version)),
        f"{checksum}  {name}\n".encode(),
    )
    print(archive_path)


if __name__ == "__main__":
    main()
