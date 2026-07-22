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

### Changed

- Split game-neutral rules infrastructure from the EU4 profile.
- Reworked workspace snapshots and per-file state to avoid query-time workspace rebuilding.
- Embedded first-party rules into official binaries and removed external rule arguments and the
  retired CWT importer.

### Security

- Added cooperative cancellation, stale-result gates, scan limits, symlink handling, and read-only
  source protections.
