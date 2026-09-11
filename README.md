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
- Diagnostics expose stable PascalCase IDs such as `UnknownKey`, `InvalidValue`,
  `WrongScope`, and `UnknownLocalisationKey`; severity is typed internally and LSP metadata
  carries certainty without exposing internal rule provenance or legacy migration fields.
- Rule-backed diagnostics can expose bounded `textDocument/codeAction` quick fixes, including
  unique did-you-mean replacements for misspelled static enum values.
- Scripted-localisation definitions are discovered from the EU4 profile's supported directory
  spellings, indexed as `defined_text`, and used for localisation-command completion and
  registry-aware unknown-command warnings. The query is path-partitioned and hash-backed, so
  large Vanilla indexes do not require a full symbol-table walk.
- Completion, hover, go-to-definition, references, and document/workspace symbols.
- Full semantic tokens support incremental `full/delta` responses with a bounded, revision-aware
  cache that is invalidated by edits and workspace refreshes.
- Viewport-limited `textDocument/semanticTokens/range` requests prune out-of-range syntax-tree
  subtrees before classification, keeping visible-editor highlighting proportional to the view.
- Rule-proven scope transitions are available as bounded `textDocument/inlayHint` annotations.
- Conflict-aware rename restricted to writable Mod sources.
- A conservative formatter that refuses to rewrite unsafe or malformed files.
- Workspace resolution across unsaved buffers, the current Mod, ordered dependency Mods, and a
  persistent local Vanilla index.
- A stdio language server (`pdc`) with cancellation, stale-result protection, and immutable
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
3. On first use the extension downloads the matching `pdc` release for your platform,
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

### pdc standalone

