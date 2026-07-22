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
    "README.md",
    "LICENSE",
    "CHANGELOG.md",
    "CONTRIBUTING.md",
    "CODE_OF_CONDUCT.md",
    "SECURITY.md",
    "docs/releasing.md",
    "docs/rfc/0014-embedded-first-party-rules.md",
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

        metadata = cargo_metadata()
        packages = metadata.get("packages", [])
        require(bool(packages), "Cargo workspace has no packages")
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
        require(extension.get("id") == "paradoxcode", "published Zed extension id must remain stable")
        require(extension.get("name") == "ParadoxCode - EU4 Language Tools", "unexpected Zed display name")
        description = extension.get("description", "").lower()
        require("unofficial" in description, "Zed description must identify the extension as unofficial")

        print("Project policy and public metadata checks passed.")
    except (OSError, RuntimeError, json.JSONDecodeError, KeyError) as error:
        print(f"Project policy check failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
