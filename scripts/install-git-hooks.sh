#!/usr/bin/env bash
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)

if ! git -C "$root" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
    echo "ParadoxCode Git hooks require a Git checkout." >&2
    exit 1
fi

if [[ ! -x "$root/.githooks/pre-commit" ]]; then
    echo "The versioned pre-commit hook is missing or not executable." >&2
    exit 1
fi

git -C "$root" config --local core.hooksPath .githooks

configured=$(git -C "$root" config --local --get core.hooksPath)
if [[ "$configured" != ".githooks" ]]; then
    echo "Failed to configure the repository Git hook path." >&2
    exit 1
fi

echo "ParadoxCode Git hooks installed. Normal git commit commands now run the local quality gates."
