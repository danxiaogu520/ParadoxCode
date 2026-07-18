#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
tree_sitter_home="${TMPDIR:-/tmp}/pdx-tree-sitter-home"
mkdir -p "$tree_sitter_home"

for grammar in \
    "$root/grammars/tree-sitter-pdx-script" \
    "$root/grammars/tree-sitter-pdx-eu4-localisation" \
    "$root/grammars/tree-sitter-pdx-eu4-csv"; do
    (
        cd "$grammar"
        if [[ -x "$grammar/node_modules/.bin/tree-sitter" ]]; then
            tree_sitter="$grammar/node_modules/.bin/tree-sitter"
        elif [[ -x "$root/grammars/tree-sitter-pdx-script/node_modules/.bin/tree-sitter" ]]; then
            tree_sitter="$root/grammars/tree-sitter-pdx-script/node_modules/.bin/tree-sitter"
        else
            npm ci --no-audit --no-fund
            tree_sitter="$grammar/node_modules/.bin/tree-sitter"
        fi
        "$tree_sitter" generate
        if [[ ! -f "$tree_sitter_home/.config/tree-sitter/config.json" ]]; then
            HOME="$tree_sitter_home" "$tree_sitter" init-config >/dev/null
        fi
        HOME="$tree_sitter_home" "$tree_sitter" test
    )
done

python3 "$root/scripts/check-phase1-grammar-deletions.py"
echo "Phase 1 grammar checks passed."
