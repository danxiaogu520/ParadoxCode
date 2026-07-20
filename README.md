# ParadoxCode

ParadoxCode is a general PDX language-tooling engine developed EU4-first. EU4 is the only game
currently scheduled for complete support; other game profiles are intentionally low priority.
The repository is currently an alpha with a working CWTools-aligned EU4 feature prototype: a pure-Rust syntax/parser core, Zed-only Tree-sitter grammar assets, a
conservative formatter, a Zed development extension, a stdio JSON-RPC/LSP server, an EU4-only CWT
importer, a validated SQLite rules artifact, a source-root/overlay workspace index, and
editor-neutral language analysis with diagnostics, completion, hover, navigation, and safe
semantic rename.

The alpha is not yet a complete end-user release. Per-file HIR/index updates, cheap immutable
snapshots, cancellable background work, formatter LSP wiring, and ordered read-only dependency
configuration are implemented. Persistent Vanilla caching, watched-file updates, automatic server
installation, and cross-platform release packaging remain active work. See
[workspace configuration](docs/configuration.md) for current Mod/dependency setup and
[RFC 0013](docs/rfc/0013-generic-engine-eu4-first.md) for the accepted engine/profile boundary.

The runtime parser does not compile or link Tree-sitter C. The `grammars/tree-sitter-*` directories
remain solely for Zed's editor-side highlighting and corpus checks. The committed rule artifact is
schema 12, with canonical rule hash
`446f21f2c08d8d802c8769df34259f880bb63467726592d3f95ee1cea7b71484`.

## Build and verify Phase 6A

```text
cargo check --workspace --all-targets
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo doc --workspace --no-deps
bash scripts/check-phase1-grammars.sh
python3 scripts/prepare-zed-dev-manifest.py
python3 scripts/check-zed-extension.py
cargo check --manifest-path fuzz/Cargo.toml --bins
cargo clippy --manifest-path fuzz/Cargo.toml --bins -- -D warnings
cargo run -p pdx-cwt -- import --source reference/cwtools-eu4-config --output /tmp/eu4.pdxrules --manifest /tmp/eu4-manifest.json --report /tmp/eu4-report.json
bash scripts/check-phase6a.sh
```

The core workspace contains only `pdx-*` packages. Its binaries are `pdx`, `pdx-ls`,
and `pdx-cwt`. The Zed extension is kept outside the Rust core workspace because it
is an editor-facing package rather than an analysis dependency.

See [the Phase 0 spike notes](docs/spikes/phase0-zed-grammar.md), [the Phase 1 grammar
README](grammars/README.md), [the LSP runtime RFC](docs/rfc/0009-lsp-runtime.md), and
[the implementation plan](plan.md) for the current boundary decisions.
