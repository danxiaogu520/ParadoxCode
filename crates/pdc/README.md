# Language-server and MCP integration

The `pdc` crate ships the `paradoxcode` stdio language server and its native MCP adapter.
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

## MCP server

The shipped `paradoxcode` executable includes a native stdio MCP adapter:

```json
{
  "mcpServers": {
    "paradoxcode": {
      "command": "/path/to/paradoxcode",
      "args": ["mcp", "--workspace", "/path/to/your-mod"]
    }
  }
}
```

Run `paradoxcode mcp --help` for options. `--server PATH` selects an explicit LSP child;
the default child is the same executable. `--timeout-ms` controls requests and
`--vanilla-mode auto|cacheOnly|disabled` controls the Vanilla source policy.
`PDC_MCP_SERVER`, `PDC_MCP_WORKSPACE` and `PDC_MCP_TIMEOUT_MS` are supported.

Workspace selection uses the explicit path, then file roots supplied by the MCP client,
then the working directory, excluding the home directory fallback. Root changes reset the
LSP session. Logs go to stderr; stdout carries newline-delimited JSON-RPC exclusively.

The adapter embeds the extension's `contributes.languageModelTools` declarations, including
descriptions and input schemas, so it exposes the same script/localisation zones and read-only
tool surface. Validation reads draft text without writing it to the Mod. Tool errors return
`isError` content; protocol errors return JSON-RPC errors. Closing stdin gracefully stops the
LSP child. It shares the bounded [native transport client](src/client.rs) with developer tools.

Real-process MCP initialization, listing, rules, validation, workspace/localisation queries,
ping and shutdown are covered by `cargo run --locked -p tools -- lsp test`. Runtime requests
remain in [the server crate](src/mcp/mod.rs); developer audits and reports are documented in
[tools](../tools/README.md).

## Local indexes

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
