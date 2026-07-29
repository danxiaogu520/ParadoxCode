#!/usr/bin/env python3
"""Require one release version across workspace and editor package metadata."""

from __future__ import annotations

import argparse
from pathlib import Path
import tomllib

from server_release_contract import validate_release_version


ROOT = Path(__file__).resolve().parent.parent


def arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--version", required=True)
    return parser.parse_args()


def main() -> None:
    options = arguments()
    try:
        validate_release_version(options.version)
    except ValueError as error:
        raise SystemExit(str(error)) from error
    with (ROOT / "Cargo.toml").open("rb") as handle:
        workspace = tomllib.load(handle)
    with (ROOT / "editors/zed/extension.toml").open("rb") as handle:
        extension = tomllib.load(handle)
    with (ROOT / "editors/zed/Cargo.toml").open("rb") as handle:
        zed_package = tomllib.load(handle)
    versions = {
        "Cargo workspace": workspace["workspace"]["package"]["version"],
        "Zed extension manifest": extension["version"],
        "Zed Rust package": zed_package["package"]["version"],
    }
    mismatches = {
        source: version for source, version in versions.items() if version != options.version
    }
    if mismatches:
        details = ", ".join(f"{source}={version}" for source, version in mismatches.items())
        raise SystemExit(f"release version {options.version} does not match: {details}")
    print(f"Release metadata agrees on {options.version}.")


if __name__ == "__main__":
    main()
