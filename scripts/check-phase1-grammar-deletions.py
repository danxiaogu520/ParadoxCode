#!/usr/bin/env python3
"""Parse every corpus case after deleting each source character.

Tree-sitter quite normally exits non-zero for a syntactically incomplete input,
so the check only treats process failures and crash-like output as failures.
This is intentionally a CLI-level smoke test: it exercises the generated C
parser in the same way a grammar consumer does, without requiring Python
Tree-sitter bindings in the workspace.
"""

from __future__ import annotations

import os
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
GRAMMARS = (
    ROOT / "grammars" / "tree-sitter-pdx-script",
    ROOT / "grammars" / "tree-sitter-pdx-eu4-localisation",
    ROOT / "grammars" / "tree-sitter-pdx-eu4-csv",
)
CASE_RE = re.compile(
    r"^={18}\n[^\n]*\n={18}\n(?P<input>.*?)\n---\n",
    re.MULTILINE | re.DOTALL,
)
CRASH_MARKERS = (
    "panic",
    "aborted",
    "segmentation fault",
    "stack overflow",
)


def corpus_inputs(grammar: Path) -> list[tuple[Path, str]]:
    cases: list[tuple[Path, str]] = []
    for corpus in sorted((grammar / "test" / "corpus").glob("*.txt")):
        text = corpus.read_text(encoding="utf-8")
        matches = list(CASE_RE.finditer(text))
        if not matches:
            raise RuntimeError(f"{corpus} contains no corpus cases")
        cases.extend((corpus, match.group("input")) for match in matches)
    return cases


def parse_without_crash(grammar: Path, sources: list[str], home: str, suffix: str) -> None:
    env = os.environ.copy()
    env["HOME"] = home
    with tempfile.TemporaryDirectory(prefix="pdx-tree-sitter-deletions-") as directory:
        directory_path = Path(directory)
        paths_file = directory_path / "paths.txt"
        paths = []
        for number, source in enumerate(sources):
            path = directory_path / f"mutation-{number}{suffix}"
            path.write_text(source, encoding="utf-8")
            paths.append(str(path))
        paths_file.write_text("\n".join(paths) + "\n", encoding="utf-8")
        local_cli = grammar / "node_modules" / ".bin" / "tree-sitter"
        shared_cli = ROOT / "grammars" / "tree-sitter-pdx-script" / "node_modules" / ".bin" / "tree-sitter"
        if local_cli.is_file():
            tree_sitter = str(local_cli)
        elif shared_cli.is_file():
            tree_sitter = str(shared_cli)
        else:
            tree_sitter = shutil.which("tree-sitter")
            if tree_sitter is None:
                raise RuntimeError("tree-sitter CLI is not installed")
        result = subprocess.run(
            [
                tree_sitter,
                "parse",
                "--no-ranges",
                "--paths",
                str(paths_file),
            ],
            cwd=grammar,
            env=env,
            text=True,
            capture_output=True,
            check=False,
        )
    output = f"{result.stdout}\n{result.stderr}".lower()
    if result.returncode not in (0, 1):
        raise RuntimeError(
            f"{grammar}: parser process failed with exit {result.returncode}\n"
            f"{result.stdout}{result.stderr}"
        )
    if any(marker in output for marker in CRASH_MARKERS):
        raise RuntimeError(f"{grammar}: parser crash marker found\n{result.stdout}{result.stderr}")


def main() -> int:
    try:
        with tempfile.TemporaryDirectory(prefix="pdx-tree-sitter-home-") as home:
            total = 0
            for grammar in GRAMMARS:
                mutations = []
                for _corpus, source in corpus_inputs(grammar):
                    for offset in range(len(source)):
                        mutated = source[:offset] + source[offset + 1 :]
                        mutations.append(mutated)
                        total += 1
                if "localisation" in grammar.name:
                    suffix = ".yml"
                elif grammar.name.endswith("csv"):
                    suffix = ".csv"
                else:
                    suffix = ".txt"
                parse_without_crash(grammar, mutations, home, suffix)
                print(f"{grammar.relative_to(ROOT)}: deletion smoke passed")
            print(f"Checked {total} single-character deletions.")
    except (OSError, RuntimeError) as error:
        print(f"Phase 1 grammar deletion check failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
