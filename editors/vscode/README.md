# ParadoxCode VS Code extension

ParadoxCode provides EU4 script and Localisation diagnostics, completion, Hover, navigation, rename,
formatting, semantic highlighting, and a live mission-tree preview backed by `pdc`.

ParadoxCode is independent and unofficial. It is not affiliated with or endorsed by Paradox
Interactive.

## Setup

Install ParadoxCode from the VS Code Marketplace, trust and open an EU4 Mod workspace, then open an
EU4 or Localisation file. No language-server setup is required: ParadoxCode automatically downloads the matching
`pdc` release for the current platform, verifies its SHA-256 checksum, caches it in VS Code's
global storage, and starts it. The download happens only in a trusted workspace that activates EU4
or Localisation support; unrelated workspaces do not start ParadoxCode.

After installation, VS Code's **Get Started** page includes **Start using ParadoxCode**, a detailed
walkthrough covering workspace trust, Mod folder selection, the automatic server download, Vanilla
symbols, diagnostics, and mission preview. The same guide can be reopened from **Help > Get Started**.

Advanced users can set `paradoxcode.serverPath` or use **ParadoxCode: Select pdc Binary** for a
local build. If automatic setup is interrupted, **ParadoxCode: Install or Update pdc** retries
it and the output channel contains the actionable error.

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

VS Code settings are independent from Zed's `.zed/settings.json`; no project file is shared between
the editors. The first server start may build or load the Vanilla index; progress is visible in the
status bar and ParadoxCode output.

The bundled server requires a modern LSP client that sends `workspaceFolders` during initialize.
Clients that send only the deprecated `rootUri` field are not supported.

## Mission Preview

Open a mission file under `common/missions` or `missions`, then choose **Open Mission Tree Preview
to the Side**. The preview supports live refresh, source navigation, texture-backed nodes, keyboard
navigation, zoom controls, a mission list, and PNG/JSON export.

## Transparent Localisation (Chinese)

Mods running the EU4dll double-byte patch store localisation as escape-tripled bytes. ParadoxCode
ships the transcoder and opens those files as readable Chinese through the `pdcloc://` view:

- Choose **ParadoxCode: Open in Decoded (Chinese) View** on any `localisation/**/*.yml` (or a
  script file matching `paradoxcode.localisation.transparentScriptGlobs`) — you can also accept
  the prompt shown when a transcoded file is opened through its raw path.
- Edits are re-encoded on save; the raw bytes on disk always stay game-ready. Saving is refused
  (never double-encoded) if the buffer itself already contains escape sequences or code points
  the ecosystem cannot round-trip.
- Files on the game read path that are still readable Chinese get a
  `LocalisationNotTranscoded` warning; use **ParadoxCode: Transcode Localisation File** to encode
  them in place (a `.pre-transcode.bak` backup is written next to the file).
- While a decoded view is active, the status bar shows **EU4 decoded view**; click it to open the
  raw transcoded file.

## Configuration

Optional path, dependency, Vanilla cache, diagnostic filtering, preview, and installer settings live under
the `paradoxcode.*` namespace. `eu4` and `localisation` are separate language IDs so their syntax
grammars do not conflict, while both are served by the same `pdc` process.

For dependencies, use the Command Palette commands **ParadoxCode: Add Dependency** and
**ParadoxCode: Remove Dependency**. Adding a dependency opens a folder picker, suggests an ID,
lets you choose live scanning or a persistent `.pdcindex` path, and writes the ordered list to the
workspace `paradoxcode.dependencies` setting. New entries are appended as the highest-priority
dependency; use **ParadoxCode: Open ParadoxCode Dependency Settings** to adjust the order or edit
the paths directly. The generated paths are workspace-relative whenever possible.

The complete setting surface is grouped below. Settings that affect the language server are applied
on the next server restart; preview settings take effect immediately.

| Setting | Default | Purpose |
| --- | --- | --- |
| `paradoxcode.serverPath` | `""` | Explicit `pdc` executable path. |
| `paradoxcode.serverInstallDirectory` | `""` | Machine-local verified server download directory. |
| `paradoxcode.server.installPolicy` | `"auto"` | Automatic server install policy: `auto`, `prompt`, or `never`. |
| `paradoxcode.modDirectory` | `""` | Current Mod directory; empty uses the workspace root. |
| `paradoxcode.dependencies` | `[]` | Ordered dependency roots (`id`, `path`, optional `index`). |
| `paradoxcode.gameDirectory` | `""` | EU4 installation root for textures and guided Vanilla setup. |
| `paradoxcode.vanillaIndexCache` | `""` | Persistent `.pdcindex` cache path. |
| `paradoxcode.vanilla.mode` | `"auto"` | Vanilla policy: `auto`, `cacheOnly`, or `disabled`. |
| `paradoxcode.workspaceWideDiagnostics` | `false` | Publish diagnostics for closed Current Mod files. Off by default; opened files are always validated. |
| `paradoxcode.backgroundReindexIntervalMinutes` | `0` | Quiet full re-scan interval; `0` disables it. |
| `paradoxcode.backgroundReindexIdleSeconds` | `15` | Required editor-idle window before a quiet re-scan. |
| `paradoxcode.ignoreFilePatterns` | `[]` | File globs excluded from workspace discovery. |
| `paradoxcode.ignoreDirectories` | `[]` | Directory globs excluded from workspace discovery. |
| `paradoxcode.diagnosticIgnoreCodes` | `[]` | Diagnostic categories hidden in Problems. |
| `paradoxcode.diagnosticIgnoreFiles` | `[]` | Client-side file globs hidden in Problems. |
| `paradoxcode.diagnosticLogging` | `false` | Log client-side diagnostic filtering counts. |
| `paradoxcode.diagnostics.severityOverrides` | `{}` | Remap diagnostic codes to `error`, `warning`, `info`, `hint`, or `off`. |
| `paradoxcode.localisation.preferredLanguages` | `[]` | Localisation language preference order. |
| `paradoxcode.localisation.transparentEncoding` | `true` | Enable the `pdcloc://` decoded read/write view over EU4dll-transcoded files. |
| `paradoxcode.localisation.transparentScriptGlobs` | `["history/**"]` | Workspace-relative globs of script files eligible for the decoded view (`latin1eu4`). |
| `paradoxcode.completion.sourceLayers` | `[currentMod, dependencies, vanilla]` | Completion layers to include; resolution priority is unchanged. |
| `paradoxcode.performance.profile` | `"balanced"` | Bounded scan concurrency: `conservative`, `balanced`, or `fast`. |
| `paradoxcode.preview.refreshMode` | `"always"` | Preview refresh timing: `always`, `onSave`, or `manual`. |
| `paradoxcode.preview.zoomSensitivity` | `1` | Wheel zoom multiplier. |
| `paradoxcode.preview.showTextures` | `true` | Use EU4 textures when available. |
| `paradoxcode.preview.persistViewport` | `false` | Remember pan and zoom per mission document. |
| `paradoxcode.preview.showExternalPrerequisites` | `true` | Show prerequisite missions outside the current file. |
| `paradoxcode.preview.showDiagnostics` | `true` | Show preview diagnostic counts, badges, and list entries. |
| `paradoxcode.preview.defaultExportDirectory` | `""` | Default export directory; relative paths use the workspace root. |
