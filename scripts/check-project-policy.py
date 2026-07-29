#!/usr/bin/env python3
"""Validate public project metadata and release-governance invariants."""

from __future__ import annotations

import json
import subprocess
import sys
import tomllib
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
REPOSITORY = "https://github.com/danxiaogu520/ParadoxCode"
REQUIRED_FILES = (
    ".githooks/pre-commit",
    "README.md",
    "LICENSE",
    "CHANGELOG.md",
    "CONTRIBUTING.md",
    "CODE_OF_CONDUCT.md",
    "SECURITY.md",
    "docs/releasing.md",
    "docs/rfc/0014-embedded-first-party-rules.md",
    "docs/rfc/0015-first-party-rule-source.md",
    "scripts/package-server-release.py",
    "scripts/server_release_contract.py",
    "scripts/test-package-server-release.py",
    "scripts/verify-server-release.py",
    "scripts/check-release-version.py",
    "scripts/check-quality-gates.sh",
    "scripts/install-git-hooks.sh",
    ".github/workflows/release.yml",
    ".github/ISSUE_TEMPLATE/bug_report.yml",
    ".github/ISSUE_TEMPLATE/feature_request.yml",
    ".github/ISSUE_TEMPLATE/config.yml",
    ".github/pull_request_template.md",
)


def require(condition: bool, message: str) -> None:
    if not condition:
        raise RuntimeError(message)


def cargo_metadata() -> dict[str, object]:
    process = subprocess.run(
        ["cargo", "metadata", "--no-deps", "--format-version", "1"],
        cwd=ROOT,
        check=False,
        capture_output=True,
        text=True,
    )
    require(process.returncode == 0, f"cargo metadata failed:\n{process.stderr}")
    return json.loads(process.stdout)


def main() -> int:
    try:
        for relative in REQUIRED_FILES:
            require((ROOT / relative).is_file(), f"required project file is missing: {relative}")

        readme = (ROOT / "README.md").read_text(encoding="utf-8")
        require("not affiliated with or endorsed by Paradox Interactive" in readme, "README disclaimer is missing")
        require("has not published an end-user release" in readme, "README alpha status is missing")

        changelog = (ROOT / "CHANGELOG.md").read_text(encoding="utf-8")
        require("## [Unreleased]" in changelog, "CHANGELOG must keep an Unreleased section")

        require(not (ROOT / "crates/pdx-cwt").exists(), "the retired CWT importer must not return")
        hook = (ROOT / ".githooks" / "pre-commit").read_text(encoding="utf-8")
        require(
            "scripts/check-quality-gates.sh" in hook,
            "the pre-commit hook must invoke the versioned quality-gate entry point",
        )
        require(
            not (ROOT / "crates/pdx-eu4").exists(),
            "the retired pdx-eu4 compatibility facade must not return",
        )
        rule_source = ROOT / "rules/eu4"
        require(rule_source.is_dir(), "first-party EU4 rule source is missing")
        require(
            not any(path.suffix.lower() == ".cwt" for path in rule_source.rglob("*")),
            "CWT files are prohibited in the authoritative rule source",
        )

        metadata = cargo_metadata()
        packages = metadata.get("packages", [])
        require(bool(packages), "Cargo workspace has no packages")
        package_versions = {package["version"] for package in packages}
        require(len(package_versions) == 1, "workspace package versions must agree")
        for package in packages:
            name = package["name"]
            require(name.startswith("pdx-"), f"workspace package does not use pdx- prefix: {name}")
            require(package.get("repository") == REPOSITORY, f"{name}: repository metadata is missing")
            require(package.get("homepage") == REPOSITORY, f"{name}: homepage metadata is missing")
            require(package.get("license") == "MIT", f"{name}: expected MIT license metadata")
            require(package.get("publish") == [], f"{name}: internal workspace crates must not be publishable")
            readme_path = package.get("readme")
            require(readme_path and readme_path.endswith("README.md"), f"{name}: README metadata is missing")

        with (ROOT / "editors/zed/extension.toml").open("rb") as handle:
            extension = tomllib.load(handle)
        with (ROOT / "editors/zed/Cargo.toml").open("rb") as handle:
            zed_package = tomllib.load(handle)
        require(extension.get("id") == "paradoxcode", "published Zed extension id must remain stable")
        require(extension.get("name") == "ParadoxCode - EU4 Language Tools", "unexpected Zed display name")
        description = extension.get("description", "").lower()
        require("unofficial" in description, "Zed description must identify the extension as unofficial")
        workspace_version = next(iter(package_versions))
        require(
            extension.get("version") == workspace_version
            and zed_package.get("package", {}).get("version") == workspace_version,
            "workspace, Zed manifest, and Zed Rust package versions must agree",
        )

        distribution = json.loads(
            (ROOT / "editors/zed/server-distribution.json").read_text(encoding="utf-8")
        )
        require(
            distribution.get("limits")
            == {
                "checksum_bytes": 1024,
                "archive_bytes": 64 * 1024 * 1024,
                "executable_bytes": 128 * 1024 * 1024,
            },
            "server distribution safety limits changed unexpectedly",
        )
        expected_targets = {
            "x86_64-unknown-linux-gnu": ("tar.gz", "pdx-ls"),
            "aarch64-unknown-linux-gnu": ("tar.gz", "pdx-ls"),
            "x86_64-apple-darwin": ("tar.gz", "pdx-ls"),
            "aarch64-apple-darwin": ("tar.gz", "pdx-ls"),
            "x86_64-pc-windows-msvc": ("zip", "pdx-ls.exe"),
        }
        require(
            set(distribution.get("artifacts", {})) == set(expected_targets),
            "server distribution target matrix is incomplete",
        )
        for target, (extension_name, binary) in expected_targets.items():
            artifact = distribution["artifacts"][target]
            expected_archive = f"pdx-ls-v{{version}}-{target}.{extension_name}"
            require(
                artifact.get("archive") == expected_archive,
                f"{target}: release archive contract mismatch",
            )
            require(
                artifact.get("checksum") == "{archive}.sha256",
                f"{target}: checksum sidecar contract mismatch",
            )
            require(artifact.get("binary") == binary, f"{target}: executable name mismatch")

        print("Project policy and public metadata checks passed.")
    except (OSError, RuntimeError, json.JSONDecodeError, KeyError) as error:
        print(f"Project policy check failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
