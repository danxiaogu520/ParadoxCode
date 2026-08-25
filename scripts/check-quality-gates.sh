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
    run env RUSTDOCFLAGS="-D warnings" cargo doc --locked --workspace --no-deps
}

check_core_fast() {
    run cargo fmt --all -- --check
    run cargo check --locked --workspace --all-features
    run cargo test --locked --workspace --all-features
    run cargo clippy --locked --workspace --all-features -- -D warnings
    run env RUSTDOCFLAGS="-D warnings" cargo doc --locked --workspace --no-deps
}

check_perf() {
    run cargo bench --locked --workspace --all-features --benches
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

check_vscode() {
    # Compile TypeScript once for the smoke and source contracts. `vsce package`
    # invokes the package's prepublish hook once more as part of packaging.
    run npm --prefix editors/vscode run test:ci
}

check_release() {
    run bash scripts/check-release.sh
}

check_fuzz() {
    # Stable-only gates for the standalone fuzz crate. The nightly/cargo-fuzz
    # run loop lives in .github/workflows/ci.yml; this keeps the local
    # pre-commit gate buildable without a nightly toolchain.
    run cargo fmt --manifest-path fuzz/Cargo.toml -- --check
    run cargo check --locked --manifest-path fuzz/Cargo.toml --all-targets
}

usage() {
    echo "usage: $0 [all|core|core-fast|perf|grammars|zed|vscode|release|fuzz] [group ...]" >&2
    echo "       with no arguments, runs the full suite; multiple groups run in order" >&2
}

groups=("$@")
if [ ${#groups[@]} -eq 0 ]; then
    groups=(all)
fi
for group in "${groups[@]}"; do
    case "$group" in
        all)
            check_core
            check_grammars
            check_zed
            check_vscode
            check_release
            check_fuzz
            ;;
        core)
            check_core
            ;;
        core-fast)
            check_core_fast
            ;;
        perf)
            check_perf
            ;;
        grammars)
            check_grammars
            ;;
        zed)
            check_zed
            ;;
        vscode)
            check_vscode
            ;;
        release)
            check_release
            ;;
        fuzz)
            check_fuzz
            ;;
        *)
            usage
            exit 2
            ;;
    esac
    echo
    echo "ParadoxCode ${group} quality gates passed."
done
