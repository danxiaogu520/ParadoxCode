#!/usr/bin/env bash
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)

cargo build --locked --manifest-path "$root/Cargo.toml" -p pdx-lsp --bin pdx-ls
cargo check --locked --manifest-path "$root/editors/zed/Cargo.toml" --all-targets
cargo run --locked --manifest-path "$root/Cargo.toml" --bin pdx -- check release --root "$root"

echo "Release and Zed smoke checks passed."
