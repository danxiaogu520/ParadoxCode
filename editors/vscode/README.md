# ParadoxCode for VS Code

[Project overview](../../README.md) · [简体中文入口](../../README.zh-CN.md)

This guide owns installation, editor workflows, and troubleshooting. Settings, defaults,
commands, and agent tools come from the [generated extension reference](REFERENCE.md).

## Setup

1. Install [ParadoxCode from Visual Studio Marketplace](https://marketplace.visualstudio.com/items?itemName=paradoxcode.paradoxcode-vscode).
2. Open and trust the Mod folder, then open an EU4 script or localisation file.
3. The extension downloads the matching server release, verifies its checksum, and starts it.
4. If discovery misses your game installation, use the installation picker in the ParadoxCode
   walkthrough. Select your EU4 installation, rather than a text-only corpus copy.

The extension's **Get Started** walkthrough supplies the current command labels and guided setup.
For an offline install, download the VSIX and a matching native server archive from
[GitHub Releases](https://github.com/danxiaogu520/ParadoxCode/releases), extract the server, and set
`paradoxcode.serverPath` to its executable. Supported targets and archive names are defined in
[server-distribution.json](server-distribution.json).

## Workspace and dependencies

Open the directory containing the Mod's `common`, `events`, or other game folders.
Use the dependency commands from the Command Palette to add, remove, or reorder dependency roots.
The ordered `paradoxcode.dependencies` array may select live source roots or persistent indexes.
When an explicitly selected dependency index becomes stale, rebuild it or remove its `index`
field to resume live scanning. Unsaved editor buffers participate in analysis.

Vanilla discovery and indexing are guided by the walkthrough. The `paradoxcode.vanilla.*` settings
control whether local Vanilla data is discovered or an existing cache is required. Configured
exclusions prune generated files before indexing. Use the reference for exact properties and defaults.

## Diagnostics and language features

The [diagnostic guide](../../crates/ide/DIAGNOSTICS.md) explains stable codes and remedies.
Diagnostic ignore codes and severity overrides are extension settings; unknown codes are rejected.
The workspace-wide diagnostics setting controls publication for closed files, while validation
commands can still compute a complete workspace summary.

Localisation is indexed for lookup/navigation and receives transparent-encoding safety feedback;
it does not publish server diagnostics or completion items. Script rename is restricted to writable
Mod sources, and formatting refuses malformed input it cannot safely preserve.

## Search

Use the editor-title search button or **ParadoxCode: Search** to open the retained search panel.
Rules, definitions, localisation, and full text keep separate queries and filters. The first open
uses selected editor text; focusing an existing panel preserves its query. Source checkboxes
select the current Mod, individual dependencies, and Vanilla independently.

Localisation searches complete readable values, including text beyond hover previews. Multiple
words must all occur; switch to key mode for key lookup. Languages come from entry headers and
an explicit language never falls back to another language. Definition search ranks exact,
prefix, substring, and subsequence matches. Version filters retain the workspace's coverage state
when a higher-priority source is hidden. Copy and find-references are separate result actions;
references open a returnable view and count semantic uses rather than ordinary text occurrences.

Full text includes readable files outside semantic definition categories, comments, and strings.
Unsaved Markdown and other text buffers participate even when they are outside the language
server’s document selectors. Source-control and ParadoxCode cache metadata are excluded.
Path filters use source-relative globs, separated by commas; exclusions take precedence. Expand
a match to inspect nearby lines. Searches use editor buffers, and navigation preserves existing
decoded-document identities without saving or transcoding a source file. Results open in a
text-editor group beside the panel. If an editor represents characters differently, navigation
relocates a unique literal or explicitly opens the source line instead of using an unsafe column.

Loading another page requires the same workspace and query snapshot. Edits refresh results;
cancelled requests cannot overwrite newer input. Coverage notices identify unavailable source
files, text budgets, and output limits. Such results are partial, including when no available
file matches. Cached semantic definitions do not imply that full localisation values or file
contents are available. This implementation is undergoing the full
[search acceptance matrix](../../SEARCH-PROPOSAL.md#验收矩阵).

## Mission Preview

Open a mission file under `common/missions` or `missions`, then choose **Open Mission Tree Preview
to the Side**. The preview supports live refresh, source navigation, texture-backed nodes, keyboard
navigation, and zoom controls. The view keeps its pan and zoom across refreshes, fitting only when
a different mission file is opened. The search box finds missions by localised title or id;
opening a result jumps to its source definition and centers the canvas on the node. A **Series**
panel mirrors the canvas layout column by column (`Slot N` blocks wrapping left to right) with a
checkbox per mission series: hidden series keep their canvas position but drop out of the canvas,
dependency arrows, search result highlighting, and the diagnostic summary, and each document
remembers its hidden set for the session. Run **Refresh Mission Tree Preview** from the Command
Palette for a manual refresh. Run **Open Mission Icon Picker** from the Command Palette or the
mission editor's context menu.

## Transparent Localisation (Chinese)

Mods running the EU4dll double-byte patch store localisation as escape-tripled bytes. ParadoxCode
ships a Rust-server-backed transcoder. By default, files keep their ordinary `file://` view:

- Use the editor-title **Decode Quoted Text (Readable Text)** button (down arrow) to write readable
  strings to disk, or **Encode Quoted Text (EU4dll Escape Form)** (up arrow) to write escaped strings.
  The buttons apply to `localisation/**/*.yml` and workspace script files matching
  `paradoxcode.localisation.transparentScriptGlobs` (`**/*.txt` by default).
- Both buttons operate only inside quoted strings. **Encode Quoted Text** converts readable fragments
  and preserves existing escape triples byte-for-byte; **Decode Quoted Text** decodes those triples
  and preserves readable fragments. This also applies when one string is partially transcoded.
  Comments and other unquoted bytes are never converted, including already-escaped comments
  in legacy files. Existing escape triples are never encoded again; decoded readable fragments
  are never decoded again.
- Save or close any unsaved editor for the file before converting. Each conversion writes a
  `.pre-transcode.bak` backup next to the original file before changing its bytes. The same
  commands are available in the Command Palette and Explorer context menu.
- Damaged escape sequences inside strings refuse the entire conversion and report a byte
  position. A conversion with no changes writes neither the file nor its backup. Changes made
  to the file or its editor while conversion is running require a retry.

The automatic transparent pipeline and editable `pdcloc://` decoded views are **experimental**
and **disabled by default**. Search, navigation, and agent integration have known limitations.
To opt in, set `paradoxcode.localisation.transparentEncoding` to `true` and reload the VS Code
window. Changing the switch requires a window reload. When enabled:

- Entry is path-scoped but content-gated: an eligible file — any `localisation/**/*.yml` or a
  script file matching `paradoxcode.localisation.transparentScriptGlobs` (`**/*.txt` by default)
  — takes over the raw tab and opens in the decoded view only when it actually participates in
  transcoding: an escaped or damaged form, or plain text whose quoted CJK a save would encode.
  Transcode fixed points (no escape markers, no quoted CJK) stay on their ordinary `file://` view
  so search, diff, git, and timeline keep working on them; a fixed point that later grows quoted
  CJK is promoted to the decoded view automatically. Files rendered inside a diff editor are
  never taken over. `paradoxcode.localisation.autoOpen` picks the policy: `needsTranscode` (this
  behaviour, the default), `always` (the legacy takeover of every eligible file whatever its
  bytes look like), or `off`. Plain readable files pass through unchanged; their quoted CJK is
  escape-encoded on the next save (announced by an informational `LocalisationWillTranscodeOnSave`
  hint).
- Saving writes the scoped form: escape triples only inside quoted strings, comments and code as
  readable UTF-8. Saving is refused when a
  string already contains escape markers, or holds code points the ecosystem cannot round-trip.
- Stray escape markers outside every quoted string are damage: the file is shown as-is with a
  `LocalisationMixedEncoding` error anchored at the marker; fix it by hand.
- While a decoded view is active, the status bar shows **EU4 decoded view (experimental)**; click it to open the
  raw transcoded file. Run **Peek at the Raw Transcoded Form** from the Command Palette to peek at
  the raw bytes momentarily; press Escape to return to the decoded view. **Open in Decoded
  (Chinese) View (Experimental)** remains available in the Command Palette and context menus.
- Turn `paradoxcode.localisation.transparentEncoding` off and reload the window to return to
  the default manual workflow. The decoded-view provider and automatic pipeline are disabled;
  the **Decode Quoted Text** and **Encode Quoted Text** buttons become available again.

## Agent tools

The running language server supplies read-only workspace queries and in-memory draft validation.
Use the [generated tool reference](REFERENCE.md#agent-tools) for names, input schemas, and prompt references.
If tools are absent after extension activation, run **Developer: Reload Window** once.
External clients use the [MCP entry point](../../crates/pdc/README.md#mcp-server).

## Configuration

Use VS Code Settings under `paradoxcode`, or workspace `settings.json` for shared project choices.
[REFERENCE.md](REFERENCE.md#settings) is generated from the same manifest VS Code reads, including
localized descriptions, defaults, enums, and array/object constraints. This reference describes
extension defaults; editor-neutral initialization behavior belongs to the
[server integration guide](../../crates/pdc/README.md).

## Troubleshooting

- Server missing or download failed: inspect the ParadoxCode output channel, installation policy,
  selected executable, and whether a matching release exists. A source checkout may be newer than
  the published release; point it at your locally built server.
- No language features: trust the workspace, open a recognized game file, and check the active
  language mode and server output. The extension's language/activation registrations live in
  [package.json](package.json).
- Missing symbols or textures: check the selected Mod, dependency order, game installation, and
  Vanilla policy. A text-only corpus does not contain the game's image assets.
- Settings changed: many server settings apply live; executable/installation changes require
  restarting the server. Reload it from the Command Palette when diagnosing configuration changes.
- Transcoding refused: consult the diagnostic guide and repair the original content before retrying.

For a bug, use the [issue template](https://github.com/danxiaogu520/ParadoxCode/issues/new/choose)
with a minimal repository-owned reproduction. Keep licensed game content and machine paths private.
Extension development and validation are described in [CONTRIBUTING.md](../../CONTRIBUTING.md).
