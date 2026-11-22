# ParadoxCode for VS Code

[Project overview](../../README.md) · [简体中文入口](../../README.zh-CN.md)

This guide owns installation, editor workflows, and troubleshooting. Settings, defaults,
and commands come from the [generated extension reference](REFERENCE.md).

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

Expand **ParadoxCode Loaded Files** in the Explorer to fetch the indexed file list. The view starts collapsed
in new workspaces and reuses its list until you refresh it or restart the server.

## Diagnostics and language features

The [diagnostic guide](../../crates/ide/DIAGNOSTICS.md) explains stable codes and remedies.
Diagnostic ignore codes and severity overrides are extension settings; unknown codes are rejected.
The workspace-wide diagnostics setting controls publication for closed files, while validation
commands can still compute a complete workspace summary.

Localisation is indexed for lookup/navigation and receives transparent-encoding safety feedback;
it does not publish server diagnostics or completion items. Script rename is restricted to writable
Mod sources, and formatting refuses malformed input it cannot safely preserve.

Document and workspace formatting use the same EU4 list layouts. Mission prerequisites expand
from two entries, fixed vectors such as colours and coordinates stay together, and long province
lists wrap in groups. Formatting also expands complex Script blocks inside single-line quoted
arguments. Workspace formatting applies to UTF-8 scripts on disk.

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
contents are available.

## Definition hovers

Definition hovers show available localisation in a **Field** and **Text** table. Click a
field to open its localisation entry or **Defined in** at the bottom to open the symbol.
Localisation used by a script effect or trigger can be inspected by hovering that reference.

Hover an event definition or reference and choose **Preview Event** to open its game-style window beside the editor. The preview follows that event while you edit its script or localisation. Click its title, description or option text to open the corresponding localisation entry. Conditional descriptions link to their script. The window grows with the description and scrolls when it exceeds the panel. Artwork is shown when available.

Event and mission previews render EU4 colour codes, `\n` line breaks and inline country flags
such as `@FRA`. If a flag texture is unavailable, the tag stays visible as text.

Hover a mission or a prerequisite in `required_missions` and choose **Locate in Mission Tree** to open its definition file and center the node, including a node in a hidden series. Sprite and texture references keep their inline image previews. The existing `hover.eventCard` and `hover.missionCard` preferences control the respective hover links.

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

Choose **ParadoxCode › Localisation: Encoding Mode** in workspace settings. It applies to
`localisation/**/*.yml`, including `replace/`. **Automatic** is the default. The previous
`transparentEncoding` preference is honoured until a mode is set explicitly. A legacy
`autoOpen: off` opts out of automatic management.

- In **Manual**, the editor-title Encode and Decode commands convert the current YML buffer,
  including unsaved text, as one undoable edit spanning the first through last changed lines. Running either command from Explorer opens the
  target in the editor. VS Code writes the result on its normal save schedule.
- In **Automatic**, a managed YML opens as readable Chinese and saves in EU4dll format on the
  editor's normal save schedule. The status bar identifies the decoded view. Mode changes apply live. A dirty decoded editor keeps its original save behaviour until closed. The raw-view and
  peek commands remain available for inspecting disk contents.
- In a workspace **TXT** file, select a continuous range and right-click **Convert Selection to
  Game-Encoded Text** or **Convert Selection to Readable Chinese Text**. Script conversion uses
  raw single-byte EU4dll escapes. Select complete escape sequences. Other files and unselected
  bytes are outside the conversion.
- When the editor can represent the result, the command is a normal undoable edit. A clean UTF-8
  file may reopen with ISO-8859-1 to hold game bytes losslessly. For an edit that requires a
  direct byte replacement, the extension asks before writing, checks for intervening changes
  and backs up the original. Pending changes require explicit saving before a direct write.
- Conversion uses the matching server executable's isolated binary stream. Damage or an
  unencodable character refuses the operation. Existing valid escapes remain byte-exact on encode. Encoded YML starts with a UTF-8 BOM.
- Editing and transcoding remain available on large YML files. Live language analysis pauses for
  a buffer exceeding the server's 16 MiB text or 32 MiB message limit and resumes when the text fits.

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
