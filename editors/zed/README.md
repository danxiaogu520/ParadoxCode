# ParadoxCode Zed extension

The extension is a thin editor client. It owns language metadata, Tree-sitter query assets, the
server discovery and launch configuration; analysis and EU4 semantics remain in the Rust core.
The official `pdx-ls` binary embeds the only supported first-party EU4 rules.

The extension directory is deliberately outside the core Cargo workspace. The three grammar
directories under `../../grammars/` are the source of truth; the development manifest points at
the repository's reachable Git remote with a pinned revision and a grammar-specific `path`. Run `cargo run --bin pdx -- dev prepare-manifest --root .` from the repository root
after changing the grammar revision or remote. A published manifest must replace those URLs with the read-only
mirrors and pinned revisions described in the Phase 0 spike.

Because `.txt` is shared by many languages, Europa Universalis IV does not globally claim that
suffix. Apply [`recommended-settings.json`](recommended-settings.json) to an EU4 Mod workspace to
associate known EU4 directories with Europa Universalis IV, localisation, and CSV. The complete
list will later be generated from the EU4 `RuleSet`; this Phase 1 file is deliberately conservative
and is not a semantic rule table.

Install the directory as a Zed development extension and open an EU4 Mod workspace with the
recommended settings. An explicitly configured `lsp.pdx-ls.binary.path` takes precedence, followed
by `pdx-ls` on the worktree PATH. A published extension otherwise downloads the exact same-version
GitHub Release asset for the current platform, validates its named SHA-256 sidecar, rejects
multi-file/path-traversing archives, enforces bounded sidecar/archive/decompressed sizes, and caches
the executable in the extension work directory. The cache stores a separate executable SHA-256 and
revalidates it before every reuse; missing, truncated, or changed cache entries are removed and
downloaded again. HTTP bodies are consumed through Zed's streaming API and rejected before a chunk
would grow the buffer past its limit.
The extension registers one `pdx-ls` server for all three languages and launches it without a rules
argument. Rules cannot be replaced through editor settings or project files.

With no project configuration, the server uses the opened worktree as the current Mod root. For a
workspace containing a separate Mod directory and ordered dependency Mods, configure
`lsp.pdx-ls.initialization_options.projectConfig` in `.zed/settings.json`:

```json
{
  "lsp": {
    "pdx-ls": {
      "initialization_options": {
        "projectConfig": ".pdx/project.toml"
      }
    }
  }
}
```

Initialization options require a language-server restart after changes.
Rename produces a WorkspaceEdit only for the current Mod and its open
overlays; dependency and cached Vanilla definitions are rejected as read-only. The `pdx setup vanilla` discovery flow and `pdx index vanilla` command handle Vanilla cache management.
