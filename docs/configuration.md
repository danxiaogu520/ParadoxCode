# Workspace configuration

ParadoxCode treats one opened editor worktree as one EU4 project. The project can contain a
writable current Mod and zero or more read-only dependency Mods. Dependency support is useful
when the current Mod refers to symbols supplied by a required library or another Mod.

## Smallest setup

If the opened worktree is the current Mod directory itself, no project file is required. The
language server uses that worktree as the writable `CurrentMod` source root.

For a project that keeps the Mod and its dependencies below a common directory, create
`.pdx/project.toml` in the opened worktree:

```toml
mod_directory = "mod"

[[dependencies]]
id = "shared-foundation"
path = "dependencies/shared-foundation"

[[dependencies]]
id = "content-extension"
path = "dependencies/content-extension"
```

Then pass the project file through the editor's LSP initialization options. In Zed, put this in
the project's `.zed/settings.json` and merge it with the file associations from
`editors/zed/recommended-settings.json`:

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

Restart `pdx-ls` after changing initialization options. Zed only sends them when the language
server starts.

## Path and priority rules

`projectConfig`, `mod_directory`, and dependency paths may be absolute. Relative paths are
resolved from the opened worktree, not from the `.pdx` directory.

Dependencies are listed from lowest to highest priority. The complete winner order is:

```text
open unsaved document > current Mod > later dependency > earlier dependency > Vanilla cache
```

The server does not guess a Steam installation path or scan the game directory during normal
startup.

Every configured directory must already exist. Source roots may not be equal, nested, or overlap.
Dependency IDs must be non-empty and unique without regard to ASCII letter case. IDs determine
stable internal root identities, so rename an ID only when it represents a different dependency.

The current Mod is writable. Dependencies and cached Vanilla are deliberately read-only: they
participate in hover, completion, definition, and references, but rename will never edit them.

## Inline configuration

Clients may configure roots directly without a TOML file:

```json
{
  "modDirectory": "mod",
  "dependencies": [
    { "id": "shared-foundation", "path": "dependencies/shared-foundation" },
    { "id": "content-extension", "path": "dependencies/content-extension" }
  ]
}
```

When `projectConfig` and inline fields are both present, each inline field replaces the
corresponding TOML field. Unknown fields, malformed TOML, duplicate IDs, missing directories, and
overlapping roots make initialization fail with a specific configuration error instead of being
silently ignored.

## Vanilla cache

Build the local cache explicitly after choosing the EU4 installation directory:

```text
pdx index vanilla \
  --rules /path/to/eu4.pdxrules \
  --source /path/to/Europa Universalis IV \
  --output /path/to/vanilla.pdxindex
```

The command scans Vanilla once and writes a versioned SQLite index. Running the same command again
is the manual refresh operation. It records file locations, semantic definition/reference shards,
creation time, a source fingerprint, game ID, and the rules hash; it does not copy source text into
the cache. An existing unrelated SQLite file is never overwritten.

Add the result to `.pdx/project.toml`:

```toml
mod_directory = "mod"
vanilla_index_cache = ".pdx/cache/vanilla.pdxindex"
```

Normal LSP startup reads this cache without scanning or monitoring the original Vanilla directory.
A missing, corrupt, wrong-game, or overlapping cache produces an editor warning and analysis
continues without Vanilla symbols. A rules-hash mismatch also produces a warning, but the cache
remains loaded: upgrading the extension never silently deletes, migrates, or rebuilds local data.
