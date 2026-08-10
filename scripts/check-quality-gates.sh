#!/usr/bin/env bash
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$root"

run() {
    echo
    echo "==> $*"
    "$@"
}

check_core() {
    run cargo fmt --all -- --check
    run cargo check --locked --workspace --all-targets --all-features
    run cargo test --locked --workspace --all-targets --all-features
    run cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
    run cargo doc --locked --workspace --no-deps
}

check_grammars() {
    run bash scripts/check-grammars.sh
}

check_zed() {
    run cargo fmt --manifest-path editors/zed/Cargo.toml -- --check
    run cargo test --locked --manifest-path editors/zed/Cargo.toml
    run cargo check --locked --manifest-path editors/zed/Cargo.toml --target wasm32-wasip1
    run cargo build --locked --manifest-path editors/zed/Cargo.toml --target wasm32-wasip1 --release
    run cargo clippy --locked --manifest-path editors/zed/Cargo.toml --all-targets -- -D warnings
}

check_release() {
    run bash scripts/check-release.sh
}

usage() {
    echo "usage: $0 [all|core|grammars|scripts|zed|release]" >&2
}

group=${1:-all}
case "$group" in
    all)
        check_core
        check_grammars
        check_zed
        check_release
        ;;
    core)
        check_core
        ;;
    grammars)
        check_grammars
        ;;
    zed)
        check_zed
        ;;
    release)
        check_release
        ;;
    *)
        usage
        exit 2
        ;;
esac

echo
echo "ParadoxCode ${group} quality gates passed."
