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
modding in VS Code.

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
  spellings and indexed as `defined_text`, including overlay-aware `$NAME$` hover lookup.
  Localisation documents remain available to indexing and navigation but intentionally publish
  no LSP diagnostics or completion items, keeping prose editing quiet.
- Completion, hover, go-to-definition, references, and document/workspace symbols.
- Full semantic tokens support incremental `full/delta` responses with a bounded, revision-aware
  cache that is invalidated by edits and workspace refreshes.
- Viewport-limited `textDocument/semanticTokens/range` requests prune out-of-range syntax-tree
  subtrees before classification, keeping visible-editor highlighting proportional to the view.
- Rule-proven scope transitions are available as bounded `textDocument/inlayHint` annotations.
- Conflict-aware rename restricted to writable Mod sources.
- A conservative formatter that refuses to rewrite unsafe or malformed files.
- Workspace resolution across unsaved buffers, the project, ordered dependency Mods, and a
  persistent local Vanilla index.
- A stdio language server (`paradoxcode`) with cancellation, stale-result protection, and immutable
  analysis snapshots, including targeted watched-file updates for live Mod roots.
- A VS Code extension with zero-configuration, checksum-verified server setup, a first-run
  walkthrough, and a live mission-tree preview (texture-backed nodes, zoom, source navigation,
  PNG/JSON export).
- Read-only language-model tools (`vscode.lm`) so VS Code agent mode can work with the indexed
  workspace: a workspace summary, script-zone symbol search, hover semantics, diagnostics,
  position- and name-based reference lookup, the embedded rule database, and in-memory draft
  validation — all served by the already-running language server. Localisation queries live in
  their own zone (exact `loc_get`, fuzzy `loc_search`, prefix `loc_list`) and the two zones never
  cross. Every tool is prompt-referenceable (`#paradoxSearch`, `#paradoxValidate`, …) and listed
  only while the language server is running, so agent tool pickers can show and enable them.
- A stdio MCP server (`editors/vscode/scripts/mcp.mjs`) that exposes the same eleven read-only
  tools to any Model Context Protocol client — hand-rolled newline-delimited JSON-RPC with no
  SDK dependency, its tool manifest mirrored from the extension's contributions.
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
3. On first use the extension downloads the matching ParadoxCode server release for your platform,
   verifies its SHA-256 checksum, caches it, and starts it automatically. No language-server
   configuration is required.
4. If your EU4 installation is not discovered automatically, use **Choose EU4 Installation /
   Vanilla Data** and select the folder containing `eu4.exe` plus `common`, `events`, `missions`,
   `decisions`, and `localisation`.

The **Get Started** page includes a **Start using ParadoxCode** walkthrough covering all of this.

### Standalone binaries

