#!/usr/bin/env python3
"""Load the canonical pdx-ls release artifact contract."""

from __future__ import annotations

from dataclasses import dataclass
import json
from pathlib import Path, PurePosixPath, PureWindowsPath
import re


ROOT = Path(__file__).resolve().parent.parent
CONTRACT = ROOT / "editors/zed/server-distribution.json"
SEMVER_IDENTIFIER = r"(?:0|[1-9][0-9]*|[0-9A-Za-z-]*[A-Za-z-][0-9A-Za-z-]*)"
RELEASE_VERSION = re.compile(
    rf"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)"
    rf"(?:-{SEMVER_IDENTIFIER}(?:\.{SEMVER_IDENTIFIER})*)?"
    r"(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?\Z"
)


@dataclass(frozen=True)
class ServerArtifact:
    """One target's archive and executable naming contract."""

    archive_template: str
    checksum_template: str
    binary: str

    def archive_name(self, version: str) -> str:
        return self.archive_template.format(version=version)

    def checksum_name(self, version: str) -> str:
        return self.checksum_template.format(archive=self.archive_name(version))

    @property
    def archive_kind(self) -> str:
        if self.archive_template.endswith(".tar.gz"):
            return "tar.gz"
        if self.archive_template.endswith(".zip"):
            return "zip"
        raise ValueError(f"unsupported release archive template: {self.archive_template}")


@dataclass(frozen=True)
class ServerLimits:
    """Installer-compatible release size limits."""

    checksum_bytes: int
    archive_bytes: int
    executable_bytes: int


def validate_release_version(version: str) -> str:
    """Return a syntactically valid SemVer release version."""

    if not RELEASE_VERSION.fullmatch(version):
        raise ValueError(f"invalid release version: {version}")
    return version


def is_plain_filename(value: str) -> bool:
    return (
        value not in {"", ".", ".."}
        and PurePosixPath(value).name == value
        and PureWindowsPath(value).name == value
    )


def load_document() -> dict[str, object]:
    document = json.loads(CONTRACT.read_text(encoding="utf-8"))
    if not isinstance(document, dict):
        raise ValueError("server distribution contract root must be an object")
    if (
        type(document.get("schema_version")) is not int
        or document.get("schema_version") != 1
        or document.get("binary") != "pdx-ls"
    ):
        raise ValueError("unsupported server distribution contract")
    if set(document) != {"schema_version", "binary", "limits", "artifacts"}:
        raise ValueError("server distribution contract has unknown fields")
    return document


def load_server_limits() -> ServerLimits:
    """Read the release size limits shared by producer and consumer."""

    value = load_document().get("limits")
    if not isinstance(value, dict):
        raise ValueError("server distribution contract has no size limits")
    if set(value) != {"checksum_bytes", "archive_bytes", "executable_bytes"}:
        raise ValueError("server distribution size limit fields are invalid")
    limits = ServerLimits(
        checksum_bytes=value.get("checksum_bytes", 0),
        archive_bytes=value.get("archive_bytes", 0),
        executable_bytes=value.get("executable_bytes", 0),
    )
    if not all(type(limit) is int and limit > 0 for limit in vars(limits).values()):
        raise ValueError("server distribution size limits are invalid")
    return limits


def load_server_artifacts() -> dict[str, ServerArtifact]:
    """Read and strictly validate the checked-in distribution contract."""

    document = load_document()
    artifacts = document.get("artifacts")
    if not isinstance(artifacts, dict) or not artifacts:
        raise ValueError("server distribution contract has no artifacts")
    result: dict[str, ServerArtifact] = {}
    for target, value in artifacts.items():
        if not isinstance(target, str) or not target or not isinstance(value, dict):
            raise ValueError("server distribution artifact entry is malformed")
        if set(value) != {"archive", "checksum", "binary"}:
            raise ValueError(f"{target}: unknown server distribution artifact fields")
        fields = (value.get("archive"), value.get("checksum"), value.get("binary"))
        if not all(isinstance(field, str) for field in fields):
            raise ValueError(f"{target}: server distribution artifact fields must be strings")
        artifact = ServerArtifact(
            archive_template=value.get("archive", ""),
            checksum_template=value.get("checksum", ""),
            binary=value.get("binary", ""),
        )
        rendered_archive = artifact.archive_template.replace("{version}", "0.0.0")
        if (
            artifact.archive_template.count("{version}") != 1
            or "{" in rendered_archive
            or "}" in rendered_archive
            or not is_plain_filename(rendered_archive)
            or artifact.checksum_template != "{archive}.sha256"
            or not is_plain_filename(artifact.binary)
        ):
            raise ValueError(f"{target}: invalid server distribution artifact")
        artifact.archive_kind
        result[target] = artifact
    return result
