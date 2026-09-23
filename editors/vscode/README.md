# ParadoxCode

English | [简体中文](#简体中文)

Complete support for EU4 modding in VS Code: diagnostics, completions, hover, go-to-definition,
references and rename, formatting, semantic highlighting, a live mission-tree preview, and
transparent localisation transcoding — all served by one `pdc` language server.

ParadoxCode is independent and unofficial. It is not affiliated with or endorsed by Paradox
Interactive.

## Setup

Install ParadoxCode from the VS Code Marketplace, trust and open an EU4 Mod workspace, then open
an EU4 or Localisation file. No language-server setup is required: in a trusted workspace,
ParadoxCode automatically downloads the matching `pdc` release for the current platform, verifies
its SHA-256 checksum, caches it in VS Code's global storage, and starts it. Unrelated workspaces
do not start ParadoxCode.

After installation, VS Code's **Get Started** page includes **Start using ParadoxCode**, a
walkthrough covering workspace trust, Mod folder selection, the automatic server download, Vanilla
symbols, diagnostics, and mission preview. The same guide can be reopened from
**Help > Get Started**.

If automatic Vanilla discovery cannot find the game, use **Choose EU4 Installation / Vanilla
Data** and select the installation folder containing `eu4.exe` plus `common`, `events`,
`missions`, `decisions`, and `localisation`; the server validates the folder, builds the local
Vanilla index, and retries. The same directory also enables mission textures. The first server
start may build or load the Vanilla index; progress is visible in the status bar and ParadoxCode
output.

Advanced users can set `paradoxcode.serverPath` or use **ParadoxCode: Select pdc Binary** for a
local build. If automatic setup is interrupted, **ParadoxCode: Install or Update pdc** retries it
and the output channel contains the actionable error. Use **Export Workspace Diagnostics** to
share a bounded JSON report, and **Reload ParadoxCode Language Server** after changing external
workspace resources.

Files anywhere below the workspace's `localisation/` directory are automatically assigned the
separate **Localisation** language, including nested files. EU4 script associations follow the
profile's configured source directories, including `common`, `customizable_localization`, `hints`,
the supported `history/*` folders, `map`, `music`, `missions`, `sound`, `tutorial`, `gfx`, and
`interface`. Script directories use direct-file associations; `map` uses its fixed
vanilla/reference-mod file names, while `localisation/` is the only recursive source tree.

The bundled server requires a modern LSP client that sends `workspaceFolders` during initialize.
Clients that send only the deprecated `rootUri` field are not supported.

## Mission Preview

Open a mission file under `common/missions` or `missions`, then choose **Open Mission Tree Preview
to the Side**. The preview supports live refresh, source navigation, texture-backed nodes, keyboard
navigation, and zoom controls. The view keeps its pan and zoom across refreshes, fitting only when
a different mission file is opened. The search box finds missions by localised title or id;
opening a result jumps to its source definition and centers the canvas on the node. A **Series**
panel mirrors the canvas layout column by column (`Slot N` blocks wrapping left to right) with a
checkbox per mission series: hidden series keep their canvas position but drop out of the canvas,
dependency arrows, search result highlighting, and the diagnostic summary, and each document
remembers its hidden set for the session.

## Transparent Localisation (Chinese)

Mods running the EU4dll double-byte patch store localisation as escape-tripled bytes. ParadoxCode
ships the transcoder and edits those files as readable Chinese through the `pdcloc://` view:

- Entry is path-based: every eligible file — any `localisation/**/*.yml` or a script file matching
  `paradoxcode.localisation.transparentScriptGlobs` (`**/*.txt` by default) — opens in the decoded
  view and takes over the raw tab, whatever its bytes look like. Plain readable files pass through
  unchanged; their quoted CJK is escape-encoded on the next save (announced by an informational
  `LocalisationWillTranscodeOnSave` hint).
- Saving writes the scoped form: escape triples only inside quoted strings, comments and code as
  readable UTF-8 — the game-side transcoder reads the strings exactly as with whole-file encoding.
  Partially escaped files self-heal on save. Saving is refused (never double-encoded) when a
  string already contains escape markers, or holds code points the ecosystem cannot round-trip.
- Stray escape markers outside every quoted string are damage: the file is shown as-is with a
  `LocalisationMixedEncoding` error anchored at the marker; fix it by hand.
- While a decoded view is active, the status bar shows **EU4 decoded view**; click it to open the
  raw transcoded file, and use the editor-title eye to peek at the raw bytes momentarily.
- Turn `paradoxcode.localisation.transparentEncoding` off to disable everything automatic (no
  `pdcloc://` provider, no redirection, no save-time encoding): the manual **Encode File (EU4dll
  Escape Form)** and **Decode File (Readable Text)** commands become available instead, each a
  one-shot disk rewrite with a `.pre-transcode.bak` backup written next to the file.

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
| `paradoxcode.modDirectory` | `""` | Project directory; empty uses the workspace root. |
| `paradoxcode.dependencies` | `[]` | Ordered dependency roots (`id`, `path`, optional `index`). |
| `paradoxcode.gameDirectory` | `""` | EU4 installation root for textures and guided Vanilla setup. |
| `paradoxcode.vanillaIndexCache` | `""` | Persistent `.pdcindex` cache path. |
| `paradoxcode.vanilla.mode` | `"auto"` | Vanilla policy: `auto`, `cacheOnly`, or `disabled`. |
| `paradoxcode.workspaceWideDiagnostics` | `false` | Publish diagnostics for closed Project files. Off by default; opened files are always validated. |
| `paradoxcode.backgroundReindexIntervalMinutes` | `0` | Quiet full re-scan interval; `0` disables it. |
| `paradoxcode.backgroundReindexIdleSeconds` | `15` | Required editor-idle window before a quiet re-scan. |
| `paradoxcode.ignoreFilePatterns` | `[]` | File globs excluded from workspace discovery. |
| `paradoxcode.ignoreDirectories` | `[]` | Directory globs excluded from workspace discovery. |
| `paradoxcode.diagnosticIgnoreCodes` | `[]` | Diagnostic categories hidden in Problems. |
| `paradoxcode.diagnosticIgnoreFiles` | `[]` | Client-side file globs hidden in Problems. |
| `paradoxcode.diagnosticLogging` | `false` | Log client-side diagnostic filtering counts. |
| `paradoxcode.diagnostics.severityOverrides` | `{}` | Remap diagnostic codes to `error`, `warning`, `info`, `hint`, or `off`. |
| `paradoxcode.localisation.preferredLanguages` | `[]` | Localisation language preference order. |
| `paradoxcode.localisation.transparentEncoding` | `true` | Master switch for the transparent EU4dll pipeline: on, eligible files open decoded and saves encode automatically; off, manual Encode/Decode commands. |
| `paradoxcode.localisation.transparentScriptGlobs` | `["**/*.txt"]` | Workspace-relative globs of script files eligible for transcoding (`latin1eu4`). Eligible files open in the decoded view whatever their bytes look like. |
| `paradoxcode.completion.sourceLayers` | `[project, dependencies, vanilla]` | Completion layers to include; resolution priority is unchanged. |
| `paradoxcode.performance.profile` | `"balanced"` | Bounded scan concurrency: `conservative`, `balanced`, or `fast`. |
| `paradoxcode.preview.refreshMode` | `"always"` | Preview refresh timing: `always`, `onSave`, or `manual`. |
| `paradoxcode.preview.zoomSensitivity` | `1` | Wheel zoom multiplier. |
| `paradoxcode.preview.showTextures` | `true` | Use EU4 textures when available. |
| `paradoxcode.preview.showExternalPrerequisites` | `true` | Show prerequisite missions outside the current file. |
| `paradoxcode.preview.showDiagnostics` | `true` | Show preview diagnostic counts, badges, and list entries. |

---

## 简体中文

ParadoxCode 为 VS Code 中的 EU4 模组开发提供完整支持：诊断、补全、悬停、跳转、查找引用与重命名、格式化、语义高亮、实时任务树预览，以及透明本地化转码——全部由同一个 `pdc` 语言服务器驱动。

ParadoxCode 独立且非官方：与 Paradox Interactive 无任何关联，也未获得其背书。

### 安装

从 VS Code 市场安装 ParadoxCode，打开并**信任**一个 EU4 模组工作区，再打开任意 EU4 或本地化文件。无需手动配置语言服务器：在受信任的工作区中，ParadoxCode 会自动下载与当前平台匹配的 `pdc` 发布版本，校验其 SHA-256 校验和，缓存到 VS Code 全局存储并启动。无关工作区不会启动 ParadoxCode。

安装后，VS Code 的 **Get Started** 页面会提供 **Start using ParadoxCode** 分步引导，覆盖工作区信任、模组目录选择、服务器自动下载、原版符号、诊断与任务树预览；也可随时从 **Help > Get Started** 重新打开。

若自动发现找不到游戏，请使用 **Choose EU4 Installation / Vanilla Data** 并选择包含 `eu4.exe` 以及 `common`、`events`、`missions`、`decisions`、`localisation` 的安装目录：服务器会校验目录、建立本地原版索引并重试。同一目录还会启用任务树纹理。首次启动可能建立或加载原版索引，进度显示在状态栏与 ParadoxCode 输出中。

高级用户可设置 `paradoxcode.serverPath`，或使用 **ParadoxCode: Select pdc Binary** 指向本地构建。自动安装被中断时，**ParadoxCode: Install or Update pdc** 会重试，输出通道中包含可操作的错误信息。使用 **Export Workspace Diagnostics** 可分享有界的 JSON 诊断报告；外部工作区资源变化后，可运行 **Reload ParadoxCode Language Server**。

工作区 `localisation/` 目录下的所有文件（包括嵌套文件）会自动使用独立的 **Localisation** 语言。EU4 脚本关联遵循 profile 配置的源目录，包括 `common`、`customizable_localization`、`hints`、受支持的 `history/*` 目录、`map`、`music`、`missions`、`sound`、`tutorial`、`gfx` 与 `interface`。脚本目录使用直接文件关联；`map` 使用固定的原版/参考模组文件名，`localisation/` 是唯一的递归源目录。

服务器要求现代 LSP 客户端在 initialize 时发送 `workspaceFolders`；仅发送已弃用 `rootUri` 字段的客户端不受支持。

以上为介绍与安装说明；任务树预览、透明本地化与全部配置项的详细文档见上文英文部分。
