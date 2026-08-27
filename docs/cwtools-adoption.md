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

## Implemented stages

The first twelve stages are now on `main`, each independently committed and pushed:

1. Rule fragments are decoded in parallel and merged in manifest order, so duplicate/error
   reporting stays deterministic.
2. The EU4 composition root checks the generated artifact manifest before parsing embedded JSON;
   a matching SQLite cache is now the normal warm-start path.
3. Disk-file CSTs use a bounded, content-addressed bincode cache with schema/source validation,
   atomic writes, and no effect on correctness when a cache entry is missing or corrupt.
4. Semantic rule lookups use direct hash buckets, while workspace member completion reuses a
   snapshot-owned name vector and a prefix/substring index with a character-mask prefilter.
5. EU4 scope intrinsics and type-root initial scopes are represented through profile/rule data and
   case-insensitive runtime APIs; the generic semantic engine no longer hardcodes those EU4 names.
6. Localisation completion uses a snapshot-owned compact key blob with binary prefix lookup and a
   character-mask substring filter, while preserving the existing result order and cancellation
   behavior.
7. The LSP has an opt-in, idle-gated quiet workspace re-scan. It is bounded, serialized against
   foreground work, cancellation-safe, and rejects stale worker snapshots; cadence can be changed
   live through `workspace/didChangeConfiguration`.
8. The LSP advertises `pdx/reindexWorkspace` through `workspace/executeCommand` for an immediate
   full refresh. The command shares the quiet worker's cancellation and revision checks, returns a
   structured revision/file-count result, and never overlaps a foreground or quiet scan.
9. Persistent syntax-tree cache entries keep only source-independent CST/tokens/errors and reattach
   the already-read source on a validated hit, avoiding a second copy of every file in the cache
   payload while bumping the schema for safe invalidation.
10. Parsed CSTs and immutable `FileState` values share one `Arc<str>` source allocation, so the
   lossless text remains available to formatting, lowering, and editor queries without retaining
   duplicate in-memory copies.
11. Index caches persist a conservative per-file filesystem metadata fingerprint. Refreshes can
    retain unchanged shards, positions, previews, and content digests without reopening or hashing
    those files; platforms without a usable metadata stamp continue through the content-read path.
12. Workspace discovery accepts bounded file and directory ignore globs. Directory matches are
    pruned before recursion and file matches before scan-budget accounting; targeted watcher
    updates use the same predicate, so ignored paths cannot re-enter through the incremental path.

The reference comparison explains the choices. Classic CWTools loads `.cwt` files into an in-memory
rule model, while `cwtools-rs` separates parallel file parsing from ordered merge, shares an
interned string table, persists per-file ASTs, prunes user-configured ignore globs before walking,
and uses compact prefix-searchable indexes for large localisation/type sets. ParadoxCode adopted
parallel/ordered loading, source-independent persistent state, metadata fast paths, compact indexes,
and bounded ignore filtering where they fit its existing JSON-authority, stable-ID, and
immutable-snapshot contracts; global StringTable IDs remain a separately gated migration because
the current lossless CST uses source ranges as its cross-layer identity. `.cwt` remains reference
material only.

The release-shaped synthetic workspace benchmark remains within its prior envelope after the
changes (2,000 EU4 event files: roughly 26 ms initial scan and 25 ms targeted disk refresh in an
optimized build). The rules, completion, and LSP scheduling changes are covered by their focused
tests plus the full core quality gates; no benchmark result is used as a semantic correctness
substitute.
