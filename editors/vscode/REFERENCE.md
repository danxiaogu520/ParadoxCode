# Extension reference

<!-- Generated; edit package.json / package.nls.json, then run cargo run --locked -p tools -- documentation write. -->

[Usage and troubleshooting](README.md). This view reflects VS Code's manifest defaults.

## Settings

### `paradoxcode.backgroundReindexIdleSeconds`

Minimum editor-idle time before a quiet workspace re-scan can begin.

```json
{
  "default": 15,
  "maximum": 86400,
  "minimum": 0,
  "type": "number"
}
```

### `paradoxcode.backgroundReindexIntervalMinutes`

Quiet full workspace re-scan interval in minutes. Zero disables automatic re-scans.

```json
{
  "default": 0,
  "maximum": 10080,
  "minimum": 0,
  "type": "number"
}
```

### `paradoxcode.completion.iconPreview`

Append the decoded sprite image to completion documentation when a suggestion's label names a sprite (mission `icon`, event `picture`, `.gfx` references). Sprites are resolved from the workspace mod first, then the located game installation.

```json
{
  "default": true,
  "type": "boolean"
}
```

### `paradoxcode.completion.sourceLayers`

Source layers allowed to contribute completion members. Resolution priority remains fixed.

```json
{
  "default": [
    "project",
    "dependencies",
    "vanilla"
  ],
  "items": {
    "enum": [
      "project",
      "dependencies",
      "vanilla"
    ],
    "type": "string"
  },
  "type": "array"
}
```

### `paradoxcode.debug.enable`

Debug mode: routes the server's INFO trail to the ParadoxCode Debug output and records every LSP message plus internal scheduling decisions there. Applies live without restarting the server.

```json
{
  "default": false,
  "type": "boolean"
}
```

### `paradoxcode.debug.logFile`

Mirror the debug output to this file while debug mode is on. Relative paths resolve against the first workspace folder. The file is append-only and grows without rotation; disable debug mode after reproducing.

```json
{
  "default": "",
  "type": "string"
}
```

### `paradoxcode.dependencies`

Dependency Mods from lowest to highest priority. Each item is { id, path, index? }: with `index` the dependency loads from a persistent .pdcindex (fast startup; changes are picked up by the **ParadoxCode: Update Index Caches** command), without `index` the dependency is scanned live and reacts to file changes automatically.

```json
{
  "default": [],
  "items": {
    "properties": {
      "id": {
        "type": "string"
      },
      "index": {
        "type": "string"
      },
      "path": {
        "type": "string"
      }
    },
    "required": [
      "id",
      "path"
    ],
    "type": "object"
  },
  "type": "array"
}
```

### `paradoxcode.diagnosticIgnoreCodes`

Diagnostic codes to hide in the VS Code Problems view.

```json
{
  "default": [],
  "items": {
    "type": "string"
  },
  "type": "array"
}
```

### `paradoxcode.diagnosticIgnoreFiles`

Workspace-relative diagnostic file globs to hide, for example **/README.txt.

```json
{
  "default": [],
  "items": {
    "type": "string"
  },
  "type": "array"
}
```

### `paradoxcode.diagnosticLogging`

Write diagnostic filtering counts to the ParadoxCode output channel.

```json
{
  "default": false,
  "type": "boolean"
}
```

### `paradoxcode.diagnostics.severityOverrides`

Map diagnostic codes to effective severities. Use off to suppress a code.

```json
{
  "additionalProperties": {
    "enum": [
      "error",
      "warning",
      "info",
      "hint",
      "off"
    ],
    "type": "string"
  },
  "default": {},
  "type": "object"
}
```

### `paradoxcode.gameDirectory`

EU4 installation root containing eu4.exe. Used for mission textures and as the explicit Vanilla source during guided setup.

```json
{
  "default": "",
  "type": "string"
}
```

### `paradoxcode.hover.eventCard`

Render a game-style event window (background chrome, picture banner, title, description, option buttons) when hovering an event inside an `events/` file. Texture paths come from the server's index; the card composes with the same fonts as the mission tree preview.

```json
{
  "default": true,
  "type": "boolean"
}
```

### `paradoxcode.hover.missionCard`

Render a game-style mission card (frame, icon, trigger and reward markers, bitmap-font title) when hovering a mission inside a mission file. Texture paths come from the server's index; the card composes with the same fonts as the mission tree preview.

```json
{
  "default": true,
  "type": "boolean"
}
```

### `paradoxcode.hover.texturePreview`

