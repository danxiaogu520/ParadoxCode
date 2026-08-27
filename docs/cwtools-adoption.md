# CWTools capability adoption

This document records the staged migration plan for adopting the useful CWTools
loading, caching, indexing, and profiling techniques without changing ParadoxCode's
rule authority or workspace identity model.

## Baseline (2026-08-28)

The reference repositories are checked out beside this workspace under `C:\Code`:

- `cwtools-reference` (classic F#/.NET implementation)
- `cwtools-rs-reference` (Rust implementation)
- `cwtools-vscode-reference` (current extension/server)
- `cwtools-eu4-config-reference` (EU4 configuration repository)

The initial local verification passed:

- `cargo test -p pdx-rules --lib`: 21 tests
- `cargo test -p pdx-game --lib`: 51 tests
- `cargo test -p pdx-engine --lib`: 77 tests

Representative release measurements are approximately 74 ms for embedded source
loading, 498 ms for a cold rule artifact compile, and 179 ms for the current warm
cache path. The warm path still parses the embedded JSON before it checks the SQLite
artifact; this is the first rules-loading optimization target.

## Non-negotiable contracts

- `rules/eu4/*.json` remains the only first-party rule authority.
- SQLite artifacts are validated, read-only runtime products, not a second source.
- Source-root priority, stable IDs, per-file shards, immutable snapshots, cancellation,
  and stale-result checks remain unchanged.
- EU4-specific semantics stay in `pdx-game::eu4`; the core engine and LSP stay generic.
- External `.cwt` files, network rule fetching, and Vanilla redistribution are not runtime
  inputs.

## Migration gates

Every stage must preserve semantic golden tests and the real JSON-RPC transport tests.
The rules compiler must continue to produce the same canonical hash and exact SQLite
round-trip. A performance change is accepted only when the comparable corpus shows a
measurable improvement in cold start, warm start, or edit latency without exceeding
the existing memory and resource limits.

If an internal compatibility-preserving refactor cannot meet those gates, the affected
subsystem may be rewritten. The whole workspace, snapshot, and LSP protocol are not
rewritten without evidence that their boundaries are the bottleneck.

