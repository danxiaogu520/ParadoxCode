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

- Error-tolerant parsers for Paradox script and EU4 localisation; CSV remains a syntax-only/opaque resource without a CSV parser.
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
profile.

## Building from source

ParadoxCode currently requires Rust 1.97 or newer and Node.js 24 LTS for the
Tree-sitter corpus checks.

```bash
git clone https://github.com/danxiaogu520/ParadoxCode.git
cd ParadoxCode
cargo build --locked --workspace
cargo test --locked --workspace --all-targets
```

Install the repository Git hooks once:

```bash
bash scripts/install-git-hooks.sh
```

After installation, normal `git commit` commands run the complete local quality gates. Use
`bash scripts/check-quality-gates.sh` to run them explicitly or pass `core`, `grammars`, `scripts`,
`zed`, or `release` to diagnose one group.

Validate and compile the developer-maintained first-party rule source with `pdx-bake`; the output
can be placed in the ignored build directory for inspection:

```bash
cargo run -p pdx-rules --bin pdx-bake -- build --source rules/eu4 --output target/rules/eu4.pdxrules --manifest target/rules/manifest.json
```

Official `pdx-ls` binaries embed the first-party JSON source and generate a validated SQLite rules
artifact in the user cache on first use or when the source `rule_hash` changes. The generated
artifact is not committed to the repository.

## Development setup

The current alpha can launch `pdx-ls` from a configured path or from `PATH`. Workspace source roots
are configured through `.pdx/project.toml` or, in Zed, through `lsp.pdx-ls.initialization_options`
in `.zed/settings.json`.
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

The CLI and language server use the embedded first-party EU4 JSON source. `pdx-ls` materializes a
validated SQLite runtime artifact in the user cache and rejects external rule inputs.

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
| `crates/` | Rust parser, rules, HIR, workspace, analysis, formatter, LSP, and CLI crates |
| `editors/zed/` | Thin Zed extension, language metadata, and queries |
| `grammars/` | Editor-only Tree-sitter grammars and corpus tests |
| `rules/` | Authoritative first-party EU4 source and generated artifact/manifest |
| `fuzz/` | Parser, edit, formatter, and HIR fuzz targets |
| `scripts/` | Reproducible quality checks and diagnostic workflows |

## Security

Please do not report security vulnerabilities through public issues.
Contact the maintainer directly.

## License

ParadoxCode source code is available under the [MIT License](LICENSE). The repository does not
redistribute EU4 game files, user Vanilla caches, or external rule corpora. Rule maintenance and
redistribution boundaries are enforced by `pdx-bake` validation and the repository quality gates.