Standalone `paradoxcode` binaries for Linux (x86_64, aarch64), macOS (x86_64, aarch64), and
Windows (x86_64) are attached to each [GitHub Release](https://github.com/danxiaogu520/ParadoxCode/releases)
as `.tar.gz`/`.zip` archives with `.sha256` sidecars. The language server embeds the first-party
EU4 rule source and never imports external rule files.

`paradoxcode` requires modern LSP initialization with at least one `workspaceFolders` entry. Clients
that send only the deprecated `rootUri` field are intentionally unsupported and receive an
`INVALID_PARAMS` response; use a current LSP client or upgrade the editor integration.

### MCP server

The same eleven read-only agent tools are also exposed as a stdio
[Model Context Protocol](https://modelcontextprotocol.io) server, so MCP clients can validate
drafts and query the rule database outside VS Code. Script-zone tools (`workspace`, `search`,
`context`, `diagnostics`, `references`, `symbol_references`, `rules`, `validate_text`) and
localisation-zone tools (`loc_get` exact lookup, `loc_search` fuzzy reverse search, `loc_list`
prefix listing) are strictly separated; result caps (for example 100 symbol-search entries,
20 localisation hits by default) are declared in each tool description. It needs a checkout of
this repository and a built (or downloaded) `paradoxcode` binary:

```json
{
  "mcpServers": {
    "paradoxcode": {
      "command": "node",
      "args": [
        "/path/to/ParadoxCode/editors/vscode/scripts/mcp.mjs",
        "--server", "/path/to/paradoxcode"
      ]
    }
  }
}
```

The workspace root to index is resolved in order: the `--workspace PATH` option (or
`PDC_MCP_WORKSPACE`), the MCP client's roots, then the working directory. Each server request
is bounded by a 120 s timeout (`--timeout-ms`); diagnostics go to stderr. The tool list mirrors
the extension's language-model tools one-to-one from the same manifest, and `--help` documents
every option.

## Project status

**Latest release: v0.4.2** (23 Sep 2026). The EU4 analysis and indexing features are implemented,
tested, and released through the tag-driven pipeline described under [Releases](#releases). See
the [changelog](CHANGELOG.md) for the complete version history. Early adopters should still expect
rough edges while 0.x matures; please report problems through the issue templates so they can be
fixed in the next release.

Known limitations of the current scope:

- CSV files are handled as syntax-only/opaque resources; there is no CSV parser yet.
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
    -> VS Code
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

Prerequisites: **Rust 1.98 or newer** and **Node.js 24 LTS** (for the VS Code extension).

```bash
git clone https://github.com/danxiaogu520/ParadoxCode.git
cd ParadoxCode
cargo build --locked --workspace
cargo test --locked --workspace --all-targets
```

Run the deterministic local checks explicitly, or select the affected group: `core`, `core-fast`,
`vscode`, `policy`, `artifact`, `fuzz`, or `perf`. There are no commit hooks:

```bash
cargo tools gates
```

The alias comes from `.cargo/config.toml`; the long spelling is `cargo run -p tools -- gates`.
The default `all` group means all default deterministic local groups (optimized `perf` remains
opt-in), not release readiness. Pull-request CI owns clean-checkout and cross-platform coverage,
and its `Conclusion` job is the stable merge authority required by branch protection.
Vulnerability feeds and optimized benchmarks run as scheduled audits so external changes cannot
block an unrelated pull request. The complete mapping from local feedback through release and
manual acceptance lives in
[docs/validation.md](docs/validation.md).

Validate the developer-maintained first-party rule source and regenerate its release manifest
with `bake`:

```bash
cargo run -p rules --bin bake -- build \
  --source rules/eu4 \
  --manifest rules/manifest.json
```

Official `paradoxcode` binaries embed the first-party JSON source and compile it straight into
the in-memory rule set at startup; there is no persisted rules artifact.

The EU4 source is intentionally split by responsibility. `catalog/` contains file categories,
symbol descriptors, and normalized records; `semantic/` contains executable rule alternatives
grouped by context and game-directory schema; `types/`, `values/`, and `localisation/` contain
the supporting semantic tables; and `profile/` contains the data-only EU4 filesystem, scope,
symbol, dynamic-value, and semantic-inheritance profile. `rules/eu4/manifest.json` lists every
fragment explicitly. The compiler merges those fragments into one logical `RulesModel`, and the
same canonical `rule_hash` covers both semantic data and profile data.

## Development setup

Launch `paradoxcode` from a configured path or from `PATH`. Editor configuration lives in the VS Code
extension's `paradoxcode.*` settings. The documented setup is for contributors, not the final
installation experience.

`paradoxcode` discovers, validates, indexes, and remembers the local EU4 installation on its own. The
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

While `index` is set, the dependency is not scanned live; after changing the dependency, delete
the stale cache file and restart the language server (command palette
**Reload ParadoxCode Language Server**) so it is
rebuilt. Remove the `index` field to fall back to live scanning.

Long-running sessions can opt into a quiet, idle-gated source-root re-scan. The default is off;
set `paradoxcode.backgroundReindexIntervalMinutes` and
`paradoxcode.backgroundReindexIdleSeconds` to enable it. The pass never replaces a newer edit or
watched-file refresh, and changing either setting through
`workspace/didChangeConfiguration` takes effect without restarting the server.

Large workspaces can prune generated files or directories before they consume the scan budget;
configure `paradoxcode.ignoreFilePatterns` and `paradoxcode.ignoreDirectories` for that.
Patterns are bounded to 200 entries per list and 1024 characters per entry. `*` and `?` stay
within one path component; `**` spans directories; a pattern without `/` matches a basename at
any depth. The same filters apply to watched-file updates.

To hide a known diagnostic category, configure `paradoxcode.diagnosticIgnoreCodes`; the setting
also updates live through `workspace/didChangeConfiguration`. Codes use the stable LSP names
shown in diagnostics. Unknown codes are rejected so a misspelled
setting cannot silently suppress nothing.

Diagnostic categories can also be remapped without changing the analysis result. Configure
`paradoxcode.diagnostics.severityOverrides` with stable codes and `error`, `warning`, `info`,
`hint`, or `off` values; the effective severity is applied before publication limits and
`validateWorkspace` aggregation.

Use `paradoxcode.vanilla.mode = "cacheOnly"` to require an already available Vanilla cache. The
`disabled` value omits the Vanilla source root entirely. The default `auto` keeps the normal
explicit-cache and one-time discovery behavior. `paradoxcode.performance.profile` accepts
`conservative`, `balanced`, or `fast` and changes only bounded scan concurrency.

Completion sources can be narrowed independently of fixed source resolution priority with
`paradoxcode.completion.sourceLayers`. Localisation hover and mission titles use the ordered
`paradoxcode.localisation.preferredLanguages` list, falling back to English when no preferred
language is present.

Initialization completion, watched-file refreshes, quiet background re-scans, and explicit
workspace refreshes publish diagnostics for closed Project files by default, bounded to 2,000
files per pass. `paradoxcode.workspaceWideDiagnostics` controls this; turned off, the Problems
view remains limited to open documents. `validateWorkspace` still computes its complete
summary when publication is disabled.

For a one-off suppression beside a piece of source, add `# cwtools-ignore <code>` to the same
line (or the immediately preceding/following line). The directive is read from raw text, so it
also works when the file has syntax errors; unknown names are ignored rather than hiding a
different diagnostic.

For an immediate refresh, invoke the advertised LSP `workspace/executeCommand` command
`pdc/reindexWorkspace`. It uses the same cancellation, serialized-worker, and revision-checked
commit path as the quiet pass and returns the new snapshot revision and source-file count.
For a complete Project validation pass, invoke `validateWorkspace`; it performs the same
refresh and returns bounded counts for discovered/validated files and diagnostics by severity
(`totalFiles`, `validatedFiles`, `filesWithErrors`, `totalErrors`, `totalWarnings`, `totalInfos`,
and `totalHints`).

Run a repeatable whole-Project diagnostic pass against that Vanilla cache with the development
script below. It opens each relevant file through the real server transport and writes ignored
JSON and Markdown reports under `diagnostic-reports/`:

```bash
node editors/vscode/scripts/diagnose.mjs \
  --mod /path/to/project \
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
| `crates/pdc` | The language-server crate: LSP lifecycle and protocol boundary (ships as the `paradoxcode` binary) |
| `crates/tools` | Repository tooling (`check`, `release`, cache building) for CI and maintainers |
| `editors/vscode/` | VS Code extension: server bootstrap, walkthrough, mission-tree preview |
| `rules/eu4/` | Authoritative first-party EU4 rule tree (catalog, semantic, supporting tables, profile) |
| `fuzz/` | Parser, edit, formatter, and HIR fuzz targets |

The current first-party EU4 rules target game version **1.37.5** (8,525 semantic rules, 121 file
categories, 2,667 symbol descriptors). The generated release manifest in `rules/manifest.json`
records the schema version, source format, canonical `rule_hash`, and artifact checksum.

## Releases

Releases are tag-driven: pushing a protected, annotated `v0.x.y` tag verifies that its commit is on
`main` with a successful `Conclusion` check, builds five native `paradoxcode` archives, five
checksum sidecars, and the VSIX, then verifies the eleven-file draft before publishing it once.
GitHub Actions uses only repository-owned source and fixtures; licensed game files and local
Vanilla sweep reports never enter CI or a Release. Visual Studio Marketplace publication remains
manual. Version history and per-release changes are tracked in
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
