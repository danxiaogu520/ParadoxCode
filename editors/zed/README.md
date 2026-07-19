# ParadoxCode Zed extension

The extension is a thin editor client. It owns language metadata, Tree-sitter query assets, the
bundled read-only rules artifact, and server discovery/launch configuration; analysis and EU4
semantics remain in the Rust core.

The extension directory is deliberately outside the core Cargo workspace. The three grammar
directories under `../../grammars/` are the source of truth; the development manifest points at
the repository's reachable Git remote with a pinned revision and a grammar-specific `path`. Run
`python3 scripts/prepare-zed-dev-manifest.py` from the repository root after changing the
grammar revision or remote. A published manifest must replace those URLs with the read-only
mirrors and pinned revisions described in the Phase 0 spike.

Because `.txt` is shared by many languages, PdxScript does not globally claim that suffix. Apply
[`recommended-settings.json`](recommended-settings.json) to an EU4 Mod workspace to associate
known EU4 directories with PdxScript, localisation, and CSV. The complete list will later be
generated from `Eu4Rules`; this Phase 1 file is deliberately conservative and is not a semantic
rule table.

Install the directory as a Zed development extension, ensure `pdx-ls` is on Zed's worktree PATH
(or configure `lsp.pdx-ls.binary.path`), and open an EU4 Mod workspace with the recommended
settings. The extension registers one `pdx-ls` server for all three languages and passes
`--rules bundled-rules/eu4.pdxrules` by default. A configured binary argument list can override
that path for development or platform-specific packaging.

The server uses the opened worktree as the current Mod root. Rename produces a WorkspaceEdit only
for that root and open overlays; Vanilla and dependency definitions are rejected as read-only.
