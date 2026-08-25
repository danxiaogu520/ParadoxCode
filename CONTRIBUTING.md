# Contributing to ParadoxCode

Thanks for your interest in ParadoxCode! This guide explains how to build, test, and contribute to
the repository. It is written for humans; AI agents working in this repository should also read
[`AGENTS.md`](AGENTS.md), which defines the enforced engineering conventions.

## Project overview

ParadoxCode is a game-neutral PDX language engine with an EU4-first scope. The architecture keeps
the parser, HIR, workspace, analysis, and LSP layers game-agnostic while EU4 paths, scopes,
commands, symbols, and special semantics stay in the EU4 profile. Contributions should respect
these boundaries:

```text
pdx-text
  -> pdx-parser -> pdx-engine -> pdx-analysis -> pdx-lsp
pdx-game (EU4 profile) -> pdx-parser + pdx-text + pdx-rules
pdx-rules -> pdx-bake
pdx-rules + pdx-game -> pdx-engine / pdx-analysis
```

Each layer has a strict responsibility list; details live in [`AGENTS.md`](AGENTS.md#4-architecture-boundaries).
As a rule of thumb: EU4 name tables, scope lists, and special semantics belong in `pdx-game` (the
EU4 profile), never in the generic engine, LSP layer, or editor extensions.

## Prerequisites

- Rust **1.98 or newer** (see `.github/workflows/ci.yml` for the enforced MSRV).
- Node.js **24 LTS** for Tree-sitter corpus checks and the VS Code extension.
- Git. Install the repository hooks once after cloning:

```bash
bash scripts/install-git-hooks.sh
```

The hooks run `scripts/check-quality-gates.sh` on every `git commit`, scoped to the staged paths
(core, grammars, zed, or vscode). Set `PDX_PRECOMMIT_ALL=1` to force the full suite.

## Building and testing

```bash
cargo build --locked --workspace
cargo test --locked --workspace --all-targets
cargo clippy --locked --workspace --all-targets -- -D warnings
```

Run the complete quality gates explicitly:

```bash
bash scripts/check-quality-gates.sh
```

or a single group to diagnose a failure: `core`, `core-fast`, `perf`, `grammars`, `zed`, `vscode`,
`release`, `fuzz`.

Pull-request CI uses `core-fast` and leaves the optimized benchmark suite to the scheduled/manual
`perf` workflow. Run the latter explicitly when changing performance-sensitive code:

```bash
bash scripts/check-quality-gates.sh perf
```

CI selects the editor, grammar, fuzz, and dependency jobs from changed paths. Fuzz is limited to
its direct runtime dependencies, and the Windows release build runs in parallel with Windows
tests and clippy. Branch protection should require the stable `Required CI checks` aggregate rather
than every conditional job.

Validate and compile the first-party EU4 rule source with `pdx-bake`:

```bash
cargo run -p pdx-rules --bin pdx-bake -- build \
  --source rules/eu4 \
  --output target/rules/eu4.pdxrules \
  --manifest target/rules/manifest.json
```

A whole-Current-Mod diagnostic pass against a local Vanilla index is available through
`scripts/diagnose-current-mod.sh` (see the README for usage). Generated reports land in the
ignored `diagnostic-reports/` directory and must not be committed.

## Repository layout

| Path | Purpose |
| --- | --- |
| `crates/` | Rust parser, rules, HIR, workspace, analysis, formatter, LSP, and CLI crates |
| `editors/zed/` | Thin Zed extension and language metadata |
| `editors/vscode/` | VS Code extension with server bootstrap and mission-tree preview |
| `grammars/` | Editor-only Tree-sitter grammars and corpus tests |
| `rules/` | Authoritative first-party EU4 rule source (`rules/eu4/*.json`) |
| `fuzz/` | Parser, edit, formatter, and HIR fuzz targets |
| `scripts/` | Reproducible quality checks and diagnostic workflows |

## How to contribute

1. **Open an issue first** for non-trivial changes so the problem and approach are agreed before
   code is written. Use the issue templates for bug reports and feature requests.
2. **Make focused, reviewable changes.** Keep behavior-preserving refactors separate from new
   features, and add tests or fixtures that prove the behavior in the same change.
3. **Run the local quality gates** (they run automatically on commit) and make sure CI passes.
4. **Open a pull request** describing the change, the tests run, and any residual risks.

### Commit message convention

ParadoxCode uses [Conventional Commits](https://www.conventionalcommits.org/):

```
<type>(<optional scope>): <imperative summary>

<optional body>
```

- Types: `feat`, `fix`, `docs`, `refactor`, `perf`, `test`, `ci`, `chore`, `style`.
- Use a scope when it helps, e.g. `fix(lsp):`, `ci(release):`, `feat(mission):`.
- Keep the subject under 72 characters, capitalized, no trailing period.
- The body is optional; when present, wrap at 72 columns and explain *why*, not what.
- Mark breaking changes with `!` after the type/scope in addition to a `BREAKING CHANGE:` trailer.

Example:

```
fix(lsp): validate document version before publishing diagnostics

Snapshot workers may finish after a newer edit; discard their results so
stale diagnostics never reach the client.
```

### Branching and publishing

- The default publishing target is `main`; contributors work on their own forks/branches and open
  pull requests into `main`.
- Maintainers push release tags (`v0.1.x`) directly after CI passes. See
  [`RELEASING.md`](RELEASING.md) for the full release checklist.

## Engineering conventions

These are the invariants the repository enforces; please keep them in mind in every change:

- **No `unsafe`.** The workspace forbids `unsafe_code` in Cargo lints.
- **No panics on user input.** Paths, file contents, and configuration errors must be returned
  explicitly; do not use `unwrap`/`expect` on user-supplied data.
- **Stable identities.** Source roots, files, documents, and symbols use stable IDs. Never use
  absolute paths or CST node pointers as cross-request identities.
- **Everything is cancellable.** Background work must cooperate with cancellation or have clear
  resource and time bounds.
- **Syntax errors never block analysis.** Parsers produce loss-aware CSTs even on malformed input;
  unrecognized constructs lower to `Unknown*` nodes instead of panicking.
- **One authoritative rule source.** `rules/eu4/*.json` is the only rule authority. Generated
  SQLite artifacts are never hand-maintained, `.cwt` files are never rule input, and the runtime
  accepts no external rule paths.
- **`rule_hash` is content-based.** It hashes canonical logical content, not artifact bytes, so it
  is unaffected by rowids, page layout, timestamps, or import order.

### Testing guidance

Pick the level that matches the change:

- `pdx-text`: offsets, line endings, UTF-16, URI/path.
- `pdx-parser`: typed CST, error recovery, incremental edits, formatter safety, token preservation.
- `pdx-rules` / `pdx-bake`: schema, foreign keys, stable identity, deterministic hash, round-trip.
- `pdx-engine`: scope transitions, unknown context, source-root order, overlay, shard replacement.
- `pdx-analysis`: diagnostics, completion, definition, references, hover, rename.
- `pdx-lsp`: real JSON-RPC transport, out-of-order versions, cancellation, stale diagnostics.
- Editors: manifest/build smoke tests and file recognition tests.

Fuzz targets live in `fuzz/`. Any crash discovered in fuzzing must be added to the regression
corpus after it is fixed.

## Getting help

- Open an issue for bugs and feature requests (bug reports should use the template).
- Report security vulnerabilities privately as described in [`SECURITY.md`](SECURITY.md) — never
  in a public issue.
