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

## Boundaries

`pdc` owns protocol adaptation, transport, document versions, workers, and stale-result rejection.
`ide` owns semantic query results. Integration tests under [src/tests](src/tests) exercise real
transport, cancellation, overlays, and source-root behavior. Diagnostic reference links point
at the [IDE diagnostic guide](../ide/DIAGNOSTICS.md).
