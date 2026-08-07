#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
tree_sitter_home="$(mktemp -d "${TMPDIR:-/tmp}/pdx-tree-sitter-home.XXXXXX")"
tree_sitter_config="$tree_sitter_home/config"
trap 'rm -r -- "$tree_sitter_home"' EXIT
mkdir -p "$tree_sitter_config"

for grammar in \
    "$root/grammars/tree-sitter-eu4"; do
    (
        cd "$grammar"
        if [[ -x "$grammar/node_modules/.bin/tree-sitter" ]]; then
            tree_sitter="$grammar/node_modules/.bin/tree-sitter"
        elif [[ -x "$root/grammars/tree-sitter-eu4/node_modules/.bin/tree-sitter" ]]; then
            tree_sitter="$root/grammars/tree-sitter-eu4/node_modules/.bin/tree-sitter"
        else
            npm ci --no-audit --no-fund
            tree_sitter="$grammar/node_modules/.bin/tree-sitter"
        fi
        "$tree_sitter" generate
        if [[ ! -f "$tree_sitter_config/tree-sitter/config.json" ]]; then
            # On Windows the tree-sitter CLI uses %APPDATA% instead of XDG_CONFIG_HOME, so the
            # isolation check above never sees the file it would write. init-config refuses to
            # overwrite an existing config there; a pre-existing config is exactly what `test`
            # needs, so a refusal is not an error.
            XDG_CONFIG_HOME="$tree_sitter_config" "$tree_sitter" init-config >/dev/null || true
        fi
        XDG_CONFIG_HOME="$tree_sitter_config" "$tree_sitter" test
    )
done

cargo run --locked --manifest-path "$root/Cargo.toml" --bin pdx -- check grammar-fuzz --root "$root"
echo "Grammar checks passed."
