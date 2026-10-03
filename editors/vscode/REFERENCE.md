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

When an eligible file opens through its decoded `pdcloc://` twin. `needsTranscode` (default): only files that actually participate in transcoding — escaped or damaged forms, or plain text whose quoted CJK a save would encode — while transcode fixed points stay on their normal `file://` view so search, diff, and git keep working; a fixed point that later gains quoted CJK is promoted automatically. `always`: every eligible file opens in the decoded view whatever its bytes look like. `off`: never automatic — open the decoded view by hand.

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

Master switch for the transparent EU4dll pipeline. On: eligible files open in the decoded view and saves escape-encode quoted strings automatically (scoped form). Off: nothing automatic — the manual Encode File / Decode File commands become available.

```json
{
  "default": true,
  "type": "boolean"
}
```

### `paradoxcode.localisation.transparentScriptGlobs`

Workspace-relative glob patterns of script files eligible for the transparent decoded view (paratranz `latin1eu4` profile). Eligible files whose bytes participate in transcoding open in the decoded view — readable text passes through, escaped strings decode — while transcode fixed points keep their normal file view (see `paradoxcode.localisation.autoOpen`). Default: `**/*.txt`.

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
| `paradoxcode.localisation.openDecoded` | Open in Decoded (Chinese) View |
| `paradoxcode.localisation.revealOriginal` | Open the Raw Transcoded File |
| `paradoxcode.localisation.peekOriginal` | Peek at the Raw Transcoded Form |
| `paradoxcode.localisation.endPeek` | Back to the Decoded View |
| `paradoxcode.localisation.encodeFile` | Encode File (EU4dll Escape Form) |
| `paradoxcode.localisation.decodeFile` | Decode File (Readable Text) |

## Agent tools

The MCP adapter uses this same manifest. Input schemas and bounds below belong to the tool definitions.

### `paradoxcode_workspace`

Summarise the EU4 workspace: game identity, embedded rule hash, source roots (Vanilla, dependency mods, project mod), file counts by zone, and the last scan outcome. Call it first when orienting in a workspace to learn what is indexed and where the project mod sits.

Prompt reference: `#paradoxWorkspace`.

```json
{
  "properties": {},
  "type": "object"
}
```

### `paradoxcode_search`

Search script-zone symbols (events, decisions, tags, missions, scripted triggers/effects) by name substring across the project mod, dependency mods, and Vanilla. Localisation keys are excluded (use the loc tools for those). Results are limited to 100 symbols. Use it before drafting to discover what the game or existing mods already define.

Prompt reference: `#paradoxSearch`.

```json
{
  "properties": {
    "limit": {
      "description": "Maximum entries to return (1-100, default 20)",
      "type": "number"
    },
    "query": {
      "description": "Symbol name substring (at least 2 characters)",
      "type": "string"
    }
  },
  "required": [
    "query"
  ],
  "type": "object"
}
```

### `paradoxcode_context`

Explain the rule-driven semantics at one position of an indexed EU4 script file (project, dependency, or Vanilla): what the key accepts, which scopes allow it, and the localisation preview when it resolves one. Input uses a 1-based line and 0-based UTF-16 character column. Script files only; for localisation keys use the loc tools.

Prompt reference: `#paradoxContext`.

```json
{
  "properties": {
    "character": {
      "description": "0-based UTF-16 character column (default 0)",
      "type": "number"
    },
    "line": {
      "description": "1-based line number",
      "type": "number"
    },
    "path": {
      "description": "Absolute path, file: URI, or workspace-relative path of an indexed script file",
      "type": "string"
    }
  },
  "required": [
    "path",
    "line"
  ],
  "type": "object"
}
```

### `paradoxcode_diagnostics`

Diagnostics for the project script files on disk (Vanilla and dependency mods excluded), with stable codes, 1-based line numbers, and messages. Pass files (logical paths) to focus on specific files, or omit to page through the whole workspace (16 files per page, up to 128). Localisation files are excluded. Use it to ask what is currently broken in the mod.

Prompt reference: `#paradoxDiagnostics`.

```json
{
  "properties": {
    "files": {
      "description": "Logical paths to focus on, such as events/my_event.txt",
      "items": {
        "type": "string"
      },
      "type": "array"
    },
    "limit": {
      "description": "Maximum files per page (1-128, default 16)",
      "type": "number"
    },
    "offset": {
      "description": "Zero-based page offset from the full sorted file list",
      "type": "number"
    }
  },
  "type": "object"
}
```

### `paradoxcode_references`

Find references to the symbol at one position of a script file (position-based; the position must point at a symbol the workspace has indexed). Input uses a 1-based line and 0-based UTF-16 character column. For lookup by name alone use paradoxcode_symbol_references.

Prompt reference: `#paradoxReferences`.

