#!/usr/bin/env python3
"""Exercise the deterministic server release packager without a Rust build."""

from __future__ import annotations

import hashlib
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import zipfile

from server_release_contract import (
    load_server_artifacts,
    load_server_limits,
    validate_release_version,
)

ROOT = Path(__file__).resolve().parent.parent
PACKAGER = ROOT / "scripts/package-server-release.py"
VERIFIER = ROOT / "scripts/verify-server-release.py"
PAYLOAD = b"portable pdx-ls fixture\n"
TARGETS = tuple(load_server_artifacts())
LIMITS = load_server_limits()


def package(directory: Path, target: str) -> Path:
    directory.mkdir(parents=True, exist_ok=True)
    binary = directory / ("fixture.exe" if "windows" in target else "fixture")
    binary.write_bytes(PAYLOAD)
    result = subprocess.run(
        [
            sys.executable,
            str(PACKAGER),
            "--version",
            "0.1.0-test.1",
            "--target",
            target,
            "--binary",
            str(binary),
            "--output-dir",
            str(directory),
        ],
        check=True,
        capture_output=True,
        text=True,
    )
    return Path(result.stdout.strip())


def verify_checksum(archive: Path) -> None:
    sidecar = archive.with_name(f"{archive.name}.sha256")
    digest, name = sidecar.read_text(encoding="utf-8").strip().split("  ", 1)
    assert name == archive.name
    assert digest == hashlib.sha256(archive.read_bytes()).hexdigest()


def verify_release(directory: Path, *, succeeds: bool) -> None:
    result = subprocess.run(
        [
            sys.executable,
            str(VERIFIER),
            "--version",
            "0.1.0-test.1",
            "--directory",
            str(directory),
        ],
        check=False,
        capture_output=True,
        text=True,
    )
    assert (result.returncode == 0) is succeeds, result.stdout + result.stderr


def main() -> None:
    assert validate_release_version("1.2.3-alpha.1+build.7") == "1.2.3-alpha.1+build.7"
    for invalid in ("01.2.3", "1.02.3", "1.2.03", "1.2.3-01", "1.2", "v1.2.3"):
        try:
            validate_release_version(invalid)
        except ValueError:
            pass
        else:
            raise AssertionError(f"invalid release version was accepted: {invalid}")

    with tempfile.TemporaryDirectory(prefix="pdx-release-test-") as temporary:
        root = Path(temporary)
        linux = package(root / "linux", "x86_64-unknown-linux-gnu")
        first = linux.read_bytes()
        verify_checksum(linux)
        with tarfile.open(linux, "r:gz") as archive:
            members = archive.getmembers()
            assert [member.name for member in members] == ["pdx-ls"]
            assert members[0].mode == 0o755
            assert archive.extractfile(members[0]).read() == PAYLOAD
        assert package(root / "linux", "x86_64-unknown-linux-gnu").read_bytes() == first

        windows = package(root / "windows", "x86_64-pc-windows-msvc")
        verify_checksum(windows)
        with zipfile.ZipFile(windows) as archive:
            assert archive.namelist() == ["pdx-ls.exe"]
            assert archive.read("pdx-ls.exe") == PAYLOAD

        release = root / "release"
        for target in TARGETS:
            package(release, target)
        (release / "fixture").unlink()
        (release / "fixture.exe").unlink()
        verify_release(release, succeeds=True)

        linux_sidecar = release / f"{linux.name}.sha256"
        valid_sidecar = linux_sidecar.read_bytes()
        linux_sidecar.write_text(f"{'0' * 64}  {linux.name}\n", encoding="utf-8")
        verify_release(release, succeeds=False)
        linux_sidecar.write_bytes(b"x" * 1025)
        verify_release(release, succeeds=False)
        linux_sidecar.write_bytes(valid_sidecar)

        extra = release / "unexpected.txt"
        extra.write_text("not a release asset\n", encoding="utf-8")
        verify_release(release, succeeds=False)
        extra.unlink()

        extra_directory = release / "unexpected-directory"
        extra_directory.mkdir()
        verify_release(release, succeeds=False)
        extra_directory.rmdir()

        missing = release / linux.name
        parked = release / f".{linux.name}.missing"
        missing.rename(parked)
        verify_release(release, succeeds=False)
        parked.rename(missing)
        verify_release(release, succeeds=True)

        outside_archive = root / "outside-linux-archive"
        missing.rename(outside_archive)
        missing.symlink_to(outside_archive)
        verify_release(release, succeeds=False)
        missing.unlink()
        outside_archive.rename(missing)
        verify_release(release, succeeds=True)

        valid_archive = missing.read_bytes()
        with tarfile.open(missing, "w:gz", format=tarfile.USTAR_FORMAT) as archive:
            link = tarfile.TarInfo("pdx-ls")
            link.type = tarfile.SYMTYPE
            link.linkname = "../outside"
            link.mode = 0o755
            archive.addfile(link)
        linux_sidecar.write_text(
            f"{hashlib.sha256(missing.read_bytes()).hexdigest()}  {missing.name}\n",
            encoding="utf-8",
        )
        verify_release(release, succeeds=False)
        missing.write_bytes(valid_archive)
        linux_sidecar.write_bytes(valid_sidecar)
        verify_release(release, succeeds=True)

        windows_release = release / windows.name
        windows_sidecar = release / f"{windows.name}.sha256"
        valid_windows_archive = windows_release.read_bytes()
        valid_windows_sidecar = windows_sidecar.read_bytes()
        with zipfile.ZipFile(windows_release, "w") as archive:
            link = zipfile.ZipInfo("pdx-ls.exe")
            link.create_system = 3
            link.external_attr = 0o120777 << 16
            archive.writestr(link, "../outside")
        windows_sidecar.write_text(
            f"{hashlib.sha256(windows_release.read_bytes()).hexdigest()}  {windows_release.name}\n",
            encoding="utf-8",
        )
        verify_release(release, succeeds=False)
        windows_release.write_bytes(valid_windows_archive)
        windows_sidecar.write_bytes(valid_windows_sidecar)
        verify_release(release, succeeds=True)

        with missing.open("wb") as oversized:
            oversized.truncate(LIMITS.archive_bytes + 1)
        verify_release(release, succeeds=False)
        missing.write_bytes(valid_archive)
        verify_release(release, succeeds=True)

        oversized_binary = root / "oversized-server"
        with oversized_binary.open("wb") as oversized:
            oversized.truncate(LIMITS.executable_bytes + 1)
        oversized_result = subprocess.run(
            [
                sys.executable,
                str(PACKAGER),
                "--version",
                "0.1.0-test.1",
                "--target",
                "x86_64-unknown-linux-gnu",
                "--binary",
                str(oversized_binary),
                "--output-dir",
                str(root / "oversized-output"),
            ],
            check=False,
            capture_output=True,
            text=True,
        )
        assert oversized_result.returncode != 0

    print("Deterministic server release packaging checks passed.")


if __name__ == "__main__":
    main()
