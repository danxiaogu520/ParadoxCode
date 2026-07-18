#!/usr/bin/env bash
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)

cargo build --manifest-path "$root/Cargo.toml" -p pdx-cli --bin pdx-ls
cargo check --manifest-path "$root/editors/zed/Cargo.toml" --all-targets
python3 "$root/scripts/check-phase6a.py"

echo "Phase 6A release and Zed smoke checks passed."
