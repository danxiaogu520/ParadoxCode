#!/usr/bin/env bash
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)

cargo build --locked --manifest-path "$root/Cargo.toml" -p pdc --bin pdc
cargo run --locked --manifest-path "$root/Cargo.toml" -p tools -- check release --root "$root"

echo "Release smoke checks passed."