Append a decoded texture image to sprite hovers (`icon = …`, event `picture`, `.gfx` sprite definitions, and `texturefile` values). Textures are resolved from the workspace mod first, then the located game installation.

```json
{
  "default": true,
  "type": "boolean"
}
```

### `paradoxcode.ignoreDirectories`

Glob patterns for directories excluded from workspace discovery.

```json
{
  "default": [],
  "items": {
    "type": "string"
  },
  "type": "array"
}
```

### `paradoxcode.ignoreFilePatterns`

Glob patterns for files excluded from workspace discovery.

```json
{
  "default": [],
  "items": {
    "type": "string"
  },
  "type": "array"
}
```

### `paradoxcode.localisation.autoOpen`

Automatic opening policy for the experimental `pdcloc://` decoded view; applies only when `paradoxcode.localisation.transparentEncoding` is enabled. `needsTranscode` (default): escaped or damaged files, or plain text whose quoted CJK a save would encode, enter the decoded view. `always`: every eligible file enters the decoded view. `off`: open the decoded view only by explicit command.

```json
{
  "default": "needsTranscode",
  "enum": [
    "needsTranscode",
    "always",
    "off"
  ],
  "type": "string"
}
```

### `paradoxcode.localisation.preferredLanguages`

Localisation language preference. The first entry is the target language: hover previews, mission titles, and localisation search show only its texts, and the Vanilla cache retains only it (english by default). English remains the navigation fallback; later entries are ignored.

```json
{
  "default": [],
  "items": {
    "pattern": "^[A-Za-z0-9_]+$",
    "type": "string"
  },
  "type": "array"
}
```

### `paradoxcode.localisation.transparentEncoding`

Experimental EU4dll transparent encoding and `pdcloc://` decoded views. Disabled by default. When enabled, eligible files can open in the decoded view and saves encode quoted strings automatically (scoped form). Search and navigation have known limitations. When disabled, use the editor-title Decode File / Encode File buttons for manual disk conversion with a `.pre-transcode.bak` backup. Reload the VS Code window after changing this setting.

```json
{
  "default": false,
  "tags": [
    "experimental"
  ],
  "type": "boolean"
}
```

### `paradoxcode.localisation.transparentScriptGlobs`

Workspace-relative script glob patterns for the manual Decode File / Encode File buttons and experimental decoded views (paratranz `latin1eu4` profile). Localisation YML files qualify independently. Default: `**/*.txt`; an empty list limits conversion to localisation YML.

```json
{
  "default": [
    "**/*.txt"
  ],
  "items": {
    "pattern": "^[^\\].*$",
    "type": "string"
  },
  "type": "array"
}
```

### `paradoxcode.modDirectory`

Directory of the project. Defaults to the workspace root.

```json
{
  "default": "",
  "type": "string"
}
```

### `paradoxcode.performance.profile`

Bounded workspace scanning profile controlling parser concurrency.

```json
{
  "default": "balanced",
  "enum": [
    "balanced",
    "conservative",
    "fast"
  ],
  "enumDescriptions": [
    "Use the default bounded concurrency for ordinary workspaces.",
    "Use fewer workers to reduce CPU and memory pressure.",
    "Use the maximum bounded worker count for faster scans."
  ],
  "type": "string"
}
```

### `paradoxcode.preview.chineseFontMod`

Path to the mod folder that provides the Chinese bitmap font (`gfx/fonts/zh-hans-16.fnt`). Leave empty to auto-discover the newest matching Steam Workshop mod of the located game installation.

```json
{
  "default": "",
  "type": "string"
}
```

### `paradoxcode.preview.gameFonts`

Render mission titles with the game's bitmap fonts (vanilla `vic_18`; the Chinese font from a located Chinese localisation mod) instead of the system font.

```json
{
  "default": true,
  "type": "boolean"
}
```

### `paradoxcode.preview.refreshMode`

Controls when the Mission Preview requests fresh layout data.

```json
{
  "default": "always",
  "enum": [
    "always",
    "onSave",
    "manual"
  ],
  "enumDescriptions": [
    "Refresh after edits with a short debounce.",
    "Refresh when the active mission document is saved.",
    "Refresh only when the preview refresh command or button is used."
  ],
  "type": "string"
}
```

### `paradoxcode.preview.showDiagnostics`

Show diagnostic counts, badges, colors, and entries in the preview.

```json
{
  "default": true,
  "type": "boolean"
}
```