```json
{
  "properties": {
    "character": {
      "description": "0-based UTF-16 character column (default 0)",
      "type": "number"
    },
    "line": {
      "description": "1-based line number",
      "type": "number"
    },
    "path": {
      "description": "Absolute path, file: URI, or workspace-relative path of a script file",
      "type": "string"
    }
  },
  "required": [
    "path",
    "line"
  ],
  "type": "object"
}
```

### `paradoxcode_symbol_references`

Find the definition and references of a script-zone symbol addressed by name, without a cursor position (event ids, decisions, scripted effects/triggers, tags). Optional kind (for example event or scripted_effect) disambiguates names defined under several kinds; an ambiguous answer lists the candidate kinds to retry with. Results are limited to 100 references. Use it to measure the impact of editing or renaming a definition.

Prompt reference: `#paradoxSymbolReferences`.

```json
{
  "properties": {
    "kind": {
      "description": "Optional symbol kind used to disambiguate, for example event",
      "type": "string"
    },
    "limit": {
      "description": "Maximum references to return (1-100, default 20)",
      "type": "number"
    },
    "name": {
      "description": "Symbol name, for example flavor_kni.1 or my_scripted_effect",
      "type": "string"
    }
  },
  "required": [
    "name"
  ],
  "type": "object"
}
```

### `paradoxcode_rules`

Search the embedded first-party EU4 semantic rule database. Filters (case-insensitive, at least one required): context (exact or prefix, for example trigger, effect, type:event), key (substring of the rule key, for example add_army_tradition), scope (substring of an allowed scope; rules valid in any scope match every scope query). Entries document the expected shape, allowed scopes, and documentation text, and results are limited to 50 rules. Use it to learn which keys are valid, what they accept, and in which scopes they work before writing them.

Prompt reference: `#paradoxRules`.

```json
{
  "properties": {
    "context": {
      "description": "Semantic root such as trigger, effect, or type:event",
      "type": "string"
    },
    "key": {
      "description": "Substring of the rule key",
      "type": "string"
    },
    "limit": {
      "description": "Maximum entries to return (1-50, default 20)",
      "type": "number"
    },
    "scope": {
      "description": "Substring of an allowed scope, for example country",
      "type": "string"
    }
  },
  "type": "object"
}
```

### `paradoxcode_validate_text`

Validate Paradox EU4 script text (1-16 files) against the embedded EU4 rules and the indexed workspace/Vanilla data without writing anything to disk. Pass each file as {path, text} where path is the mod-relative logical path (for example "events/my_event.txt"). Returns per-file diagnostics with stable codes (UnknownKey, InvalidValue, WrongScope, UnknownLocalisationKey, ...), 1-based line numbers, and messages. Use it to verify drafted mod files before or after writing them.

Prompt reference: `#paradoxValidate`.

```json
{
  "properties": {
    "files": {
      "items": {
        "properties": {
          "path": {
            "description": "Logical mod path such as events/my_event.txt",
            "type": "string"
          },
          "text": {
            "description": "Full file content to validate",
            "type": "string"
          }
        },
        "required": [
          "path",
          "text"
        ],
        "type": "object"
      },
      "maxItems": 16,
      "minItems": 1,
      "type": "array"
    }
  },
  "required": [
    "files"
  ],
  "type": "object"
}
```

### `paradoxcode_loc_get`

Address one EU4 localisation key exactly (case-insensitive): returns the winning definition with its value, language, and file, or nothing. Never truncated. Use it whenever the key is known, for example an event title key from a script file.

Prompt reference: `#paradoxLocGet`.

```json
{
  "properties": {
    "key": {
      "description": "Exact localisation key, for example my_mod.1.t",
      "type": "string"
    }
  },
  "required": [
    "key"
  ],
  "type": "object"
}
```

### `paradoxcode_loc_search`

Discover EU4 localisation entries by displayed-value substring, case-insensitive, across the project mod, dependencies, and Vanilla. Returns the winning definition per key with its value, language, and file, limited to 50 entries. Use it to reverse-search visible text to its key before referencing it from events or decisions.

Prompt reference: `#paradoxLocSearch`.

```json
{
  "properties": {
    "limit": {
      "description": "Maximum entries to return (1-50, default 20)",
      "type": "number"
    },
    "text": {
      "description": "Substring of the displayed value",
      "type": "string"
    }
  },
  "required": [
    "text"
  ],
  "type": "object"
}
```

### `paradoxcode_loc_list`

Enumerate the EU4 localisation key family under an anchored key prefix (for example "flavor_kni.1." lists the .t, .d, and option keys of that event). Returns the winning definition per key with value, language, and file, limited to 50 entries. Use it to see which keys a definition site already provides or which are missing.

Prompt reference: `#paradoxLocList`.

```json
{
  "properties": {
    "keyPrefix": {
      "description": "Anchored key prefix, for example my_event.1.",
      "type": "string"
    },
    "limit": {
      "description": "Maximum entries to return (1-50, default 20)",
      "type": "number"
    }
  },
  "required": [
    "keyPrefix"
  ],
  "type": "object"
}
```
