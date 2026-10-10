# Language-server integration

The `pdc` crate ships the `paradoxcode` stdio language server.
For VS Code, start with
[the extension guide](../../editors/vscode/README.md); source builds follow
[CONTRIBUTING.md](../../CONTRIBUTING.md#build-and-debug).

## Other editors

Obtain a native server archive and its checksum from
[GitHub Releases](https://github.com/danxiaogu520/ParadoxCode/releases).
The [distribution manifest](../../editors/vscode/server-distribution.json) declares available targets
and archive layouts. Launch the extracted `paradoxcode` executable over stdio.

Send an LSP `initialize` request with at least one `workspaceFolders` entry.
Clients sending only deprecated `rootUri` are rejected. Initialization and configuration handling
are implemented in [initialize.rs](src/initialize.rs); advertised capabilities come from that code.
The server embeds the first-party rules and accepts no external rule source path.

Use [VS Code's generated configuration reference](../../editors/vscode/REFERENCE.md#settings)
as a guide to shared `paradoxcode` settings, while respecting the server's initialization contract.
Editor defaults need not equal defaults for an independent LSP client.

## External MCP integration

MCP is maintained in the independent `ParadoxCodeMCP` project. Its `paradoxcode-mcp`
executable connects to this language server over stdio using `--server /path/to/paradoxcode`.
The language-server executable no longer accepts the `mcp` subcommand. MCP tool declarations,
client instructions, build configuration, and MCP contract tests live in the separate project.
The editor-neutral `pdc/*` query requests remain available to external clients.

## Local indexes

Template-generated disk facts are discovered in immutable rounds with positive and
negative lookup dependencies. Scans, disk events and persistent refresh share the
[index discovery queue](../index/src/fact_stabilization.rs). Disk edits rebuild the
affected reader component before discovery, so deleted generated facts are revoked.
Cancelled, oscillating or budget-limited candidates do not replace the committed index.
Overlay discovery uses the same transaction contract and retains the latest syntax
with explicit incomplete fact coverage after a failed solve. Persistent shards retain
whether reference discovery completed, so cache installation cannot erase a rename
frontier. Pending workspace validation may run alongside immutable query requests;
mutation workers still gate its start and stale revisions are requeued.

The repository CLI builds persistent indexes and performs guided Vanilla setup. Read its
current arguments instead of relying on an independently maintained option table:

```sh
cargo run --locked -p tools -- --help
```

Selected dependency indexes replace live scans for that root. Rebuild them when their source
changes. Both index and syntax caches use the LSP release version from workspace `Cargo.toml`.
Every version change forces regeneration; old indexes cannot substitute for a failed rebuild.
No separate cache schema version or analyzer build hash is maintained. Rule fingerprints remain
available for audit identity. Cache mechanics are documented with [their implementation](../engine/src/index_cache/mod.rs).

Published versions are immutable. For development builds that change semantics without changing
the version, rebuild dependency/Vanilla indexes and clear the persistent parse-cache directory
reported in the server's startup log.

## Editor search

The VS Code panel uses independent, cancellable snapshot requests:

- `pdc/editorSearchContext` returns source roots, loaded language choices, definition types,
  rule contexts/scopes, and the workspace revision. Omitted or null parameters are accepted. It does not materialize full values or
  enumerate the full-text corpus when opening an empty panel.
- `pdc/editorSearch` selects `rules`, `definitions`, `localisation`, or `text`. It returns groups
  of independently addressable versions/locations, confirmed counts, coverage limitations, and
  a continuation offset. Continuations require both the original revision and opaque query
  snapshot; a mismatch produces LSP content-modified (`-32801`) rather than mixing pages.
- `pdc/editorSearchReferences` validates a returned definition identity and preserves source
  filtering. Overridden definitions do not inherit the winner's references. Localisation uses
  refer to the semantic key, independently of a selected translation language.
- `pdc/editorSearchDetail` retrieves a full localisation value or rule explanation on demand,
  with explicit feedback when its display budget is reached. Full-value details retain the
  materialized corpus generation; an evicted snapshot restarts rather than mixing versions.

Request shapes and bounds are authoritative in [the adapter](src/requests/search.rs); matching,
full-value extraction, and readable-to-source mappings live in [IDE search](../ide/src/search.rs).
The full-text corpus is independently materialized by [the engine](../engine/src/search_text.rs),
uses workspace exclusion rules and source encoding, and has one retained cache slot per text
corpus and full-value localisation corpus. Source-control and ParadoxCode cache metadata are
excluded. Unsaved documents outside the language-client selectors travel as bounded inline
buffers for full text; they replace the same physical disk file without changing analysis state. A file
watch epoch distinguishes external changes to text outside semantic file categories. Budgets
bound files, retained text, output groups, versions, and expanded details; coverage/output
notices are part of the result contract. Unreadable or changed backing reference caches are
reported instead of being counted as a complete empty reference result. Binary game asset categories are excluded. Persistent
index formats and existing external-client query shapes and limits are unchanged. Shared source
ownership follows the configured global load order; opening a dependency buffer does not
promote it above the Mod, and merge files keep contributions from other roots.

Search data shares immutable file text and metadata. A validated, bounded previous corpus
reuses unchanged localisation files across document edits; changed files update their values
and ordering. First materialization uses at most two configured source workers, with a serial
retry when a file needs the complete remaining memory allowance. Disk text survives buffer-only
revisions, while index changes, saves and external watch epochs invalidate disk reads.
Ordinary ASCII text bypasses per-byte mapping; Unicode folding and encoded carriers retain
the original mapping path. Localisation positions and JSON are materialized for the requested
page only, preserving confirmed totals, 200 displayed locations per page and 100 versions per group.
These are in-memory optimizations; persistent cache formats and protocol shapes are unchanged.

## Boundaries

`pdc` owns protocol adaptation, transport, document versions, workers, and stale-result rejection.
`ide` owns semantic query results. Integration tests under [src/tests](src/tests) exercise real
transport, cancellation, overlays, and source-root behavior. Diagnostic reference links point
at the [IDE diagnostic guide](../ide/DIAGNOSTICS.md).
