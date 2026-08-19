# ParadoxCode VS Code extension

ParadoxCode provides EU4 diagnostics, completion, Hover, navigation, rename, formatting, semantic
highlighting, and a live mission-tree preview backed by `pdx-ls`.

## Setup

Open a Mod workspace. The extension first checks `paradoxcode.pdxLsPath`, the shared
`.pdx/project.toml` `[server].binary`, the checksum-verified server cache, and finally `pdx-ls` on
`PATH`. Use **ParadoxCode: Install or Update pdx-ls** for the guided release installer, or
**ParadoxCode: Select pdx-ls Binary** when you already have a local build.

Use **Select EU4 Installation Directory** to enable game textures, **Export Workspace Diagnostics**
to share a bounded JSON report, and **Reload ParadoxCode Language Server** after changing external
workspace resources.

The shared project configuration is also understood by the Zed extension. The first server start
may build or load the Vanilla index; progress is visible in the status bar and ParadoxCode output.

## Mission Preview

Open a mission file under `common/missions` or `missions`, then choose **Open Mission Tree Preview
to the Side**. The preview supports live refresh, source navigation, texture-backed nodes, keyboard
navigation, zoom controls, a mission list, and PNG/JSON export.

## Configuration

Path, dependency, Vanilla cache, diagnostic filtering, preview, and installer settings live under
the `paradoxcode.*` namespace. Localisation YAML and asset/sfx language contributions are
intentionally outside this VS Code release's explicit selector scope; the server still indexes
authoritative workspace data according to the EU4 profile.
