#!/usr/bin/env bash
set -euo pipefail

test -f Cargo.toml
test -f Cargo.lock
test -f CHANGELOG.md
test -f CONTRIBUTING.md
test -f SECURITY.md
test -f CODE_OF_CONDUCT.md
test -f editors/zed/extension.toml
test -f docs/spikes/phase0-zed-grammar.md
test -f docs/spikes/phase0-server-distribution.md
test -d grammars/tree-sitter-pdx-script
test -d grammars/tree-sitter-pdx-eu4-localisation
test -d tests/fixtures/phase0
test -d fuzz

for manifest in crates/*/Cargo.toml; do
    package_name="$(sed -n 's/^name = "\([^"]*\)"/\1/p' "$manifest" | head -n 1)"
    case "$package_name" in
        pdx-*) ;;
        *) echo "non-pdx package: $package_name ($manifest)" >&2; exit 1 ;;
    esac
done

cargo metadata --no-deps --format-version 1 >/dev/null
echo "Phase 0 layout checks passed."
