#!/usr/bin/env python3
"""Validate the Phase 1 Zed manifest, language metadata, and query loading."""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import tempfile
import tomllib
from pathlib import Path
from urllib.parse import urlparse


ROOT = Path(__file__).resolve().parents[1]
EXTENSION = ROOT / "editors" / "zed"
GRAMMAR_ROOT = ROOT / "grammars"
LANGUAGES = {
    "eu4": (
        "eu4",
        "tree-sitter-eu4",
        "test/corpus/eu4.txt",
        ("highlights.scm", "brackets.scm", "indents.scm", "outline.scm"),
    ),
    "pdx-eu4-localisation": (
        "pdx_eu4_localisation",
        "tree-sitter-pdx-eu4-localisation",
        "test/corpus/localisation.txt",
        ("highlights.scm", "outline.scm"),
    ),
    "pdx-eu4-csv": (
        "pdx_eu4_csv",
        "tree-sitter-pdx-eu4-csv",
        "test/corpus/csv.txt",
        ("highlights.scm", "brackets.scm", "indents.scm", "outline.scm"),
    ),
}


def require(condition: bool, message: str) -> None:
    if not condition:
        raise RuntimeError(message)


def tree_sitter_cli() -> str:
    candidates = [
        GRAMMAR_ROOT / "tree-sitter-eu4" / "node_modules" / ".bin" / "tree-sitter",
        GRAMMAR_ROOT / "tree-sitter-pdx-eu4-localisation" / "node_modules" / ".bin" / "tree-sitter",
        GRAMMAR_ROOT / "tree-sitter-pdx-eu4-csv" / "node_modules" / ".bin" / "tree-sitter",
    ]
    for candidate in candidates:
        if candidate.is_file():
            return str(candidate)
    found = shutil.which("tree-sitter")
    if found:
        return found
    raise RuntimeError("tree-sitter CLI is not installed")


def main() -> int:
    try:
        with (EXTENSION / "extension.toml").open("rb") as handle:
            manifest = tomllib.load(handle)
        require(manifest["schema_version"] == 1, "unsupported Zed extension schema")
        grammars = manifest.get("grammars", {})
        require(
            set(grammars) == {"eu4", "pdx_eu4_localisation", "pdx_eu4_csv"},
            "manifest grammar set is incomplete",
        )

        for language_dir_name, (grammar_id, grammar_dir_name, sample_relative, query_names) in LANGUAGES.items():
            language_dir = EXTENSION / "languages" / language_dir_name
            with (language_dir / "config.toml").open("rb") as handle:
                config = tomllib.load(handle)
            require(config.get("grammar") == grammar_id, f"{language_dir_name}: grammar metadata mismatch")
            repository = grammars[grammar_id]["repository"]
            parsed_url = urlparse(repository)
            require(
                parsed_url.scheme in {"file", "https"},
                f"{grammar_id}: manifest must use a file:// or https:// repository",
            )
            require(grammars[grammar_id].get("rev"), f"{grammar_id}: grammar revision is missing")
            require(
                grammars[grammar_id].get("path") == f"grammars/{grammar_dir_name}",
                f"{grammar_id}: repository path must point at the monorepo grammar directory",
            )
            grammar_path = GRAMMAR_ROOT / grammar_dir_name
            require(grammar_path.name == grammar_dir_name, f"{grammar_id}: repository path mismatch")
            require((grammar_path / "grammar.js").is_file(), f"{grammar_id}: grammar.js missing")
            require((grammar_path / "src" / "parser.c").is_file(), f"{grammar_id}: generated parser missing")
            require((grammar_path / "tree-sitter.json").is_file(), f"{grammar_id}: tree-sitter.json missing")
            for query_name in query_names:
                query_path = language_dir / query_name
                require(query_path.is_file(), f"{language_dir_name}: {query_name} missing")
                require(
                    "Phase 0 query placeholder" not in query_path.read_text(encoding="utf-8"),
                    f"{query_path}: placeholder query",
                )

            env = os.environ.copy()
            with tempfile.TemporaryDirectory(prefix="pdx-zed-query-home-") as home:
                env["HOME"] = home
                cli = tree_sitter_cli()
                sample = grammar_path / sample_relative
                for query_name in query_names:
                    result = subprocess.run(
                        [cli, "query", str(language_dir / query_name), str(sample), "--quiet"],
                        cwd=grammar_path,
                        env=env,
                        capture_output=True,
                        text=True,
                        check=False,
                    )
                    require(
                        result.returncode == 0,
                        f"{query_name}: query failed\n{result.stdout}{result.stderr}",
                    )

        settings = json.loads((EXTENSION / "recommended-settings.json").read_text(encoding="utf-8"))
        require(
            set(settings["file_types"]) == {"Europa Universalis IV", "EU4 Localisation", "EU4 CSV"},
            "recommended settings are incomplete",
        )
        print("Zed Phase 1 manifest, metadata, and queries passed.")
    except (OSError, KeyError, json.JSONDecodeError, RuntimeError) as error:
        print(f"Zed Phase 1 check failed: {error}")
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