Standalone `pdc` binaries for Linux (x86_64, aarch64), macOS (x86_64, aarch64), and
Windows (x86_64) are attached to each [GitHub Release](https://github.com/danxiaogu520/ParadoxCode/releases)
as `.tar.gz`/`.zip` archives with `.sha256` sidecars. The language server embeds the first-party
EU4 rule source and never imports external rule files.

`pdc` requires modern LSP initialization with at least one `workspaceFolders` entry. Clients
that send only the deprecated `rootUri` field are intentionally unsupported and receive an
`INVALID_PARAMS` response; use a current LSP client or upgrade the editor integration.

## Project status

**Latest release: v0.3.2** (11 Sep 2026). The EU4 analysis and indexing features are implemented,
tested, and released through the tag-driven release pipeline (see [Releases](#releases)). This
release reworks hover into a first-class presentation surface (category titles, scope tables,
parallel multi-language localisation previews), shares one per-site dynamic-parameter derivation
across completion, hover, and diagnostics, rebuilds the EU4 variable model and modifier data from
wiki and vanilla evidence, and cuts steady-state diagnostics latency on large workspaces to under
a second. Early adopters should still expect rough edges while 0.x matures; please report problems
through the issue templates so they can be fixed in the next
release.

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
text
  -> parser -> vfs -> hir -> index -> engine -> ide -> pdc
game (EU4 profile) -> parser + text + rules
rules -> bake
rules + game -> engine / ide / index
```

## Building from source

Prerequisites: **Rust 1.98 or newer** and **Node.js 24 LTS** (for Tree-sitter corpus checks).

```bash
git clone https://github.com/danxiaogu520/ParadoxCode.git
cd ParadoxCode
cargo build --locked --workspace
cargo test --locked --workspace --all-targets
```

Run the quality gates explicitly (or diagnose one group: `core`, `grammars`, `zed`, `vscode`,
`release`, `fuzz`, `core-fast`, `perf`). There are no commit hooks; CI runs the same gates on
every pull request:

```bash
bash scripts/check-quality-gates.sh
```

The pull-request CI uses the `core-fast` group, which keeps correctness checks but excludes
benchmark targets. The optimized benchmark suite is retained under the `perf` group and runs in
the scheduled/manual Performance workflow. CI also selects editor, grammar, fuzz, and dependency
jobs from changed paths; fuzz is limited to its direct runtime dependencies, while the Windows
release build runs in parallel with the Windows test/lint job. The `Required CI checks` job is the
stable aggregate for branch protection.

Validate and compile the developer-maintained first-party rule source with `bake`; the output
can be placed in the ignored build directory for inspection:

```bash
cargo run -p rules --bin bake -- build \
  --source rules/eu4 \
  --output target/rules/eu4.pdcrules \
  --manifest target/rules/manifest.json
```

Official `pdc` binaries embed the first-party JSON source and generate a validated SQLite rules
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

Launch `pdc` from a configured path or from `PATH`. Editor configuration is intentionally
separate: VS Code uses its `paradoxcode.*` settings, while Zed uses
`lsp.pdc.initialization_options` in `.zed/settings.json`. The two editors do not read a shared
project file. The documented setup is for contributors, not the final installation experience.

`pdc` discovers, validates, indexes, and remembers the local EU4 installation on its own. The
first launch performs one non-blocking quick attempt when no explicit cache or previous attempt
exists: launcher metadata (Steam libraries, Epic manifests, GOG registry entries) and common
locations are probed once. If that produces no candidate, point the game directory setting at
the installation (VS Code: `paradoxcode.gameDirectory`) and reload; the cache is then built and
kept fresh automatically, with background reindexing when the installation changes.

Large dependency Mods can be indexed once and loaded from the persistent cache on every launch
instead of being rescanned:

In the VS Code extension, **ParadoxCode: Add Dependency** opens a folder-and-cache wizard and
writes the ordered entry to the workspace `paradoxcode.dependencies` setting. The matching Remove
and Open Dependency Settings commands are available from the Command Palette; new entries are
appended as the highest-priority dependency.

In Zed, the cache is declared in `.zed/settings.json`; `pdc` loads it in the background and
builds or rebuilds it automatically when the file is missing (a rules-hash change regenerates it
like the Vanilla cache):

```json
{
  "lsp": {
    "pdc": {
      "initialization_options": {
        "dependencies": [
          {
            "id": "gui-xu",
            "path": "/path/to/dependency-mod",
            "index": "/path/to/dependency.pdcindex"
          }
        ]
      }
    }
  }
}
```

While `index` is set, the dependency is not scanned live; after changing the dependency, delete
the stale cache file and restart the language server (command palette `pdc: restart`) so it is
rebuilt. Remove the `index` field to fall back to live scanning.

Long-running sessions can opt into a quiet, idle-gated source-root re-scan. The default is off.
In VS Code, set `paradoxcode.backgroundReindexIntervalMinutes` and
`paradoxcode.backgroundReindexIdleSeconds`; in Zed, pass the camelCase keys through its
`initialization_options`:

```json
{
  "backgroundReindexIntervalMinutes": 30,
  "backgroundReindexIdleSeconds": 15
}
```

The pass never replaces a newer edit or watched-file refresh, and changing either setting through
`workspace/didChangeConfiguration` takes effect without restarting the server.

Large workspaces can prune generated files or directories before they consume the scan budget.
Configure `paradoxcode.ignoreFilePatterns` and `paradoxcode.ignoreDirectories` in VS Code, or pass
`ignoreFilePatterns` and `ignoreDirectories` through Zed's `initialization_options`:

```json
{
  "ignoreFilePatterns": ["**/*.generated.txt"],
  "ignoreDirectories": ["generated", "build/cache"]
}
```

Patterns are bounded to 200 entries per list and 1024 characters per entry. `*` and `?` stay
within one path component; `**` spans directories; a pattern without `/` matches a basename at
any depth. The same filters apply to watched-file updates.

To hide a known diagnostic category, configure `paradoxcode.diagnosticIgnoreCodes` in VS Code, or
pass `ignoredErrorCodes` through Zed's `initialization_options` (the setting also updates live
through `workspace/didChangeConfiguration`):

```json
{
  "ignoredErrorCodes": ["LogicalContainer", "ModifierScopeMismatch"]
}
```

Codes use the stable LSP names shown in diagnostics. Unknown codes are rejected so a misspelled
setting cannot silently suppress nothing.

Diagnostic categories can also be remapped without changing the analysis result. Configure
`paradoxcode.diagnostics.severityOverrides` in VS Code, or `diagnosticSeverityOverrides` in Zed,
with stable codes and `error`, `warning`, `info`, `hint`, or `off`; the effective severity is
applied before publication limits and `validateWorkspace` aggregation:

```json
{
  "diagnosticSeverityOverrides": {
    "WrongScope": "warning",
    "UnknownLocalisationKey": "info"
  }
}
```

Use `paradoxcode.vanilla.mode = "cacheOnly"` in VS Code, or `vanillaMode: "cacheOnly"` in Zed,
to require an already available Vanilla cache. The `disabled` value omits the Vanilla source root
entirely. The default `auto` keeps the normal explicit-cache and one-time discovery behavior.
`performance.profile` / `performanceProfile` accepts `conservative`, `balanced`, or `fast` and
changes only bounded scan concurrency.

Completion sources can be narrowed independently of fixed source resolution priority with
`paradoxcode.completion.sourceLayers` in VS Code or `completionSourceLayers` in Zed. Localisation
hover and mission titles use the ordered preferred-language list, falling back to English when no
preferred language is present:

```json
{
  "preferredLocalisationLanguages": ["french", "english"],
  "completionSourceLayers": ["currentMod", "dependencies"],
  "performanceProfile": "conservative"
}
```

Initialization completion, watched-file refreshes, quiet background re-scans, and explicit
workspace refreshes publish diagnostics for closed Current Mod files by default, bounded to 2,000
files per pass. Set `paradoxcode.workspaceWideDiagnostics = false` in VS Code, or send
`workspaceWideDiagnostics: false` in Zed initialization/configuration settings, when the Problems
view should remain limited to open documents. `validateWorkspace` still computes its complete
summary when publication is disabled.

For a one-off suppression beside a piece of source, add `# cwtools-ignore <code>` to the same
line (or the immediately preceding/following line). The directive is read from raw text, so it
also works when the file has syntax errors; unknown names are ignored rather than hiding a
different diagnostic.

For an immediate refresh, invoke the advertised LSP `workspace/executeCommand` command
`pdc/reindexWorkspace`. It uses the same cancellation, serialized-worker, and revision-checked
commit path as the quiet pass and returns the new snapshot revision and source-file count.
For a complete Current Mod validation pass, invoke `validateWorkspace`; it performs the same
refresh and returns bounded counts for discovered/validated files and diagnostics by severity
(`totalFiles`, `validatedFiles`, `filesWithErrors`, `totalErrors`, `totalWarnings`, `totalInfos`,
and `totalHints`).

Run a repeatable whole-Current-Mod diagnostic pass against that Vanilla cache with the development
script below. It opens each relevant file through the real `pdc` transport and writes ignored
JSON and Markdown reports under `diagnostic-reports/`:

```bash
bash scripts/diagnose-current-mod.sh \
  --mod /path/to/current-mod \
  --vanilla-cache /path/to/vanilla.pdcindex
```

The command exits non-zero when errors are found; use `--fail-on warning` or `--fail-on none` to
change the automation threshold. Use `--help` for all options.

## Repository layout

| Path | Purpose |
| --- | --- |
| `crates/text` | Text, range, position, and path primitives |
| `crates/parser` | Loss-aware parser and canonical formatter |
| `crates/rules` | Game-neutral rules schema, runtime, and first-party compiler (`bake`) |
| `crates/game` | EU4 profile: game discovery, local config, and EU4 mission model |
| `crates/vfs` | Source roots, workspace scans, and the stable document data model |
| `crates/hir` | Rule-aware semantic lowering (definitions, scopes, templates) |
| `crates/index` | Workspace symbol index shards and the per-file analysis pipeline |
| `crates/engine` | Analysis host, immutable snapshots, query caching, and the `.pdcindex` persistence |
| `crates/ide` | Editor-neutral analysis queries (diagnostics, completion, navigation, rename) |
| `crates/pdc` | The `pdc` language server: LSP lifecycle and protocol boundary |
| `crates/tools` | Repository tooling (`check`, `release`, cache building) for CI and maintainers |
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

Releases are tag-driven: pushing a `v0.x.y` tag builds and verifies all five native `pdc`
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
redistribution boundaries are enforced by `bake` validation and the repository quality gates.
