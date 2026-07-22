# Contributing to ParadoxCode

Thank you for considering a contribution. ParadoxCode is an EU4-first language tool built on a
game-neutral PDX engine. Contributions should preserve that boundary and leave behavior verifiable.

## Before opening a change

1. Read `agent.md`, `docs/README.md`, `docs/architecture.md`, and the RFC related to your change.
2. Check existing issues and pull requests to avoid duplicate work.
3. Keep the change focused. Separate behavior-preserving refactors from new behavior.
4. Use original, minimal fixtures. Do not copy files from Europa Universalis IV or the research
   checkouts under `reference/`.

For a substantial architecture or product change, open an issue before investing in an
implementation. Bug fixes, tests, documentation improvements, and small scoped features can go
directly to a pull request.

## Development requirements

- Rust 1.88 or newer with `rustfmt` and `clippy`.
- Node.js 22 for Tree-sitter grammar tests.
- Python 3 for repository validation scripts.

The core workspace and the Zed extension use separate Cargo manifests. Do not add the Zed extension
to the core workspace dependency graph.

## Quality checks

Run the checks relevant to your change. A full local pass is:

```bash
cargo fmt --all -- --check
cargo check --workspace --all-targets --all-features
cargo test --workspace --all-targets --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo doc --workspace --no-deps
bash scripts/check-phase1-grammars.sh
python3 scripts/prepare-zed-dev-manifest.py
python3 scripts/check-zed-extension.py
cargo check --manifest-path editors/zed/Cargo.toml --all-targets
bash scripts/check-phase6a.sh
```

If a check cannot be run, say so in the pull request and explain why.

## Architecture rules

- Keep editor protocol types in `pdx-lsp`; analysis APIs remain editor-neutral.
- Keep mutable workspace ownership in `AnalysisHost`; queries read immutable snapshots.
- Keep EU4 paths, scopes, commands, and special semantics in `pdx-game-eu4` or rule data.
- Preserve source priority: unsaved overlay, current Mod, ordered dependencies, then Vanilla.
- Treat unknown or incomplete input as recoverable data, not a reason to panic.
- Do not introduce runtime CWT parsing, arbitrary configuration execution, or implicit game-path
  discovery.

The authoritative details are in `agent.md` and the accepted RFCs.

## Rules and generated artifacts

`rules/eu4.pdxrules`, `rules/manifest.json`, and `rules/import-report.json` must move together. A
rules change must include schema/invariant validation, the canonical `rule_hash`, and accurate
provenance. Do not hand-edit generated reports merely to make a check pass.

The original CWT corpus and EU4 Vanilla files must never be committed or redistributed.

## Pull requests

A useful pull request description includes:

- the problem and user-visible result;
- the architectural layer and RFC affected;
- tests and validation commands run;
- compatibility or migration impact;
- known limitations and follow-up work.

By contributing, you agree that your contribution is licensed under the repository's MIT License
and that you have the right to submit it.
