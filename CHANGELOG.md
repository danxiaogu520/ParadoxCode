# Changelog

All notable user-visible changes to ParadoxCode will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and releases will
follow [Semantic Versioning](https://semver.org/) once the public API and distribution contract are
established.

## [Unreleased]

### Added

- EU4-first PDX Script, localisation, and CSV syntax frontends.
- Rule-driven diagnostics, completion, hover, navigation, symbols, references, and safe rename.
- Conservative document formatting over LSP.
- Current Mod, ordered dependency, overlay, and persistent Vanilla index support.
- Thin Zed extension with editor-only Tree-sitter grammars.
- Strict developer-maintained EU4 rule source and reproducible `pdx-rulec` compiler.
- Exact-version Zed server installation and a deterministic five-target native release workflow
  with SHA-256 sidecars and complete-matrix verification.

### Changed

- Split game-neutral rules infrastructure from the EU4 profile.
- Reworked workspace snapshots and per-file state to avoid query-time workspace rebuilding.
- Embedded first-party rules into official binaries and removed external rule arguments and the
  retired CWT importer.
- Reused conservative HIR scope transitions in nested diagnostics and completion, including
  statically disambiguated rule contexts without choosing arbitrary multi-scope candidates.

### Security

- Added cooperative cancellation, stale-result gates, scan/message limits, strict LSP framing,
  symlink handling, and read-only source protections.
- Added bounded streaming downloads, restricted single-file archive extraction, tar/ZIP integrity
  checks, container-overhead-aware executable limits, and self-validating executable caches to the
  Zed server installer.
