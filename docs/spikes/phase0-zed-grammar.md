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
  repository and a Git revision. When the grammar is a subdirectory of a monorepo, the entry
  must also provide its relative `path`. A development or published extension needs a
  repository reachable from the host running Zed and a pinned revision.
- A `file://` URL is only suitable when Zed and the repository share the same filesystem. It is
  not portable between a Windows Zed host and a WSL checkout.

## Decision

`grammars/tree-sitter-*` remains the monorepo source of truth. The development manifest uses the
monorepo's reachable Git remote and pins the current commit, while retaining a grammar-specific
`path`. For publishing, CI will publish a read-only split mirror for each grammar and pin the
extension manifest to a commit. The mirror is a distribution view only; grammar edits happen in
this repository.

Phase 1 now supplies reachable grammar entries, generated parsers, language metadata, and
syntax-only queries. The extension uses the current `zed_extension_api` package for its thin
Wasm entry point; it still does not contain server or EU4 semantic logic. A published manifest
must replace the monorepo URL with reachable read-only mirrors and pinned revisions.

## Phase 1 acceptance test

1. Generate the Tree-sitter parser from each monorepo grammar directory. (Automated in CI.)
2. Install `editors/zed` as a dev extension using the reachable pinned grammar URL. (Host smoke test.)
3. Open an original fixture and verify language selection plus query loading. (Host smoke test.)
4. Replace local URLs with CI mirror URLs and pinned revisions in the release manifest.
