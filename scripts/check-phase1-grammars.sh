#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
tree_sitter_home="$(mktemp -d "${TMPDIR:-/tmp}/pdx-tree-sitter-home.XXXXXX")"
tree_sitter_config="$tree_sitter_home/config"
trap 'rm -r -- "$tree_sitter_home"' EXIT
mkdir -p "$tree_sitter_config"

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
        if [[ ! -f "$tree_sitter_config/tree-sitter/config.json" ]]; then
            XDG_CONFIG_HOME="$tree_sitter_config" "$tree_sitter" init-config >/dev/null
        fi
        XDG_CONFIG_HOME="$tree_sitter_config" "$tree_sitter" test
    )
done

python3 "$root/scripts/check-phase1-grammar-deletions.py"
echo "Phase 1 grammar checks passed."
