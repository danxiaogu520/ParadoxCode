# Phase 0 spike: Zed grammar layout

Status: concluded for the Phase 0 boundary; the Phase 1 source-level implementation is present,
while live Zed install remains a host smoke test.

## Question

Can the grammar source stay under `grammars/` in this monorepo while the Zed extension owns
only a thin language client?

## Findings

- Zed language metadata belongs under `editors/zed/languages/<language>/`, which is present for
  PdxScript, EU4 localisation, and CSV.
- Zed registers a Tree-sitter grammar through an `extension.toml` grammar entry containing a
  repository and a Git revision. A local development extension may use a `file://` URL; when
  the grammar is a subdirectory of a monorepo, the entry must also provide its relative `path`.
  A published extension needs a reachable repository and pinned revision.
- The current workspace is not a Git checkout and has no `zed` executable, so this turn cannot
  run a live dev-extension installation or produce a valid pinned revision.

## Decision

`grammars/tree-sitter-*` remains the monorepo source of truth. Phase 1 will first try a local
`file://` grammar URL for dev-extension validation. For publishing, CI will publish a read-only
split mirror for each grammar and pin the extension manifest to a commit. The mirror is a
distribution view only; grammar edits happen in this repository.

Phase 1 now supplies local `file://` grammar entries, generated parsers, language metadata, and
syntax-only queries. The extension uses the current `zed_extension_api` package for its thin
Wasm entry point; it still does not contain server or EU4 semantic logic. A published manifest
must replace local URLs with reachable mirrors and pinned revisions.

## Phase 1 acceptance test

1. Generate the Tree-sitter parser from each monorepo grammar directory. (Automated in CI.)
2. Install `editors/zed` as a dev extension using local `file://` grammar URLs. (Host smoke test.)
3. Open an original fixture and verify language selection plus query loading. (Host smoke test.)
4. Replace local URLs with CI mirror URLs and pinned revisions in the release manifest.
