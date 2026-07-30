# ParadoxCode

[![CI](https://github.com/danxiaogu520/ParadoxCode/actions/workflows/ci.yml/badge.svg)](https://github.com/danxiaogu520/ParadoxCode/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Status: alpha](https://img.shields.io/badge/status-alpha-orange.svg)](#project-status)

ParadoxCode is an independent, open-source language toolkit for Paradox modding. It is built as a
game-neutral PDX language engine with an EU4-first product scope. The first release targets Europa
Universalis IV and the Zed editor.

> [!IMPORTANT]
> ParadoxCode is in alpha and has not published an end-user release. The core language features are
> implemented and tested, including checksummed server installation and a native release workflow,
> but the first real release run, workspace-dependent scope transitions, and final clean-machine Zed
> installation testing are still in progress.

ParadoxCode is not affiliated with or endorsed by Paradox Interactive. Europa Universalis IV and
Paradox Interactive are trademarks of their respective owners.

## What it provides

- Error-tolerant parsers for Paradox script, EU4 localisation, and supported EU4 CSV files.
- Syntax and semantic diagnostics driven by a validated EU4 rules database.
- Completion, hover, go-to-definition, references, and document/workspace symbols.
- Conflict-aware rename restricted to writable Mod sources.
- A conservative formatter that refuses unsafe rewrites.
- Workspace resolution across unsaved buffers, the current Mod, ordered dependency Mods, and a
  persistent local Vanilla index.
- A stdio Language Server (`pdx-ls`) with cancellation, stale-result protection, and immutable
  analysis snapshots, including targeted watched-file updates for live Mod roots.
- A thin Zed extension with Tree-sitter grammars used only for editor-side highlighting.
- Exact-version Zed server download with SHA-256 verification, restricted extraction, bounded
  streaming, and self-validating executable caches.

The runtime parser is implemented in Rust and does not link Tree-sitter C. Tree-sitter assets under
`grammars/` are isolated to Zed highlighting and grammar corpus tests.

## Project status

The repository currently contains an EU4 alpha rather than a production release. The main language
features and real JSON-RPC integration tests exist.

Current release blockers include:

- exercising and reviewing the first tag-driven five-target native release matrix;
- a clean-machine smoke test against an actually published Zed extension and server release;
- completing workspace-dependent and conflicting-alternative scope transitions.

No release date is promised until those checks pass.

## Architecture

```text
source text
    -> loss-aware syntax
    -> profile- and rule-aware HIR
    -> per-file index shards
    -> immutable workspace snapshot
    -> editor-neutral analysis
    -> LSP adapter
    -> Zed
```

The engine/profile boundary keeps workspace, indexing, analysis, LSP, and release infrastructure
game-neutral while EU4 paths, scopes, commands, symbols, and special semantics remain in the EU4
profile. See [the architecture guide](docs/architecture.md) and
[RFC 0012](docs/rfc/0012-generic-engine-eu4-first.md) for the accepted design.

## Building from source

ParadoxCode currently requires Rust 1.97 or newer and Node.js 24 LTS for the
Tree-sitter corpus checks.

```bash
git clone https://github.com/danxiaogu520/ParadoxCode.git
cd ParadoxCode
cargo build --workspace
cargo test --workspace --all-targets
```

Install the repository Git hooks once:

```bash
bash scripts/install-git-hooks.sh
```

After installation, normal `git commit` commands run the complete local quality gates. Use
`bash scripts/check-quality-gates.sh` to run them explicitly or pass `core`, `grammars`, `zed`, or
`release` to diagnose one group.

Compile the developer-maintained first-party rule source with `pdx-bake`; no external checkout or
rule corpus is required:

```bash
cargo run -p pdx-rules --bin pdx-bake -- build --source rules/eu4 --output rules/eu4.pdxrules --manifest rules/manifest.json
```

## Development setup

The current alpha can launch `pdx-ls` from a configured path or from `PATH`. Workspace source roots
are configured through `.pdx/project.toml`.
The documented setup is for contributors and is not the final installation experience.

Let ParadoxCode discover, validate, index, and remember the local EU4 installation:

```bash
pdx setup vanilla
```

The first `pdx-ls` launch also performs one non-blocking quick attempt when no project override or
previous attempt exists. If common locations do not produce exactly one candidate, run
`pdx setup vanilla --deep` or select a directory with `--source`. Searches are not repeated on
normal startup.

Build or manually refresh a cache at an explicit location with the lower-level command:

```bash
pdx index vanilla \
  --source /path/to/eu4 \
  --output /path/to/vanilla.pdxindex
```

The CLI and language server use the embedded first-party EU4 rules and reject external rule inputs.

## Repository layout

| Path | Purpose |
| --- | --- |
| `crates/` | Rust parser, rules, HIR, workspace, analysis, formatter, LSP, and CLI crates |
| `editors/zed/` | Thin Zed extension, language metadata, and queries |
| `grammars/` | Editor-only Tree-sitter grammars and corpus tests |
| `rules/` | Authoritative first-party EU4 source and generated artifact/manifest |
| `docs/` | Architecture, accepted RFCs, configuration, and release criteria |
| `fuzz/` | Parser, edit, and formatter fuzz targets |
| `scripts/` | Reproducible project quality checks |

## Security

Please do not report security vulnerabilities through public issues.
Contact the maintainer directly.

## License

ParadoxCode source code is available under the [MIT License](LICENSE). The repository does not
redistribute EU4 game files, user Vanilla caches, or external rule corpora. Rule maintenance and
redistribution boundaries are documented in [rules/README.md](rules/README.md).