### `paradoxcode.preview.showExternalPrerequisites`

Show prerequisite missions that live outside the current mission file.

```json
{
  "default": true,
  "type": "boolean"
}
```

### `paradoxcode.preview.showTextures`

Use EU4 frame and icon textures when available in Mission Preview.

```json
{
  "default": true,
  "type": "boolean"
}
```

### `paradoxcode.preview.zoomSensitivity`

Mission Preview wheel zoom multiplier.

```json
{
  "default": 1,
  "maximum": 2,
  "minimum": 0.5,
  "type": "number"
}
```

### `paradoxcode.server.installPolicy`

Controls whether the matching pdc release may be downloaded when no server is found.

```json
{
  "default": "auto",
  "enum": [
    "auto",
    "prompt",
    "never"
  ],
  "enumDescriptions": [
    "Download and checksum-verify the matching server automatically.",
    "Ask before downloading; offer install, binary selection, or output actions.",
    "Never download automatically; use serverPath, the cache, or PATH."
  ],
  "type": "string"
}
```

### `paradoxcode.serverInstallDirectory`

Advanced: directory for checksum-verified pdc downloads. Empty uses VS Code global storage.

```json
{
  "default": "",
  "scope": "machine",
  "type": "string"
}
```

### `paradoxcode.serverPath`

Path to the pdc server binary. When empty, ParadoxCode checks the downloaded cache and PATH.

```json
{
  "default": "",
  "type": "string"
}
```

### `paradoxcode.vanilla.mode`

Controls how Vanilla symbols are sourced for this workspace.

```json
{
  "default": "auto",
  "enum": [
    "auto",
    "cacheOnly",
    "disabled"
  ],
  "enumDescriptions": [
    "Use configured caches and the normal automatic discovery/build path.",
    "Use an available cache but do not discover or build a new cache automatically.",
    "Do not load or build Vanilla symbols for this workspace."
  ],
  "type": "string"
}
```

### `paradoxcode.vanillaIndexCache`

Path to a persistent Vanilla index cache (.pdcindex). Loaded without a full rescan; refresh it with the **ParadoxCode: Update Index Caches** command.

```json
{
  "default": "",
  "type": "string"
}
```

### `paradoxcode.workspaceWideDiagnostics`

Publish diagnostics for closed files in the project during workspace validation and refreshes. Disabled by default; files opened in the editor are always validated.

```json
{
  "default": false,
  "type": "boolean"
}
```

## Commands

| Command | Title |
| --- | --- |
| `paradoxcode.showMissionPreview` | Open Mission Tree Preview to the Side |
| `paradoxcode.refreshMissionPreview` | Refresh Mission Tree Preview |
| `paradoxcode.openMissionIconPicker` | Open Mission Icon Picker |
| `paradoxcode.installServer` | Install or Update the ParadoxCode Server |
| `paradoxcode.selectServer` | Select ParadoxCode Server Binary |
| `paradoxcode.selectGameDirectory` | Choose EU4 Installation / Vanilla Data |
| `paradoxcode.addDependency` | Add ParadoxCode Dependency |
| `paradoxcode.removeDependency` | Remove ParadoxCode Dependency |
| `paradoxcode.openDependencySettings` | Open ParadoxCode Dependency Settings |
| `paradoxcode.updateIndexCaches` | Update Index Caches |
| `paradoxcode.reloadServer` | Reload ParadoxCode Language Server |
| `paradoxcode.exportDiagnostics` | Export Workspace Diagnostics |
| `paradoxcode.formatWorkspace` | Format Workspace Scripts |
| `paradoxcode.openOutput` | Open ParadoxCode Output |
| `paradoxcode.toggleDebug` | Toggle Debug Mode |
| `paradoxcode.openDebugOutput` | Open ParadoxCode Debug Output |
| `paradoxcode.refreshLoadedFiles` | Refresh Loaded Files |
| `paradoxcode.localisation.openDecoded` | Open in Decoded (Chinese) View (Experimental) |
| `paradoxcode.localisation.revealOriginal` | Open the Raw Transcoded File |
| `paradoxcode.localisation.peekOriginal` | Peek at the Raw Transcoded Form |
| `paradoxcode.localisation.endPeek` | Back to the Decoded View |
| `paradoxcode.localisation.encodeFile` | Encode Quoted Text (EU4dll Escape Form) |
| `paradoxcode.localisation.decodeFile` | Decode Quoted Text (Readable Text) |
| `paradoxcode.openSearch` | Search |
