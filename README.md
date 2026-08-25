# ParadoxCode

[English](README.md) | [简体中文](README.zh-CN.md)

[![CI](https://github.com/danxiaogu520/ParadoxCode/actions/workflows/ci.yml/badge.svg)](https://github.com/danxiaogu520/ParadoxCode/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/danxiaogu520/ParadoxCode)](https://github.com/danxiaogu520/ParadoxCode/releases)
[![VS Code Marketplace](https://img.shields.io/visual-studio-marketplace/v/paradoxcode.paradoxcode-vscode)](https://marketplace.visualstudio.com/items?itemName=paradoxcode.paradoxcode-vscode)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

ParadoxCode is an independent, open-source language toolkit for Paradox modding. It is built as a
game-neutral PDX language engine with an EU4-first product scope: the engine layers (workspace,
indexing, analysis, LSP) stay reusable across games, while Europa Universalis IV paths, scopes,
commands, symbols, and special semantics live in the EU4 profile. The current releases target EU4
modding in VS Code and Zed.

ParadoxCode is **not affiliated with or endorsed by Paradox Interactive**. Europa Universalis IV
and Paradox Interactive are trademarks of their respective owners.

## Features

- Error-tolerant parsers for Paradox script and EU4 localisation. Syntax errors never block
  analysis: parsing produces a loss-aware syntax tree, and unrecognized constructs lower to
  `Unknown*` nodes instead of crashing.
- Syntax and semantic diagnostics driven by a validated first-party EU4 rules database.
- Diagnostics expose stable PascalCase IDs such as `UnknownKey`, `UnknownBareValue`,
  `UnknownScope`, and `TargetWrongScope`; severity is typed internally and LSP metadata carries
  certainty without exposing internal rule provenance or legacy migration fields.
- Completion, hover, go-to-definition, references, and document/workspace symbols.
- Conflict-aware rename restricted to writable Mod sources.
- A conservative formatter that refuses to rewrite unsafe or malformed files.
- Workspace resolution across unsaved buffers, the current Mod, ordered dependency Mods, and a
  persistent local Vanilla index.
- A stdio language server (`pdx-ls`) with cancellation, stale-result protection, and immutable
  analysis snapshots, including targeted watched-file updates for live Mod roots.
- A VS Code extension with zero-configuration, checksum-verified server setup, a first-run
  walkthrough, and a live mission-tree preview (texture-backed nodes, zoom, source navigation,
  PNG/JSON export).
- A thin Zed extension that adds Tree-sitter highlighting; editor highlighting is the only
  Tree-sitter usage — the runtime parser is pure Rust and does not link Tree-sitter C.
- Exact-version server downloads with SHA-256 verification, restricted extraction, bounded
  streaming, and self-validating executable caches.

## Quick start

### VS Code

Install **ParadoxCode - EU4 Language Tools** from the
[Visual Studio Marketplace](https://marketplace.visualstudio.com/items?itemName=paradoxcode.paradoxcode-vscode)
(or run `ext install paradoxcode.paradoxcode-vscode` in the Command Palette). Then:

1. Open (or create) a workspace and **trust** it.
2. Open a file from an EU4 Mod — e.g. `common/`, `events/`, `decisions/`, `missions/`,
   `history/`, `interface/`, or any file below `localisation/` (including nested files).
3. On first use the extension downloads the matching `pdx-ls` release for your platform,
   verifies its SHA-256 checksum, caches it, and starts it automatically. No language-server
   configuration is required.
4. If your EU4 installation is not discovered automatically, use **Choose EU4 Installation /
   Vanilla Data** and select the folder containing `eu4.exe` plus `common`, `events`, `missions`,
   `decisions`, and `localisation`.

The **Get Started** page includes a **Start using ParadoxCode** walkthrough covering all of this.

### Zed

The Zed extension is developed in this repository (`editors/zed`) and awaits review in the
[`zed-industries/extensions`](https://github.com/zed-industries/extensions) registry. Until it is
listed there, install it as a dev extension pointing at a checkout of this repository
(`editors/zed`). Recommended language settings are in `editors/zed/recommended-settings.json`.
They associate every file under `localisation/` (recursively) with the separate `Localisation`
language while keeping the profile's configured EU4 script directories on the `Europa Universalis IV`
language. Script directories are matched only at their configured directory level; `map` uses
fixed vanilla/reference-mod file names, and `localisation/` is the only recursive source tree.

### pdx-ls standalone

Standalone `pdx` / `pdx-ls` binaries for Linux (x86_64, aarch64), macOS (x86_64, aarch64), and
Windows (x86_64) are attached to each [GitHub Release](https://github.com/danxiaogu520/ParadoxCode/releases)
as `.tar.gz`/`.zip` archives with `.sha256` sidecars. The language server embeds the first-party
EU4 rule source and never imports external rule files.

`pdx-ls` requires modern LSP initialization with at least one `workspaceFolders` entry. Clients
that send only the deprecated `rootUri` field are intentionally unsupported and receive an
`INVALID_PARAMS` response; use a current LSP client or upgrade the editor integration.

## Project status

**Latest release: v0.1.4** (26 Aug 2026). The core EU4 language features are implemented, tested,
and released through the tag-driven release pipeline (see [Releases](#releases)). Early adopters
should expect rough edges while 0.1.x matures; please report problems through the issue templates
so they can be fixed in the next release.

Known limitations of the current scope:

- CSV files are handled as syntax-only/opaque resources; there is no CSV parser yet.
- The Zed extension is not yet listed in the Zed Extension Gallery (registry review pending).
- EU4 is the only implemented game profile. The engine is game-neutral by design, but no second
  profile exists yet, so no commitment is made about other games' timelines.

## Architecture

```text
source text
    -> loss-aware syntax
    -> profile- and rule-aware HIR
    -> per-file index shards
    -> immutable workspace snapshot
    -> editor-neutral analysis
    -> LSP adapter
    -> Zed / VS Code
```

The engine/profile boundary keeps workspace, indexing, analysis, LSP, and release infrastructure
game-neutral while EU4 paths, scopes, commands, symbols, and special semantics remain in the EU4
profile. The crate dependency direction is strict:

```text
pdx-text
  -> pdx-parser -> pdx-engine -> pdx-analysis -> pdx-lsp
pdx-game (EU4 profile) -> pdx-parser + pdx-text + pdx-rules
pdx-rules -> pdx-bake
pdx-rules + pdx-game -> pdx-engine / pdx-analysis
```

## Building from source

Prerequisites: **Rust 1.98 or newer** and **Node.js 24 LTS** (for Tree-sitter corpus checks).

```bash
git clone https://github.com/danxiaogu520/ParadoxCode.git
cd ParadoxCode
cargo build --locked --workspace
cargo test --locked --workspace --all-targets
```

Install the repository Git hooks once (they run the quality gates on every commit):

```bash
bash scripts/install-git-hooks.sh
```

Run the quality gates explicitly, or diagnose one group (`core`, `grammars`, `zed`, `vscode`,
`release`, `fuzz`, `core-fast`, `perf`):

```bash
bash scripts/check-quality-gates.sh
```

The pull-request CI uses the `core-fast` group, which keeps correctness checks but excludes
benchmark targets. The optimized benchmark suite is retained under the `perf` group and runs in
the scheduled/manual Performance workflow. CI also selects editor, grammar, fuzz, and dependency
jobs from changed paths; fuzz is limited to its direct runtime dependencies, while the Windows
release build runs in parallel with the Windows test/lint job. The `Required CI checks` job is the
stable aggregate for branch protection.

Validate and compile the developer-maintained first-party rule source with `pdx-bake`; the output
can be placed in the ignored build directory for inspection:

```bash
cargo run -p pdx-rules --bin pdx-bake -- build \
  --source rules/eu4 \
  --output target/rules/eu4.pdxrules \
  --manifest target/rules/manifest.json
```

Official `pdx-ls` binaries embed the first-party JSON source and generate a validated SQLite rules
artifact in the user cache on first use or when the source `rule_hash` changes. The generated
artifact is not committed to the repository.

The EU4 source is intentionally split by responsibility. `catalog/` contains file categories,
symbol descriptors, and normalized records; `semantic/` contains executable rule alternatives
grouped by context and game-directory schema; `types/`, `values/`, and `localisation/` contain
the supporting semantic tables; and `profile/` contains the data-only EU4 filesystem, scope,
symbol, dynamic-value, and semantic-inheritance profile. `rules/eu4/manifest.json` lists every
fragment explicitly. The compiler merges those fragments into one logical `RulesModel`, and the
same canonical `rule_hash` covers both semantic data and profile data.

## Development setup

Launch `pdx-ls` from a configured path or from `PATH`. Workspace source roots are configured
through `.pdx/project.toml` or, in Zed, through `lsp.pdx-ls.initialization_options` in
`.zed/settings.json`. The documented setup is for contributors, not the final installation
experience.

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

Large dependency Mods can be indexed once and loaded from the persistent cache on every launch
instead of being rescanned:

```bash
pdx index dependency \
  --id gui-xu \
  --source /path/to/dependency-mod \
  --output /path/to/dependency.pdxindex
```

The `id` must match the dependency id configured in the editor. In Zed, the cache is declared in
`.zed/settings.json`; `pdx-ls` loads it in the background and rebuilds it automatically when the
file is missing (a rules-hash change regenerates it like the Vanilla cache):

```json
{
  "lsp": {
    "pdx-ls": {
      "initialization_options": {
        "dependencies": [
          {
            "id": "gui-xu",
            "path": "/path/to/dependency-mod",
            "index": "/path/to/dependency.pdxindex"
          }
        ]
      }
    }
  }
}
```

While `index` is set, the dependency is not scanned live; after changing the dependency, rebuild
its cache with `pdx index dependency` and restart the language server (command palette
`pdx-ls: restart`). Remove the `index` field to fall back to live scanning.

Run a repeatable whole-Current-Mod diagnostic pass against that Vanilla cache with the development
script below. It opens each relevant file through the real `pdx-ls` transport and writes ignored
JSON and Markdown reports under `diagnostic-reports/`:

```bash
bash scripts/diagnose-current-mod.sh \
  --mod /path/to/current-mod \
  --vanilla-cache /path/to/vanilla.pdxindex
```

The command exits non-zero when errors are found; use `--fail-on warning` or `--fail-on none` to
change the automation threshold. Use `--help` for all options.

## Repository layout

| Path | Purpose |
| --- | --- |
| `crates/pdx-text` | Text, range, position, and path primitives |
| `crates/pdx-parser` | Loss-aware parser and canonical formatter |
| `crates/pdx-rules` | Game-neutral rules schema, runtime, and first-party compiler (`pdx-bake`) |
| `crates/pdx-game` | EU4 profile: game discovery, local config, and EU4 mission model |
| `crates/pdx-engine` | VFS, source roots, index shards, and immutable snapshots |
| `crates/pdx-analysis` | Editor-neutral analysis queries (diagnostics, completion, navigation, rename) |
| `crates/pdx-lsp` | LSP lifecycle, protocol boundary, and CLI entry points (`pdx`, `pdx-ls`) |
| `editors/vscode/` | VS Code extension: server bootstrap, walkthrough, mission-tree preview |
| `editors/zed/` | Thin Zed extension, language metadata, and queries |
| `grammars/` | Editor-only Tree-sitter grammars and corpus tests |
| `rules/eu4/` | Authoritative first-party EU4 rule tree (catalog, semantic, supporting tables, profile) |
| `fuzz/` | Parser, edit, formatter, and HIR fuzz targets |
| `scripts/` | Reproducible quality checks and diagnostic workflows |

The current first-party EU4 rules target game version **1.37.5** (8,525 semantic rules, 121 file
categories, 2,667 symbol descriptors). The generated release manifest in `rules/manifest.json`
records the schema version, source format, canonical `rule_hash`, and artifact checksum.

## Releases

Releases are tag-driven: pushing a `v0.x.y` tag builds and verifies all five native `pdx-ls`
archives, creates the immutable GitHub Release, and packages and attaches the VSIX. Visual Studio
Marketplace publication is temporarily manual; download the attached VSIX and upload it from the
publisher management page. Version history and per-release changes are tracked in
[CHANGELOG.md](CHANGELOG.md); the full release checklist lives in [RELEASING.md](RELEASING.md).

## Contributing

Contributions are welcome. Read [CONTRIBUTING.md](CONTRIBUTING.md) first for the build/test
setup, commit conventions, and the engineering invariants enforced by the repository
(no `unsafe`, stable identities, one authoritative rule source, and more).

## Security

Please do not report security vulnerabilities through public issues. See
[SECURITY.md](SECURITY.md) for how to report them privately and how they are handled.

## License

ParadoxCode source code is available under the [MIT License](LICENSE). The repository does not
redistribute EU4 game files, user Vanilla caches, or external rule corpora. Rule maintenance and
redistribution boundaries are enforced by `pdx-bake` validation and the repository quality gates.
