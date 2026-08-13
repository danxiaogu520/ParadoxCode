# ParadoxCode Zed extension

The extension is a thin editor client. It owns language metadata, Tree-sitter query assets, `pdx-ls` discovery/installation and launch configuration; parsing, indexing, analysis and EU4 semantics remain in the Rust core.

`editors/zed` is outside the core Cargo workspace. The grammar source of truth is
`grammars/tree-sitter-eu4`. The development manifest pins the repository revision and grammar path;
regenerate it after changing those values:

```text
cargo run --locked --bin pdx -- dev prepare-manifest --root .
```

## Development installation

Install `editors/zed` as a Zed development extension. The static
[`recommended-settings.json`](recommended-settings.json) associates known EU4 directories with the
registered language; `.txt` is not claimed globally because it is shared by many languages. Files not
covered by the fragment can be assigned manually in Zed.

With no project configuration, the opened worktree is the Current Mod. For separate Mod and dependency
roots, pass `.pdx/project.toml` through Zed initialization options:

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

Alternatively configure the roots inline; the extension declares a JSON Schema for these options, so
Zed offers completion, validation, and hover documentation while editing the settings file:

```json
{
  "lsp": {
    "pdx-ls": {
      "initialization_options": {
        "modDirectory": "D:/Games/MyMod",
        "dependencies": [
          {
            "id": "gui-xu",
            "path": "D:/SteamLibrary/workshop/content/236850/3047072888",
            "index": "D:/Games/MyMod/.pdx/cache/gui-xu.pdxindex"
          }
        ],
        "vanillaIndexCache": "D:/Games/EU4/vanilla.pdxindex"
      }
    }
  }
}
```

While a dependency declares `index`, `pdx-ls` loads the persistent cache instead of scanning the
dependency live, and rebuilds it in the background when the file is missing. After changing the
dependency, rebuild its cache (`pdx index dependency --id <id> --source <path> --output <cache>`)
and restart the language server; remove the `index` field to fall back to live scanning.

Restart the language server after changing initialization options. The extension does not implement
symbol extraction, scope derivation, diagnostics, Vanilla discovery or rule interpretation.

## Server selection and installation

The server executable is selected in this order:

1. explicit `lsp.pdx-ls.binary.path`;
2. `pdx-ls` on the worktree `PATH`;
3. the matching official GitHub Release asset.

Release assets are streamed with size limits, verified with their SHA-256 sidecar, and restricted to a
single precisely named executable in a `.tar.gz` (Linux/macOS) or `.zip` (Windows). Path traversal,
symlink, extra-member, CRC, compression and metadata errors are rejected. The extracted executable is
cached with a second SHA-256 digest and revalidated before reuse.

The extension launches the server without a rules argument and never carries or overrides semantic
rules. Zed-configured binary arguments are passed through; the official server rejects unsupported
arguments. Vanilla cache management is provided by `pdx setup vanilla`, `pdx index vanilla`, and the
server's current initialization flow, not by extension code.
