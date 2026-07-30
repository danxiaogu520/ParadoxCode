#!/usr/bin/env python3
"""Pin the Zed dev manifest to this checkout's reachable Git remote."""

from pathlib import Path
import re
import subprocess


ROOT = Path(__file__).resolve().parents[1]
MANIFEST = ROOT / "editors" / "zed" / "extension.toml"
GRAMMARS = (
    ("eu4", "eu4"),
)


def git_revision() -> str:
    result = subprocess.run(
        ["git", "-C", str(ROOT), "rev-parse", "HEAD"],
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode != 0:
        raise SystemExit(
            "The Zed development manifest needs a Git checkout so its grammar revision can be pinned."
        )
    return result.stdout.strip()


def git_repository() -> str:
    result = subprocess.run(
        ["git", "-C", str(ROOT), "remote", "get-url", "origin"],
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode != 0 or not result.stdout.strip():
        raise SystemExit(
            "The Zed development manifest needs an origin remote so Zed can fetch grammar sources."
        )
    return result.stdout.strip()


def main() -> None:
    text = MANIFEST.read_text(encoding="utf-8")
    repository = git_repository()
    revision = git_revision()
    for grammar_id, grammar_dir_name in GRAMMARS:
        table = f"[grammars.{grammar_id}]"
        start = text.index(table)
        next_table = text.find("\n[", start + len(table))
        end = len(text) if next_table == -1 else next_table
        block = text[start:end]
        block = re.sub(
            r'^repository = ".*"$',
            f'repository = "{repository}"',
            block,
            count=1,
            flags=re.MULTILINE,
        )
        block = re.sub(
            r'^rev = ".*"$',
            f'rev = "{revision}"',
            block,
            count=1,
            flags=re.MULTILINE,
        )
        block = re.sub(
            r'^path = ".*"$',
            f'path = "grammars/tree-sitter-{grammar_dir_name}"',
            block,
            count=1,
            flags=re.MULTILINE,
        )
        text = f"{text[:start]}{block}{text[end:]}"
    MANIFEST.write_text(text, encoding="utf-8")
    print(f"Updated {MANIFEST} for this checkout.")


if __name__ == "__main__":
    main()
