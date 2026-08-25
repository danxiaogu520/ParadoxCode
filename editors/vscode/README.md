# ParadoxCode VS Code extension

ParadoxCode provides EU4 script and Localisation diagnostics, completion, Hover, navigation, rename,
formatting, semantic highlighting, and a live mission-tree preview backed by `pdx-ls`.

ParadoxCode is independent and unofficial. It is not affiliated with or endorsed by Paradox
Interactive.

## Setup

Install ParadoxCode from the VS Code Marketplace, trust and open an EU4 Mod workspace, then open an
EU4 or Localisation file. No language-server setup is required: ParadoxCode automatically downloads the matching
`pdx-ls` release for the current platform, verifies its SHA-256 checksum, caches it in VS Code's
global storage, and starts it. The download happens only in a trusted workspace that activates EU4
or Localisation support; unrelated workspaces do not start ParadoxCode.

After installation, VS Code's **Get Started** page includes **Start using ParadoxCode**, a detailed
walkthrough covering workspace trust, Mod folder selection, the automatic server download, Vanilla
symbols, diagnostics, and mission preview. The same guide can be reopened from **Help > Get Started**.

Advanced users can still set `paradoxcode.pdxLsPath`, add `[server].binary` to the shared
`.pdx/project.toml`, or use **ParadoxCode: Select pdx-ls Binary** for a local build. If automatic
setup is interrupted, **ParadoxCode: Install or Update pdx-ls** retries it and the output channel
contains the actionable error.

Use **Choose EU4 Installation / Vanilla Data** if automatic Vanilla discovery cannot find the game.
Select the installation folder containing `eu4.exe` plus `common`, `events`, `missions`, `decisions`,
and `localisation`; the server validates the folder, builds the local Vanilla index, and retries
without requiring a project file. The same directory also enables mission textures. Use **Export
Workspace Diagnostics** to share a bounded JSON report, and **Reload ParadoxCode Language Server**
after changing external workspace resources. Files anywhere below the workspace's `localisation/`
directory are automatically assigned the separate **Localisation** language, including nested files.
EU4 script associations follow the profile's configured source directories, including `common`,
`customizable_localization`, `hints`, the supported `history/*` folders, `map`, `music`, `missions`,
`sound`, `tutorial`, `gfx`, and `interface`. Script directories use direct-file associations;
`map` uses its fixed vanilla/reference-mod file names, while `localisation/` is the only recursive
source tree.

The shared project configuration is also understood by the Zed extension. The first server start
may build or load the Vanilla index; progress is visible in the status bar and ParadoxCode output.

The bundled server requires a modern LSP client that sends `workspaceFolders` during initialize.
Clients that send only the deprecated `rootUri` field are not supported.

## Mission Preview

Open a mission file under `common/missions` or `missions`, then choose **Open Mission Tree Preview
to the Side**. The preview supports live refresh, source navigation, texture-backed nodes, keyboard
navigation, zoom controls, a mission list, and PNG/JSON export.

## Configuration

Optional path, dependency, Vanilla cache, diagnostic filtering, preview, and installer settings live under
the `paradoxcode.*` namespace. `eu4` and `localisation` are separate language IDs so their syntax
grammars do not conflict, while both are served by the same `pdx-ls` process.
