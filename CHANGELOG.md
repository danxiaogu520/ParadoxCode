# Changelog

All notable changes to ParadoxCode are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.4] - 2026-08-26

Maintenance release focused on EU4 rule coverage, semantic completion quality, and editor
integration reliability.

### Added

- The first-party EU4 rule source is organized into catalog, semantic, supporting-table, and
  profile data with a single canonical `rule_hash` covering the complete logical model.
- Common EU4 path whitelists and event modifier keys are covered by the profile and rule data.

### Changed

- Completion candidates are ranked by contextual and macro-expansion relevance, with scripted macro
  highlighting and completion aligned across the supported editors.
- VS Code retriggers completion after assignments and block edits, while the CI matrix keeps
  Windows release builds parallel with Windows test and lint checks.

### Fixed

- Completion and semantic analysis no longer lose context after assignments or nested blocks, and
  the legacy LSP compatibility path has been removed in favor of the current initialization contract.

## [0.1.3] - 2026-08-22

Maintenance release focused on formatting canonicalization, completion quality, and hover
reliability.

### Added

- Detailed startup and index-cache progress logs in `pdx-ls` make server bootstrap state visible to
  clients.
- Decision completion and hover work again after the semantic-context traversal rework.

### Changed

- The formatter canonicalizes the fixed script keywords `AND`/`OR`/`NOT` and
  `ROOT`/`FROM`/`PREV`/`THIS` to capitals, including scope references used as values; quoted text
  is not rewritten.
- Completion now suggests the uppercase intrinsic spellings (`THIS`, `ROOT`, `FROM`, `PREV`) and no
  longer offers lowercase variants.
- Control-flow keywords are highlighted as code keywords in both editors, with aligned semantic
  token layers.
- Hover is faster and safer: known-key sets are memoized per snapshot revision and richer
  formatting is cached.

## [0.1.2] - 2026-08-20

Maintenance release focused on editor readiness, workspace loading performance, and release
reliability.

### Added

- The language server emits a `pdx/ready` notification after initial workspace and index setup,
  allowing clients to distinguish a completed startup from a finished protocol handshake.
- Mission-tree preview titles now resolve from active workspace localisation definitions, including
  Mod overrides.
- Dependency index caches can be installed in one batched operation without changing source-priority
  semantics.

### Changed

- Mission-tree preview rendering now caches layout measurements, avoids unnecessary work outside the
  viewport, and coalesces high-frequency redraws for large trees.
- VS Code shows an explicit loading/ready state, refreshes open files after the server is ready, and
  serializes language-server restarts to avoid stale clients.
- Release downloads use longer timeouts and retry transient network or HTTP failures before archive
  verification.
- Release workflow no longer publishes to the VS Code Marketplace automatically; the VSIX remains
  attached to the GitHub Release for manual publication.

### Fixed

- Completion snippet indentation in generated insert text.
- Completion inside `if`/`limit` clause blocks where the clause context was lost.
- VS Code token scopes and highlighting for EU4 localisation and script.

## [0.1.1] - 2026-08-19

Maintenance release focused on the publish pipeline and release correctness.

### Fixed

- VS Code extension now ships its runtime dependencies in the packaged VSIX.
- CLI binaries derive their version from package metadata instead of duplicated constants.
- Zed extension release manifest metadata (game version and artifact checksums).
- Release workflow reruns are idempotent: re-running the workflow on an existing tag no longer
  corrupts or duplicates release assets.

### Changed

- VS Code Marketplace publishing now uses OIDC-based trusted publishing
  (`vsce publish --oidc`); no long-lived `VSCE_PAT` secret is required.

## [0.1.0] - 2026-08-19

Initial alpha release of the game-neutral `pdx-lsp` engine with an EU4-first profile.

### Added

- Loss-aware parsers for Paradox script and EU4 localisation; CSV handled as opaque/syntax-only.
- Typed HIR lowering with `UnknownConstruct` recovery; syntax errors never block analysis.
- Frozen per-file index shards and immutable workspace snapshots with targeted watched-file updates.
- Workspace resolution across unsaved buffers, the current Mod, ordered dependency Mods, and a
  persistent local Vanilla index with automatic EU4 installation discovery.
- Validated first-party EU4 rule database (JSON source compiled by `pdx-bake`) replacing CWT
  imports; `rule_hash`-based integrity for generated artifacts.
- LSP features over real JSON-RPC: diagnostics, completion, hover, go-to-definition, references,
  document/workspace symbols, conflict-aware rename, and safe document formatting with
  cancellation and stale-result protection.
- `pdx` CLI: `setup vanilla`, `index vanilla`, and `index dependency` commands.
- VS Code extension with zero-configuration, checksum-verified server setup and a mission-tree
  preview; thin Zed extension with Tree-sitter highlighting.
- Release workflow for five native `pdx-ls` target archives with checksum sidecars, GitHub
  Releases, and Marketplace publishing.
- Fuzz targets for script/localisation parsing, incremental edits, typed CST walks, HIR lowering,
  formatting, line indexing, and first-party rule parsing.

[Unreleased]: https://github.com/danxiaogu520/ParadoxCode/compare/v0.1.4...HEAD
[0.1.4]: https://github.com/danxiaogu520/ParadoxCode/compare/v0.1.3...v0.1.4
[0.1.3]: https://github.com/danxiaogu520/ParadoxCode/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/danxiaogu520/ParadoxCode/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/danxiaogu520/ParadoxCode/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/danxiaogu520/ParadoxCode/releases/tag/v0.1.0
