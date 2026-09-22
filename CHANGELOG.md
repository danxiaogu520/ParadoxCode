# Changelog

All notable changes to ParadoxCode are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- The script formatter now canonicalizes asset-path separators: in scalar values whose
  spelling ends with a known asset extension (`.dds`, `.tga`, `.mesh`, fonts, sounds, …),
  every run of `\`, `\\`, or `//` separators collapses to single forward slashes, quoted or
  bare alike. Vanilla ships the same texture with all four spellings and the engine accepts
  every one of them, so this is a canonicalization on par with keyword casing, validated by
  the formatter's existing re-parse, token-equivalence, and idempotence gates. Values without
  a known extension, values containing whitespace, escaped quotes, or empty/numeric strings
  keep their spelling untouched.

- `.gfx` completion now mirrors the decisions scaffold end to end. An empty `interface/*.gfx`
  file completes its three wrapper blocks (`spriteTypes`, `bitmapfonts`, `objectTypes`) with
  block skeletons, and the gap inside a wrapper completes the fixed instance vocabulary
  (`spriteType` and its five sibling kinds, `cursor_offset`, `bitmapfont`, and the `objectTypes`
  kinds) through new `wrapper:{type}` rules — free-form wrappers such as `country_decisions`
  keep their silence. The `object` type now recognises `arrowType`, `tradeRouteType`,
  `pdxparticle`, `PieChartType`, `LineChartType`, and `animatedmaptext` as instances (their
  bodies previously had no completion or validation), and `animatedmaptext`'s `textblock`
  gained named rows for `text`, `color`, `font` (bitmap-font member completion), `position`,
  and `format`.

- `texturefile`-style values complete as a directory browse instead of a flat 200-entry
  alphabetical head: an empty prefix lists the top-level asset directories, each level lists
  its subdirectories (new `Folder` completion kind) plus the files directly inside it, and the
  200-file cap now applies per directory rather than to the whole catalog (~10k entries on a
  vanilla workspace).

- The texture catalog now harvests DLC archives record-only: each `dlc/<pack>/<id>.zip`'s
  central directory is read for member names (no extraction, no decompression — a hand-rolled
  bounded parser that yields nothing on zip64, truncated, or corrupt archives), catalog-image
  members join the same normalized namespace and the directory browse as if they were files,
  and files shipped loose on disk keep priority. Hover provenance for a packed asset shows
  both the archive path and the member inside it (`…/dlc128.zip :: gfx/event_pictures/….dds`);
  hover previews of packed assets degrade to the no-preview state because only names are
  recorded. On the maintainer's vanilla install this resolves every previously unreachable
  DLC-packed texture reference (King of Kings, Winds of Change) and grows the catalog by
  ~2,200 entries.

### Changed

- The eleven `paradoxcode_` language-model tools now declare `"when": "paradoxcodeServerRunning"`,
  so VS Code agent mode only lists them in windows where the ParadoxCode extension has actually
  activated and its language server is running. Previously they appeared in the tool picker of
  every window (including non-EU4 workspaces), where calling them could only fail. The MCP mirror
  is unaffected: it reads names, descriptions, and schemas from the same manifest and ignores the
  `when` clause.
- Every tool now opts into prompt references (`canBeReferencedInPrompt: true` with unique
  `toolReferenceName`s such as `paradoxSearch` or `paradoxValidate`). Current VS Code tool
  pickers only list prompt-referenceable extension tools, so without the flag the tools could
  never be enabled by the user: agent sessions rejected every call with "Tool … is currently
  disabled by the user" while the Configure Tools dialog showed no way to turn them on.

### Removed

- The `@paradox` chat participant (`paradoxcode.modding`) and its `/validate`, `/symbols`,
  `/rules`, `/loc`, `/hover` commands. Its private agent loop masked the agent-mode tool
  enablement gap above; with the tools directly listed, referenceable, and enableable in agent
  sessions, the participant duplicated that surface. Conversational modding now goes through the
  normal agent chat (plus `#paradox…` references), and deterministic checks remain available via
  the editor commands, diagnostics, and the MCP server.

### Fixed

- Saving a `paradoxcode.*` setting no longer surfaces a spurious "Sending notification
  workspace/didChangeConfiguration failed / Starting server failed" error. The language
  client's `synchronize.configurationSection` auto-push and the extension's
  server-setting restart both reacted to the same settings save; the push passed the
  client's running-state check, then watched the restart's `stop()` tear the connection
  down mid-flight. The push was also dead weight on its own terms — it delivers the
  settings wrapped in a `paradoxcode` namespace that the server's flat-key configuration
  handler never reads, and every server-consumed setting triggers the restart that
  re-sends fresh `initializationOptions` anyway. The synchronize block is removed; the
  server keeps its `workspace/didChangeConfiguration` handler for raw LSP clients.

- Position-bound agent tools answered "document is not open" for exactly the files they are meant
  to describe. `paradoxcode_context` (hover) and `paradoxcode_references` resolve paths to
  `file://` URIs, but the server only served documents an editor had opened under that exact URI:
  the MCP server's own pdc instance never receives `didOpen` (both tools failed for every file),
  and in the extension transparent encoding syncs the `pdcloc://` decoded twin instead of the
  `file://` URI, so the tools failed for the very files being edited. The server now lazily stages
  the scanned disk text for snapshot requests addressing known workspace files (unknown URIs keep
  the error), and the extension's tools prefer an open document's URI — the decoded twin carries
  the live, possibly unsaved text — before falling back to the on-disk URI.

- Mission Tree Preview failed with "The mission file must live inside the workspace root." for any
  mission file opened through its `pdcloc://` decoded view (transparent encoding, on by default
  since 0.4.1, takes over every `*.txt` tab): the preview located the workspace folder with a
  scheme-blind URI containment check that never matches the `pdcloc` twin. The logical path is now
  computed against the backing `file://` URI. The same unwrapping now also keeps
  `diagnosticIgnoreFiles` patterns matching diagnostics published on decoded views (they previously
  fell back to the full path and silently never matched).

## [0.4.1] - 2026-09-22

### Added

- Four new agent tools over the running language server, bringing the read-only surface to
  eleven `paradoxcode_` tools split into two zones that never cross. Script zone:
  `paradoxcode_workspace` (game id, rule hash, workspace roots, and per-zone file counts via
  the new parameterless `pdc/workspaceSummary` request), `paradoxcode_diagnostics` (workspace
  diagnostics through `pdc/workspaceDiagnostics`, now filterable by `parser: "script"` /
  `"localisation"` and by logical `files` paths), `paradoxcode_symbol_references` (name-driven
  reference lookup without a position via the new `pdc/symbolReferences`; ambiguous or unknown
  names return the candidate kinds instead of a guess), and `paradoxcode_search` (script symbol
  discovery via the new `pdc/symbolSearch`, prefix > substring > fuzzy scoring, capped at 100
  entries, localisation definitions excluded server-side). Localisation zone:
  `paradoxcode_loc_get` (exact key lookup, 0 or 1 results, never truncated),
  `paradoxcode_loc_search` (fuzzy reverse search by displayed text, default 20 / max 50 hits),
  and `paradoxcode_loc_list` (bounded prefix-family listing), all three riding
  `pdc/localisationSearch` with a new `keyMatch` parameter (`exact` / `prefix` / `substring`).
- `paradoxcode_context` and `paradoxcode_references`: script-zone hover semantics and
  position-based references that reject `localisation/` paths with a pointer to the matching
  localisation tool instead of silently crossing zones.

- Momentary raw peek for the decoded localisation view. The editor-title eye on a `pdcloc://` tab
  flips it in place to the on-disk escaped form; Esc (bound to
  `paradoxcode.localisation.endPeek` while peeking), the eye on the raw view, or moving focus to
  another editor group flips it back. The cursor line survives every flip because escape triples
  never contain newline bytes. Unsaved raw edits keep the peek open (it degrades to the pinned
  mode), and `paradoxcode.localisation.revealOriginal` — now on the status bar and command
  palette — remains the pinned variant.
- Scoped transcoding: escape triples now land only inside quoted strings, so comments and code
  stay readable UTF-8 on disk while the game-side transcoder reads the strings exactly as with
  whole-file encoding. The Rust crate gained a byte-level quote/comment scanner plus
  scoped form dispatch (`plain` / `whole` / `scoped` / `damaged`), scoped encode/decode with
  in-span iron-rule ② refusals, and self-healing for partially escaped files (a save converges
  readable and escaped strings alike); the TypeScript twin mirrors all of it, pinned by 9,760
  new differential vectors (84,309 total) and a golden-corpus property — a master whose CJK
  lives entirely inside strings scope-encodes byte-identical to the whole-file release.
- Manual one-shot transcoding commands for when the transparent pipeline is off:
  **Encode File (EU4dll Escape Form)** rewrites a readable eligible file in the scoped escaped
  form and **Decode File (Readable Text)** rewrites an escaped one (legacy whole-file or scoped)
  as readable UTF-8. Both write a `.pre-transcode.bak` backup next to the file first, refuse
  when an editor holds unsaved changes for it, and appear in the palette and Explorer context
  menu only while `paradoxcode.localisation.transparentEncoding` is `false`.
- Mission icon picker: a webview panel (command palette, the editor-title button on mission
  files, and the editor context menu) whose mission tab lists every sprite named `mission…` or
  whose texture lives under `gfx/interface/missions/`, with a second tab browsing all sprites.
  Each cell shows the first frame, the sprite name, a vanilla/mod source badge, and the frame
  count; pixels load lazily in batches. Clicking writes the value into the `icon = …` span in
  place (or inserts at the cursor) and closes the panel. Completion docs for sprite values
  (mission `icon`, event `picture`) embed the decoded first frame via `completionItem/resolve`,
  gated by `paradoxcode.completion.iconPreview` (default on).
- Bilingual extension UI (English + 简体中文): all user-visible strings go through
  `vscode.l10n.t`, and the webviews receive a localised dictionary with an English fallback
  table baked into the media scripts. Terminology follows `docs/glossary.md` (Vanilla → 原版,
  Mod → 模组, sprite → 图像), and a new i18n contract gate enforces bundle key parity and
  webview table sync. LLM-facing strings and server diagnostics stay English.

### Changed

- The mission tree preview now pins to the most recently focused mission file: moving focus to a
  non-mission file keeps the rendered tree on screen instead of clearing to the empty-state hint,
  and focusing another mission file retargets the preview as before. Closing the pinned file's tab
  returns the panel to the empty state. A filename badge in the preview toolbar names the document
  the tree was computed from (hover for the full path), since with pinning it can differ from the
  active editor.
- The five 0.4.0 `vscode.lm` tools were renamed to the vendor-prefixed `paradoxcode_` scheme
  and reorganised: `paradoxcode-validate-text` → `paradoxcode_validate_text`,
  `-search-symbols` → `paradoxcode_search`, `-search-rules` → `paradoxcode_rules`,
  `-search-localisation` → split into `paradoxcode_loc_get` / `paradoxcode_loc_search` /
  `paradoxcode_loc_list`, `-hover-info` → `paradoxcode_context`. The same renames apply to the
  stdio MCP server, whose manifest stays mirrored from `languageModelTools`. The `@paradox`
  system prompt and the MCP instructions were restructured into gopls-style read/edit
  workflows with an explicit zone-discipline section, and `/loc` now routes `key=` / bare
  single words to exact lookup, `text=` / bare multi-word queries to reverse search, and the
  new `prefix=` token to prefix listing.
- Symbol search result caps are now declared per zone in each tool description (100 script
  symbols, 20 default / 50 max localisation entries).
- The add-dependency loading-strategy quick pick now defaults to the **persistent index cache**
  (listed first and marked recommended, with pros/cons in each option's detail line) instead of
  live scanning. A new **ParadoxCode: Update Index Caches** command
  (`paradoxcode.updateIndexCaches`) notifies and restarts the server so persistent dependency
  caches and the Vanilla cache refresh incrementally at re-initialize — the manual pickup path,
  since cached dependencies are not file-watched mid-session. The `paradoxcode.dependencies`
  and `paradoxcode.vanillaIndexCache` setting descriptions now explain both loading modes.
- When the participant's 12-round tool budget is exhausted, the loop now forces a final
  no-tools request that answers from the results already gathered (and states what could not
  be verified) instead of ending with a static "ask me to continue" line that discarded the
  whole investigation.

- Opening an eligible transcoded file now takes over its tab instead of adding a second one: the
  decoded `pdcloc://` view is shown in the raw tab's own group and preview state and the raw tab
  is closed. Raw views opened deliberately (revealOriginal, an active peek) are immune to the
  automatic redirect until their tab closes, and invisible programmatic document opens no longer
  trigger a redirect at all — only the editor the user is actually looking at is taken over.

- `paradoxcode.localisation.transparentEncoding` is now the single master switch: on, the full
  transparent pipeline runs (decoded views, path-based takeover, save-time scoped encoding); off,
  nothing automatic happens and only the manual Encode/Decode commands remain. The separate
  `paradoxcode.localisation.autoOpenDecoded` setting is gone — its behaviour is exactly the
  master switch being on. The one-shot `Transcode Localisation File` command was absorbed into
  **Encode File (EU4dll Escape Form)**, now writing the scoped form with the same
  `.pre-transcode.bak` backup.

- Entry to the decoded view is now path-based rather than content-based: every eligible file
  (localisation yml or `transparentScriptGlobs`) opens through its `pdcloc://` twin and takes
  over the raw tab. readFile dispatches on the actual form — plain readable files pass through
  unchanged (quoted CJK is escape-encoded on the next save, announced by an info diagnostic),
  legacy whole-escaped files decode wholesale, scoped files decode their strings while comments
  stay readable, and damaged files show as-is with an error anchored at the first stray marker.
  Three consequences worth knowing: (1) a legacy whole-escaped file saved through the decoded
  view migrates to the scoped form — strings stay byte-identical, previously escaped comments
  become readable UTF-8; (2) with the default `**/*.txt` glob, every workspace `.txt` now opens
  through the decoded view, not only already-escaped ones; (3) files that land on an eligible
  path mid-session (Save As from an untitled buffer, a file moved into scope) are adopted on
  open, so typed CJK is encoded by the next save. Known limitation: a brand-new file that has
  never been saved has no path yet and keeps its plain editor until it does.
- The save gate is span-aware: pasted escape markers inside quoted strings refuse the save with
  their positions (iron rule ②), while markers in comments or code no longer block saving —
  those regions are written verbatim. Saves from the decoded view always write the scoped form.
- `LocalisationMixedEncoding` diagnostics are narrowed to actual damage: a stray escape marker
  outside every quoted string is the error (anchored at the marker itself); markers inside
  strings are repairable `LocalisationBrokenEscapeSequence` warnings; readable CJK sharing a
  string with escapes self-heals on save and is no longer flagged. Unencodable-code-point
  warnings now apply only inside strings — comment/code positions stay verbatim on save.
- The eye-icon context key `paradoxcode.transcodeEscaped` was renamed to
  `paradoxcode.transcodeEligible` and now reflects pure path eligibility (no disk read) instead
  of the classifier verdict.

## [0.4.0] - 2026-09-21

### Added

- Extension-internal agent surface: five read-only tools over the running server, a `@paradox`
  chat participant, and a stdio MCP endpoint. Two new snapshot requests expose the server's
  own knowledge to tooling without touching workspace state — `pdc/ruleSearch` (bounded
  search over the embedded first-party rule database by context / key / scope, each entry
  carrying shape, allowed and push scopes, flags, and bounded documentation) and
  `pdc/localisationSearch` (search indexed localisation definitions across vanilla,
  dependencies, and project by key or decoded value; both require a filter and report
  truncation). On top of them the extension registers five `vscode.lm` tools that ride the
  already-running language-server client — `paradoxcode-validate-text` (in-memory diagnostics
  for up to 16 unsaved files), `-search-symbols`, `-search-rules`, `-search-localisation`, and
  `-hover-info` — all read-only, every result shaped through shared output budgets, and
  operational failures returned as readable tool results the model can react to. The
  `@paradox` chat participant (`paradoxcode.modding`) runs plain prompts through a bounded
  agent loop (12 tool rounds, chat-model fallback, and a domain prompt with hard tool
  discipline — validate every draft before calling it done, look up allowed scopes instead
  of guessing) and offers deterministic `/validate` `/symbols` `/rules` `/loc` `/hover`
  commands that keep working without any chat model. The same five tools are exposed to any
  Model Context Protocol client through a dependency-free stdio server
  (`editors/vscode/scripts/mcp.mjs`, hand-written ndjson JSON-RPC): the tool manifest is
  read at runtime from the same `languageModelTools` contribution the extension registers
  from, workspace roots resolve from `--workspace` / `PDC_MCP_WORKSPACE`, the client's
  roots, or the working directory (the home directory excluded), and root changes re-boot
  the language server against the new root. The extension's engine requirement moves to
  `^1.99.0` for the language-model API.
- `lab/perf` performance lab for local development: a single-entry workflow
  (`lab/perf/perf.sh` with portable defaults and a runbook) covering baseline snapshots,
  warm/median A/B comparison with noise marking, full-corpus sweeps through the existing
  `sweep.mjs`, a Linux-native cwtools-rs control group, corpus import filtered to the
  server's scan categories, and samply/perf profiling. Scripts are tracked; run outputs,
  machine-local overrides, control binaries, and game-data corpus copies stay gitignored.

### Changed

- Startup and whole-workspace validation performance round. Vanilla and dependency cache
  loads now overlap the initial workspace scan instead of serializing behind it (deferred
  setup events preserve install ordering), cutting `pdc/ready` on a ~10k-file corpus from
  ~7.0 s to ~4.6 s and warm sweep session boot by two orders of magnitude. Workspace
  validation skips republishing identical diagnostics and consults a per-file cache keyed
  on the file's content hash, a workspace context fingerprint (rules hash, profile
  identity, language preference, source roots, a texture-catalog generation, and every
  file's effective contribution), and the diagnostics filter — a back-to-back no-change
  `validateWorkspace` reuses every file (~33 s → ~1.2 s on the ~8k-file reference corpus).
  Steady-state query paths get cheaper throughout: workspace suggestion lookups memoize
  per snapshot revision, installed-cache references load lazily by kind, cached
  localisation previews filter by preferred language at load, transient reparses of
  evicted closed files serve byte-identical trees from the persistent parse cache (~9×
  cheaper than reparsing), Project position-table entries prune at scan commit,
  cardinality occurrence counting hashes case-folded keys instead of linear-probing
  key-dense containers (cold diagnostics latency −9.7% on the interactive benchmark),
  the per-reference file id drops out of the index, and HIR kind spellings intern through
  the process pool. Diagnostics output is byte-identical across the round.
- Missing-entry Cardinality diagnostics anchor on the owning block key instead of the
  container's opening brace: `some_block = { … }` missing a required entry now squiggles
  `some_block` — where the fix belongs — and agrees with `MissingLimit` on `if` blocks;
  over-quota findings keep anchoring on the first excess entry, and the file root keeps
  the first-character anchor.
- Typed rule hovers gain localisation previews, and symbol previews cover every resolvable
  binding. Block-scope keys classified by `KeyMatcher::Type` rules — an event option's
  `…_area = { … }`, country-tag and province-id blocks — now append the workspace
  localisation preview when every typed candidate agrees on one kind (mirroring the
  typed-reference convention), instead of never attempting localisation at all. Symbol
  previews render every resolvable explicit reference, implicit same-name key, and
  binding-template row as labelled entries (a decision shows `name` and `desc` side by
  side), deduplicated and capped, instead of returning on the first strategy hit.
- The rules pipeline drops its SQLite artifact layer. First-party rules compile from the
  embedded JSON source straight into the in-memory rule set at every startup — measured
  faster than loading the old ~20 MB user cache — so `rules.pdcrules` is no longer
  materialized under the cache root, and upgrades best-effort remove artifacts left by
  earlier releases. The canonical `rule_hash` computation stays: it remains the identity
  key that decides vanilla-index and dependency-cache reuse. `bake` loses its `--output`
  flag and now only validates the source and regenerates `rules/manifest.json`;
  `schema_version` and `artifact_sha256` retire with the artifact, and the `rules`
  crate no longer depends on `rusqlite`.
- Mission hover cards drop the trigger and reward corner markers. The game
  interface (`countrymissionsview.gui`) shows those markers state-dependently,
  but the card drew them statically whenever the mission declared
  `trigger`/`effect` blocks — which modded missions almost always do — so every
  card carried both badges piled on the frame. Cards now render the classic
  frame + icon + title look only; the `pdc/hoverCard` wire stops carrying the
  `triggerMarker`/`effectMarker` chrome assets and the `hasTrigger`/`hasEffect`
  facts (protocol version stays 1 — both fields were optional in each
  direction).
- Terminology: the transcode concept drops its historical `codec` naming.
  `transcode::CODEC_VERSION` becomes `TRANSCODE_VERSION` (Rust and the TypeScript
  twin alike), the crate's core module is renamed `codec.rs`→`escape.rs`, and the
  index-cache metadata key `codec_version` becomes `transcode_version` (existing
  index caches rebuild once, silently, through the built-in invalidation path).
  The engine's cache-serialization modules (`index_cache/codec.rs`,
  `position_codec`, `template_codec`) deliberately keep their names — they are
  serialization codecs, not transcoders.
- Mission and event hover cards anchor on tokens instead of whole blocks. A
  card now renders on the definition's block-name token (`<mission_name> = {`
  in a missions file, `country_event`/`province_event` at an event block's
  head) and on references the resolver already knows: `event`-keyed values and
  fire-event call blocks serve the referenced event's window, and
  `required_missions` members serve the referenced mission's card (resolved
  through the symbol layer, cross-file included). Positions inside block
  bodies no longer card, so trigger/effect/option hovers keep the plain
  semantic pipeline, and hovering an `icon`/`picture` value now falls through
  to the more specific sprite preview instead of the whole-block card.
- Renamed the editable source layer from Current Mod to Project across the server, extension,
  scripts, and documentation: `SourceRootKind::Project`, hover-card and workspace-files
  `rootKind`/`kind` value `project`, and the `paradoxcode.completion.sourceLayers` value
  `project`. No legacy spellings are accepted: an existing Vanilla index cache rebuilds once
  from the discovered installation, and a stale `currentMod` settings entry fails
  initialization with the valid values listed.

### Removed

- The shared-project-file compatibility paths: the `projectConfig` initialization sentinel,
  the `.pdx/project.toml` workspace probe, and the packaging guards asserting their absence.
  Stale clients now receive the standard unknown-field rejection.

## [0.3.8] - 2026-09-17

### Added

- Structured hover cards over a new `pdc/hoverCard` request: the server answers with a
  versioned, pixel-free card payload (mission / sprite / texture) whose assets carry
  TextureCatalog-resolved absolute paths plus root provenance (`currentMod` / `dependency` /
  `vanilla`), extension-fallback flags, and frame counts — path resolution stays a single
  server-owned source of truth while the client only reads, decodes, and caches files.
  Hovering a mission inside a mission file composes the game-look mission card in the
  extension host: the 103×123 frame with the icon underneath, trigger and reward corner
  markers (frame strips crop to frame 0), and a §-coloured bitmap-font title (CJK-aware font
  choice and per-character wrapping, centred, two lines), degrading to frame + icon +
  markers when no font is available. Sprite and texture hovers render single-texture
  previews from the server-resolved path. Any protocol failure — old server without the
  method, timeout, malformed payload — falls back to the legacy hover paths unchanged.
- `paradoxcode.hover.missionCard` (default on) toggles the composed mission card.
- Event windows compose through the same protocol: hovering a
  `country_event`/`province_event` block in an `events/` file renders the game's
  564-wide event window — stacked background chrome (`GFX_event_bg_top/middle`
  and the `bottom_S`/`_M`/`_L` pieces sized by option count), the picture
  banner, §-coloured title and wrapped description, and one button row per
  option. Event pictures resolve through the sprite index verbatim (vanilla
  `eventpictures.gfx` names carry no `GFX_` prefix) with a prefixed fallback
  for mods, and omitted title/desc keys default to `<id>.t`/`<id>.d` the way
  the engine does. Controlled by `paradoxcode.hover.eventCard` (default on).
  Event text follows the game's colours — white shadowed title and option
  labels, black description — with § colours applied as authored; the whole
  description wraps (Latin at word boundaries, CJK per character) and the
  window grows with the tiled middle chrome to fit any length instead of
  clipping at the engine's 128px description box. Mission titles fold beyond
  two lines with a trailing `...` marker.

### Fixed

- Hover texture previews never appeared since the feature shipped: VS Code's `Hover` class
  always wraps `contents` in an array, and the hover middleware's markdown extraction
  returned early on arrays, silently dropping every sprite hover augmentation. The
  extraction now accepts both the wrapped and the bare shape.
- Hover images larger than VS Code's 100,000-character hover-markdown cap rendered as
  literal markdown source: the renderer truncates oversized strings mid-data-URL, which
  beheads the image syntax. Inline images beyond a 90k budget (composed cards and large
  textures) now spill into a bounded temp-file cache referenced via `file:///` URIs, and
  small images stay inline.
- Latin text never wrapped in composed cards or the mission tree preview: the shared
  tokenizer accumulated whole space-separated runs into single unbreakable tokens, so
  only CJK content wrapped at all. Whitespace now opens wrap tokens, so Latin wraps at
  word boundaries everywhere the pipeline renders text.
- The mission preview no longer loses every game texture when its panel is closed and reopened in
  the same session. Sprite delivery now tracks which names each panel has received, so a rebuilt
  webview — and any sprite a later refresh newly references after being decoded for a previous
  panel — is resent instead of being suppressed by the extension host's warm cache. The
  client-side texture resolver also accepts the engine's `.tga`/`.dds` extension drift and
  case-insensitive path matching, so a sprite whose file ships under another spelling or casing
  renders instead of silently falling back to the schematic.
- The mission tree preview only previews mission files, and it no longer retargets behind the
  user's back. The refresh gate is a single `missions/` path pattern (previously any EU4 document
  or around 140 path patterns qualified, so events, decisions, and common files rendered pseudo
  trees from their top-level blocks); focusing a non-mission file shows an empty state explaining
  the requirement. When focus leaves text editors — most commonly clicking the preview canvas to
  pan or zoom — the refresh fallback now resolves the document behind the last pushed preview
  instead of the first open mission-like document in open order, which used to flip the panel to
  another file and reset the viewport.

### Changed

- Scripted-effect/trigger completion now follows the invocation-form rule the game actually
  enforces: a definition is scalar (`= yes` for effects, a boolean for triggers) if and only
  if its body declares no `$PARAM$` at all; every parameterized definition completes as a
  parameter block. Previously any definition whose parameters were all "optional" — runtime
  `if`/`else`-branch-local uses, `[[chunk]]`-only uses, or same-named relays — completed as
  `= yes`, which vanilla itself never writes (all of its parameterized calls are blocks, and
  47 definitions fell into that gap). The block skeleton prefills one tabstop per
  effectively-required parameter (the final one doubles as the cursor position, and no
  trailing placeholder line remains); parameters whose every use sits inside a
  `[[conditional]]` chunk or is forwarded into a nested dynamic call stay optional and get no
  tabstop. Hover's callable signature and per-parameter presence lines now group by the same
  activation-scoped partition, so hover, completion, and diagnostics agree. Scalar `= yes`
  calls of definitions with effectively-required parameters gain the missing-parameter
  diagnostic; block calls keep the existing branch-local leniency unchanged, and purely
  chunk-parameterized definitions stay legally scalar.
- First-party EU4 modifier catalog synced with a full wiki audit: the frozen 28-key
  exported-modifier enum is replaced by the already-indexed `<faction>_influence` template family
  (`pr_buccaneers_influence` validates again), the per-estate loyalty-equilibrium spelling is
  retired in favour of `<estate>_loyalty_modifier` (only the all-estate form keeps the
  equilibrium name), the long-removed `reduced_native_attacks` stops validating, and
  `secondary_religion` moves from the numeric modifier context to the event-modifier flag layer
  beside `religion` — a yes-flag that drops the modifier on syncretic-religion change.
- Ancestor personalities follow the government-attributes pattern: the frozen import enum is
  replaced by a workspace `ancestor_personality` type harvested from
  `common/ancestor_personalities`, so mod-added personalities validate and complete and the nine
  short keys the frozen enum had never gained are accepted. The
  `remove_{ruler,queen,heir}_personality` operands resolve the full-name type instead of a dead
  never-interpolated literal, and seven unreferenced legacy enums with their symbol kinds retire.
- Repository validation now has explicit lifecycle ownership: targeted local checks provide
  developer feedback, the remote `Conclusion` check is the merge authority, scheduled security
  and performance workflows are audits, and the tag workflow builds and verifies only
  redistributable repository-owned release assets.
- The full Vanilla sweep is now a local-only development audit. It requires an explicit server
  binary, rejects a stale binary whose embedded rules hash differs from the checkout, records the
  binary checksum and dirty-worktree state, and keeps all game-derived reports in the ignored
  `performance-results/` directory. Licensed game data, diagnostic excerpts, fingerprints, and
  sweep reports are no longer uploaded to Actions or attached to Releases.
- Local gate groups are deterministic and purpose-specific; dependency vulnerability scans remain
  in the scheduled remote security workflow rather than blocking unrelated local or pull-request
  work when an external advisory database changes.

### Removed

- The self-hosted Vanilla sweep runner, its host guard and recovery runbook, the checked-in
  diagnostics fingerprint baseline/history, and the release workflow's sweep dependency. Release
  publication no longer depends on a maintainer workstation or a licensed EU4 installation.

## [0.3.7] - 2026-09-16

### Added

- `.gfx` files get the same semantic depth as `.txt` files. All six GUI sprite block kinds
  (`spriteType`, `textSpriteType`, `progressbartype`, `corneredTileSpriteType`,
  `maskedShieldType`, `frameAnimatedSpriteType`) now carry key-level rules — per-key hover
  documentation, key and value completion, unknown-key diagnostics, and validation for
  `loadType`, `noOfFrames`, booleans, and the `animation`/`color`/`size` sub-blocks — on top of
  full symbolization (the five previously invisible kinds join `spriteType` in the shared
  engine namespace, so mission icons and event pictures resolve across all six).
  `objectTypes`/`pdxmesh` blocks symbolize as a new `object` kind and validate their mesh keys;
  `bitmapfonts`/`bitmapfont` blocks symbolize as `bitmap_font` (including vanilla's numbered
  `2-bitmapfonts` root key in `chatfonts.gfx`) and validate the font keys. The legacy
  animated-overlay keys `animationtime` and `animationtype` join the sprite key set.
- Texture reference checking with a new `UnknownTexturePath` diagnostic (error severity,
  ignorable via `paradoxcode.diagnosticIgnoreCodes`). The server builds a workspace texture
  catalog from every source root's `gfx/` and `tutorial/` trees plus DLC pack directories and
  resolves each `texturefile`-family value the way the engine does: separator and case
  normalization, mod-over-game root priority, pack-relative DLC lookup, the `.tga`↔`.dds`
  extension drift fallback, and a direct game-root probe for paths outside the harvested
  directories. Stale-but-working references stay silent; a truly dangling path gets an error
  with a same-directory did-you-mean. `texturefile` values also complete from the catalog
  (slash-aware prefix replacement from the opening quote) and hover with their resolution
  provenance (which root serves the file, and whether the extension fallback saved it); the
  VS Code hover middleware appends the decoded texture preview on top of the server's
  semantic hover instead of substituting for it. Existence checking is scoped to
  root-relative texture references it can actually adjudicate: a pdxmesh `animation`'s
  `type` (a compiled clip name such as `polearm_onehanded_attack_animation`, not a file
  path), `effectFile` (the engine resolves the compiled gfx/FX shader by name — the
  historical `.lua` spellings do not exist on disk even in vanilla), and `meshsettings`
  texture overrides (bare file names resolved relative to the mesh's own directory) are
  documented but not existence-checked. Across the full vanilla sweep the check fires only
  on genuinely missing files.

- Mission Preview renders titles with the game's own bitmap fonts and honours `§` colour codes.
  Titles blit glyph-by-glyph from the BMFont atlases the game uses — vanilla `vic_18` for English
  and the `zh-hans-16` DXT5 atlas for Chinese — with kerning, per-character CJK wrapping, and
  per-glyph colour tinting; font choice follows content, not the `l_english` headers Chinese
  replace files carry. The 14 `§` codes nest and pop like in game (`§!` restores the enclosing
  colour); every value is game ground truth from vanilla `interface/core.gfx` — the global
  `textcolors` block plus the `vic_18` bitmapfont's own G/R/Y overrides, identical in the Chinese
  font mods. Search, tooltips, and aria labels all match
  against the same `§`-stripped plain text. Two settings control it:
  `paradoxcode.preview.gameFonts` (default on) and `paradoxcode.preview.chineseFontMod` (explicit
  mod folder override; empty auto-discovers the newest matching Steam Workshop mod of the located
  game installation, so players need no extra setup).

- Debug mode: the new `paradoxcode.debug.enable` setting (default off, live without a server
  restart) quiets the default experience and unlocks full observability when needed. Off, the
  ParadoxCode channel shows only lifecycle lines plus server errors/warnings (server INFO
  messages stop printing but keep flowing, because the Vanilla walkthrough state machine parses
  them). On, a new "ParadoxCode Debug" channel receives the INFO trail, the complete LSP
  conversation (via the client's verbose protocol trace), and a new `pdc/trace` server
  notification stream exposing event-loop scheduling decisions — deferrals with their busy
  reasons, deferred replays, `$/cancelRequest` arrivals, stale worker-result drops, scan/reindex
  commits and revision races, and shutdown-drain transitions, every line timestamped and tagged
  with the request id it concerns — the exact blind spots of the
  0.3.5 CPU-burn investigation. The server learns the trace level from the initialize `trace`
  parameter and a `$/setTrace` notification (which it now handles instead of silently
  discarding); everything gates on a single string compare, so the off path is unchanged.
  `paradoxcode.toggleDebug` and `paradoxcode.openDebugOutput` commands round out the workflow,
  `paradoxcode.debug.logFile` mirrors the debug channel to an append-only file, and a headless
  `PDC_TRACE=<path>` environment variable (optional `PDC_TRACE_FULL=1` for truncated params)
  writes per-frame lines with request round-trip times from the transport's two framing
  choke points for repro-driver sessions. The decision stream now also covers the per-document
  edit-diagnostics lifecycle — the exact path the 0.3.5 single-core burn lived in and the one
  class of worker it previously could not see: every round is traced from `started` through its
  outcome (`published <file> v<n> (<k> diagnostic(s))`, `suppressed … (batch identical to last
  publish)` for the deduped-identical case, `discarded … (stale version)`, `aborted … (cancelled
  or panicked)`), and superseding a still-running round emits `superseded …; cancelled in flight`
  once (not per loop iteration). Without verbose trace none of these lines are evaluated past the
  gating string compare.
- New `pdc/formatWorkspace` command (VSCode: "ParadoxCode: Format Workspace Scripts") formats
  every Current Mod script file in one pass on a bounded worker pool, writing canonical text
  straight to disk. Each file is re-read from disk and must pass the formatter's full safety
  pipeline (no parse errors, token equivalence, idempotence) before it is rewritten; files that
  are not valid UTF-8 are skipped rather than silently re-encoded, and localisation files,
  vanilla, and dependency roots stay out of scope. The client saves all dirty editors first so
  disk is authoritative, and a summary (formatted / unchanged / skipped / failed) is reported when
  the pass completes. This is the first server feature that writes user source files: the
  watched-file pipeline and clean open-document reloads pick the rewrites up, so no host commit
  is involved and `$/cancelRequest` plus shutdown drain both stop the walk.
- Mission Tree Preview series visibility: a new **Series** panel in the preview toolbar lists every
  mission series in the file with a checkbox (plus All/None shortcuts and a visible/total count in
  its summary). Unchecking a series hides its nodes, column label, and every dependency arrow that
  starts or ends in it — the `pdc/missionPreview` arrow payload now carries the endpoint series of
  each segment (`tree` for the dependent, `from` for the prerequisite), so cross-series arrows never
  dangle into hidden space. Hidden series keep their canvas position (the gap stays, the view does
  not move), and they drop out of the mission list and the diagnostic summary. Each document
  remembers its hidden set for the lifetime of the panel, keyed by series id so edits that add,
  remove, or rename series keep the set pointing at the right ones; explicit Fit and the
  switch-document fit measure only visible content.
- Mission Tree Preview search: a search box in the preview toolbar matches missions by localised
  title or id (case-insensitive substring, capped at 50 results, each row naming its series).
  Opening a result jumps to the mission's source definition and centers the canvas view on its
  node with the hover ring; matches inside hidden series are listed dimmed and jump to source
  only. Arrow keys plus Enter navigate results, Escape clears.

### Changed

- The pdc server no longer decodes pixel data: the entire texture pipeline
  (`game::eu4::mission::texture` — DDS/TGA decode, PNG encode, gfx sprite resolution) is deleted
  from Rust and every `pdc/missionPreview` response is pure text (arrow sprites are still named
  on the payload, via `arrow_sprite_name` in `geometry`). The extension decodes the referenced
  sprites and fonts itself — new `GameAssetStore` with mtime caching — and ships them once per
  asset generation in a separate `assets` webview message, instead of re-sending every sprite as
  base64 on each keystroke.
- The Mission Preview **Series** panel now mirrors the canvas layout: series are grouped into
  `Slot N` column blocks arranged left to right (stacked series keep their vertical order inside
  a column), wrapping across a wider panel — a miniature of the file's column structure instead
  of a flat list.

- The Mission Tree Preview keeps its pan and zoom across payload refreshes: editing the mission
  file (or a manual refresh) no longer snaps the view back to the fitted default, which was the
  default `persistViewport: false` behavior refitting on every debounced update. The view now fits
  only on first load, when a different mission file becomes active (each document remembers its
  own viewport for the lifetime of the panel), and on explicit Fit (button, `F`, or double-click).
  The `paradoxcode.preview.persistViewport` setting is retired — session viewport retention is now
  unconditional and needs no configuration.
- Mission Preview interaction: pressing on a mission node and dragging now pans the view (any
  press point pans once the pointer moves more than 3 px), and a press released without movement
  is the jump-to-source gesture, removing the accidental jumps from pressing a node to start a
  drag. Keyboard `+`/`-` and the toolbar zoom buttons now zoom around the current view center
  instead of the canvas top-left corner.

### Removed

- Mission Tree Preview PNG/SVG/JSON export (toolbar buttons, handlers, and the
  `paradoxcode.preview.defaultExportDirectory` setting). The PNG export captured only the current
  viewport, the SVG export was a texture-less schematic, and the JSON export was a raw wire dump.
- The root-mission green highlight (border, fallback fill, legend entry, and mission-list marker)
  and the `isRoot` field on the `pdc/missionPreview` node payload.
- The Mission Preview legend and mission-list panels. The new search box replaces the list's
  jump-to-source with match-by-title-or-id plus canvas centering, and node border colors are
  error red / warning amber / hover blue only.

### Fixed

- Editing a document that calls scripted triggers/effects no longer sends the server into an
  unending single-core spin (the "type in a file that uses a `scripted_trigger`, CPU pins one core
  forever and no diagnostics ever publish" failure mode). The diagnostics path resolved the
  workspace-wide dynamic-rule, contract, and cycle reports through uncancellable fresh tokens and
  cached them in the documents domain, so every keystroke dropped them; a worker whose revision the
  domain had advanced past then had every insert discarded, rebuilding all 3,306 vanilla-and-mod
  definitions per property without end. Three changes land together: the dynamic reports and
  per-definition resolutions move to a new definitions cache domain that only advances when a
  document declaring dynamic definitions commits or closes (caller-file edits keep the entries),
  probes on a superseded revision degrade to an empty report instead of rebuilding work whose
  insert would be dropped, and the per-property diagnostics and completion entry points forward
  the request's cancellation token so interrupted workers stop at the next checkpoint. The
  rule-row build also resolves the contract report once per build instead of re-probing it per
  nested call. On the reproducing workspace a 50-second edit session with the old failure now
  costs 3.7 seconds of CPU total and publishes diagnostics; editing the declaring file itself
  stays busy per keystroke (each edit legitimately rebuilds the reports) and settles the moment
  typing stops. The rebuilt dynamic-definition reports also surface on the batch validation
  path: eighteen genuine `mechanic_type` typos in vanilla `common/parliament_bribes` that the
  stale-drop loop previously discarded now publish like any other InvalidValue.

## [0.3.5] - 2026-09-13

This release retires the typed-language editor surface for EU4 localisation prose: `.yml`
localisation documents stay indexed for hover and navigation but no longer receive diagnostics or
identifier completion, the decoded `pdcloc://` view opens automatically for EU4dll-transcoded
files, and only that view still reports the not-transcoded warning. Completion's dynamic-contract
inference becomes cancellable so obsolete requests stop pinning a core, and the release process is
hardened into protected atomic releases: a PR-only mainline with the stable `Conclusion` check,
immutable verified assets, a sweep of the packaged Windows server, pinned GitHub Actions, and a
host-local allowlist on the dedicated self-hosted runner.

### Changed

- Let the release sweep accept exactly one reviewed diagnostics fingerprint through a checked-in
  baseline file (`editors/vscode/scripts/sweep-baseline.json`): releases that intentionally change
  diagnostic output record the accepted fingerprint there through a pull request, and the gate
  still fails on any drift the file does not name. The published sweep summary records the
  expected fingerprint openly.
- Keep EU4 localisation documents in the workspace index for hover and navigation while returning
  no LSP diagnostics or completion items from those documents, preventing prose edits from
  triggering unrelated key lists and warnings. The server-side `LocalisationNotTranscoded`
  release-path warning is retired with it: readable CJK under `localisation/…/replace/…` must be
  transcoded deliberately, and only the decoded-view provider still attaches that code.
- Open eligible EU4dll-transcoded localisation files directly in their decoded `pdcloc://` view
  by default, while preserving automatic re-encoding on save; the redirection can be disabled
  with `paradoxcode.localisation.autoOpenDecoded`.
- Extend the decoded view to workspace script files: `paradoxcode.localisation.transparentScriptGlobs`
  defaults to `**/*.txt`, files that are not classifier-escaped (readable UTF-8, BOM included, and
  ASCII) never enter the view, and the editor and explorer entries follow a cached
  `paradoxcode.transcodeEscaped` context key that refreshes right after Transcode Localisation
  File.
- Protect the integration and release process: pull requests require the stable `Conclusion`
  check, releases verify tag provenance, and the packaged Windows server must pass the Vanilla
  sweep before a complete draft is published.
- Make self-hosted sweep configuration portable through repository variables and add governance,
  release recovery, and runner reconstruction runbooks.

### Fixed

- Lowering no longer emits a substitution reference nested inside a `[[condition] … ]` head: the
  conditional's own reference already represents that occurrence, and the overlapping pair broke
  the non-overlapping source order that `parameter_reference_at` binary-searches over (surfaced
  by the fuzz invariant smoke).
- Cancel dynamic contract inference when its triggering completion request is cancelled: the
  workspace-wide contract report previously ran under a fresh, never-cancelled token on the
  completion path, and documents without dynamic definitions now skip the report entirely.

### Security

- Publish future GitHub releases immutably, refuse asset replacement, audit production npm
  dependencies, and pin GitHub Actions dependencies to reviewed commit SHAs.
- Enforce a host-local pre-job guard on the dedicated self-hosted sweep runner: the repository,
  workflow path, event, and protected ref are independently allowlisted before any repository
  step executes on the host.

## [0.3.3] - 2026-09-12

This release restructures the repository along rust-analyzer's layering: twelve crates with the
`pdx-` prefixes gone, the language server binary renamed to `paradoxcode`, and every user-visible
contract (`paradoxcode.serverPath`, `.pdcindex`, `.pdcrules`, `pdcloc://`, `pdc/`) carrying the
new name. The shell tooling layer is replaced by `cargo tools gates`, CI is reorganized the
rust-analyzer way, whole-workspace validation no longer pegs every worker core while a document
is open, engine-parameterized modifier families validate through template key matchers, and UNC
paths work end to end.

### Changed

- **Breaking:** the editor scripts are renamed to single-word names and the diagnostic tool is
  split into importable modules: `diagnose-current-mod.mjs`→`diagnose.mjs`,
  `codec-ts-test.mjs`→`transcode.mjs`, `extension-contract.mjs`→`extension.mjs`,
  `package-contract.mjs`→`package.mjs`, `host-test.mjs`→`host.mjs`,
  `performance/lsp-e2e.mjs`→`probe.mjs`, `performance/head-to-head.mjs`→`compare.mjs`,
  `lib/lsp-client.mjs`→`lib/client.mjs`, and `lib/process-stats.mjs`→`lib/sampler.mjs` (the
  `performance/` directory is gone). The npm aliases follow (`perf:lsp` becomes `perf:probe`);
  the diagnose logic now lives in `lib/{options,workspace,overlay,diagnosis,report}.mjs` so
  tooling can import its phases directly.

- **Breaking:** the workspace is restructured along rust-analyzer's layering and all `pdx-`
  crate prefixes are gone: `pdx-text`→`text`, `pdx-codec`→`transcode`, `pdx-parser`→`parser`,
  `pdx-rules`→`rules`, `pdx-game`→`game`, `pdx-analysis`→`ide`, and `pdx-lsp`→`pdc`. The old
  `pdx-engine` split into `vfs` (source roots, scans, document data), `hir` (semantic lowering),
  `index` (symbol shards and the analysis pipeline), and a slim `engine` orchestrator
  (analysis host, snapshots, `.pdcindex` persistence) that re-exports their API.
- **Breaking:** the language server binary is now `paradoxcode` (previously `pdx-ls`) and is the
  only shipped executable; the `pdc` crate name stays internal, and every user-visible surface
  (status bar, walkthrough, command titles, installer messages, release archives) shows
  **ParadoxCode** rather than an acronym. The `pdx` CLI is removed: vanilla/dependency cache
  building and game
  discovery are performed by the server itself (missing dependency caches are rebuilt in place;
  game selection moves to editor settings), and repository tooling (`check`, `release`, `index`,
  `setup`) moved to the unpublished `tools` crate
  (`cargo run -p tools -- check policy --root .`).
- **Breaking:** user-visible contract names drop the `pdx` prefix: index caches are
  `.pdcindex` (old caches are regenerated), compiled rules are `.pdcrules`, the virtual
  localisation URI scheme is `pdcloc://`, workspace-local data lives under `.pdc/`, LSP methods
  and commands use the `pdc/` prefix, release archives are `paradoxcode-v…`, and diagnostic codes
  are `pdc-parser-*`/`pdc-localisation-*`. The VS Code setting `paradoxcode.pdxLsPath` is replaced
  by `paradoxcode.serverPath` (the old key is still honoured).
- CI is reorganized the rust-analyzer way: no path filtering or skip bookkeeping, every job
  runs on every pull request, and the fast lint gates (rustfmt, clippy, rustdoc) are separate
  jobs that do not wait behind the test suite. Following rust-analyzer's actual workflow
  further: a hardened global env (retries, no incremental artifacts, short backtraces), bash
  as the default step shell, an Ubuntu/Windows test matrix with `fail-fast: false` plus a
  run-cancelling companion job, a `conclusion` job aggregating everything into one branch
  protection check, nextest as the test runner, cargo-machete for unused dependencies,
  a pinned crate-ci/typos gate (game-data spellings allowlisted in `_typos.toml`), clippy
  rejects `dbg!`/`todo!`, and rustc/clippy diagnostics surface as inline annotations through
  the `.github/rust.json` problem matcher.
- Dependencies left behind by the crate carve are removed: `engine` drops `encoding_rs`,
  `postcard`, and `zstd`; `index` drops `postcard`, `rustc-hash`, `serde`, `serde_json`, and
  `transcode`; `pdc` drops `hir`, `sha2`, and `toml`; the fuzz workspace drops `engine`
  (all verified by cargo-machete and a full rebuild).
- **Breaking (tooling entry points):** the shell-script layer is gone. `tools gates` runs the
  local quality gates in Rust (`cargo tools gates [core|core-fast|perf|vscode|release|fuzz|all]`,
  aliased through `.cargo/config.toml` in rust-analyzer style) and replaces
  `scripts/check-quality-gates.sh`; `scripts/check-release.sh` and `scripts/diagnose-current-mod.sh`
  are inlined into CI and `node scripts/diagnose-current-mod.mjs` respectively; the one-off
  `scripts/audit-quoted-scripts.mjs` is deleted (its capability ships in the analyzer). The
  `flag-audit` rule-data bin is now documented alongside `bake`.
- The Node dev-tooling scripts move under `editors/vscode/scripts/` so the repository keeps a
  single Node home (rust-analyzer keeps its TypeScript under `editors/code`):
  `diagnose-current-mod.mjs`, `lib/`, and `performance/` relocate unchanged in behavior, gain
  `npm run diagnose` / `perf:lsp` / `perf:compare` entries, and stay outside the packaged VSIX
  via the existing `files` allowlist. The top-level `scripts/` directory is gone.

### Removed

- The versioned Git pre-commit hook and `scripts/install-git-hooks.sh`. Quality gates run in
  CI on every pull request; run `cargo tools gates` locally when needed.
- The EU4 CJK localisation planning documents (`docs/eu4-cjk-localisation-requirements.md`,
  `docs/eu4-cjk-localisation-design.md`). The feature shipped and is verified end-to-end;
  its user-facing behavior is documented by `docs/diagnostics.md` and the crate docs, and
  the code comments no longer cite the design sections.

### Added

- UNC paths work end to end: `file://server/share/...` URIs from the client decode to
  `\\server\share\...` and `pdcloc://` keeps its twin semantics, while server-constructed
  URIs of a verbatim UNC root (`\\?\UNC\server\share`) serialize as a proper authority URI
  instead of a broken backslash spelling. Non-Windows hosts still reject remote authorities,
  where no filesystem path exists.
- Release sweep gate: `scripts/sweep.mjs` (npm `release:sweep`, workflow `sweep.yml` on release
  publication, self-hosted runner) cold-starts the server, lets it rebuild the Vanilla index
  cache, diagnoses the full Vanilla workspace through virtual overlays, and records per-phase
  timings, a stable diagnostics fingerprint, whole-lifetime server CPU and peak working set
  (sampling starts at process spawn, before the handshake), and interactive-query latency
  percentiles (p50/p99/max for completion, hover, definition, references, documentSymbol, and
  the incremental didChange->publishDiagnostics round trip) over a spread of sampled files.
  Each run appends a summary line to `performance-results/history.jsonl`; with a previous
  summary the gate fires on fingerprint drift rather than the known nonzero Vanilla error
  baseline. First full-Vanilla baseline on the reference machine: 8,670 files diagnosed in
  ~13 s, full sweep including query sampling ~1 minute.
- Template key matchers for parameterized rule families: `KeyMatcher::Template` splices a
  workspace type or static-enum member between literal affixes, with an optional member-prefix
  strip (estate members `estate_nobles` spell keys `nobles_…`). Completion expands the template
  into concrete spellings, unknown-key suggestions include them, and the rule compiler rejects
  `<...>` placeholders in exact keys so a dead placeholder row cannot return (rules artifact
  schema 26; caches regenerate).

### Changed

- Source roots, scanned files, document paths, and disk-change events are typed as
  `text::AbsPath`, a construction-only wrapper over `PathBuf` whose spelling is normalized by
  its constructors; the physical-path lookup map keys on it, so "every ingress canonicalizes"
  is now a type invariant instead of a convention. Editor paths that intentionally fall back
  to URI-derived logical paths keep their graceful degradation.
- The editor extension consolidates path handling into `src/paths.ts`: one glob converter,
  and diagnostic-ignore prefix matching that folds Windows drive/directory casing (a settings
  root spelled with a different case no longer silently disables the ignore list). Mission
  preview logical paths derive from the decoded `fsPath` instead of percent-encoded URI
  components, so spaces and non-ASCII directories reach the server as real names. The dead
  legacy `serverPath` fallback read and the duplicated glob converter are gone, and the
  sweep/diagnose scripts share one relative-path helper.

### Fixed

- `pdc flag-audit` collects `.TXT` and `.GUI` files regardless of extension casing; previously
  uppercase variants were silently skipped while directory names were already folded.
- Mission `texturefile` values written with Windows separators (`gfx\interface\x.dds`)
  normalize to forward slashes instead of being dropped entirely; drive-letter and escaping
  paths are still rejected.

- Whole-workspace validation no longer pegs every worker core indefinitely when a document is
  open (the "open a file, CPU jumps to 4 cores and all language features freeze" failure mode
  on large EU4 workspaces). Three defects compounded: staging a document open wiped the whole
  query cache instead of only the documents domain (`stage_open_document` used the full
  revision advance), the cache's single global revision gate then dropped every insert from
  the pass's base revision — so each per-property rule-view lookup rebuilt from scratch and
  every file recomputed the workspace-wide dynamic-contract report — and the pass kept
  grinding on a superseded snapshot whose results the completion handler was guaranteed to
  discard. The cache now tracks revisions per domain (index-derived entries serve every
  reader whose revision observes the same index state), document staging advances only the
  documents domain, hosts expose a clone-shared `live_revision` so the pass aborts as soon as
  its base revision is superseded (the existing mismatch path reschedules it), and dynamic
  contract inference consults its memo before re-resolving a callee definition, which also
  removes a per-property template deep-clone. On the 12-file reproducing workspace the pass
  went from never completing to finishing in ~1.5 seconds with zero idle CPU.
- The five engine-parameterized modifier families actually validate now. They were exact-key
  rows with literal spellings such as `<estate>_loyalty_modifier` that never match a real key,
  so every concrete spelling (`nobles_loyalty_modifier = 0.1`,
  `monthly_divine_authority = 1`, `<power>_gain_modifier`, …) was an unknown-key false
  positive while completion offered the placeholder literals. The rows now resolve through the
  workspace `estate`/`government_mechanic_power` domains — mod-added estates and mechanic powers
  included — `<estate>_loyalty_equilibrium` joins the family, the six redundant hardcoded power
  instances (`monthly_arabic_trade_influence_power`, `monthly_asha_vahishta`, `monthly_blood`,
  `monthly_militarized_society`, `monthly_persian_influence`, `monthly_russian_modernization`)
  are deleted in favour of the template, and the 23 special estate modifiers declared by
  `common/estates_preload` (religion-scoped, exclusive, and piety variants such as
  `brahmins_hindu_loyalty_modifier` and `nobles_exclusive_influence_modifier`) gain first-party
  rows.

- Path spellings are now uniform across the engine: document open/save/watched-file ingress
  and source-root resolution canonicalize through `dunce`, which returns real-cased paths
  without the Windows extended-length (`\\?\`) prefix, replacing the two hand-rolled
  normalizers (`normalize_workspace_path`'s raw `fs::canonicalize` and `game::portable_path`).
  URI conversion moves behind a typed `pdc::FileUri` built on the `url` crate, so the
  hand-written percent codec is gone and round trips are the standard's guarantee. Index
  caches recorded by older releases with verbatim root spellings keep loading; comparisons
  accept both spellings.
- `pdc index vanilla` and `pdc index dependency` record canonical source roots without the
  extended-length prefix, matching what `pdc setup` has always persisted.

## [0.3.2] - 2026-09-11

This release reworks hover into a first-class presentation surface, deepens dynamic-definition
analysis so completion, hover, and diagnostics replay one shared per-site derivation, and
rebuilds the EU4 variable model and modifier data from wiki and vanilla evidence. Steady-state
diagnostics latency on large workspaces drops to under a second.

### Added

- Dynamic-definition completion round: candidate lists filter by the inferred entry-scope
  contract, key-rendered parameters complete their commands or derive candidates from rule keys
  matching their literal affixes, and editing a scripted definition's body seeds the completion
  scope from its own contract when that contract pins a single concrete scope.
- Hover rework: titles follow the symbol-hover pattern (`### Trigger `is_part_of_hre``, `###
  Modifier `x``), an ambient `#### Scope` table shows the active here/root/prev/from registers
  and scope transitions, localisation previews render every language in parallel (capped at four
  plus a count) including generated event `.t` titles, `$NAME$` fragments inside localisation
  values resolve as nested keys, scripted localisation, or context placeholders, and
  ambiguous-symbol candidate lists carry line/column positions.
- Rule-data documentation backfill from the EU4 wiki and repository semantics: 148 trigger/effect
  keys, 515 modifier keys, and the definition-body fields of events, decisions, missions, and
  the shared structural fields.
- Typed entry keys: `luck` entries are country tags and `government_ranks` entries are rank
  numbers 1-10. Unknown keys are reported with the expected type, and completion in the root gap
  offers workspace country tags.
- First-party EU4 rule-data corrections: four constructs from the patchnotes' Usermodding
  sections (`militarized_society`, `add_militarised_society`, `back_current_issue`, and the 1.37
  state-edict on_action hooks), the `hussars_cavalry` province effect rows, and the wiki-audit
  arbitration: nine missing modifiers, estate privilege templates, renames to wiki spellings,
  corpus-tightened bool/float keys, trigger/effect shape corrections (`is_discounted`,
  `num_of_musketeers`, ...), and `mean_time_to_happen` cardinality fixes.

### Changed

- The `government_attributes` enum (132 hardcoded entries) is gone, replaced by
  a TypeDescriptor that harvests `custom_attributes = {}` children from
  `common/government_reforms`. EU4's government attributes are defined where
  they are consumed, so workspace definitions now drive both completion and
  enum validation for `has_government_attribute`/`has_government_attribute_short_desc`
  — mods see exactly the attributes they (or vanilla: 149 distinct keys)
  define. The 52 wiki-only spellings that vanilla never defines leave
  completion; legacy flat reform flags keep passing through the any_scalar
  row.
- Dynamic-parameter constraints are derived once per revision as replayable per-site rows
  consumed by completion, hover, and diagnostics, so the three surfaces can no longer drift.
  Quoted payloads validate at their real render-site context and scope - including
  `limit`-style structural sub-blocks and blocks whose key is itself rendered from a
  parameter - and parameters embedded between literal affixes constrain their bare argument by
  splicing.
- Whole-workspace diagnostics now default to off on the server as well, matching the published
  VS Code default; the explicit validateWorkspace command keeps its opt-in semantics.
- Steady-state diagnostics latency: the dynamic cycle-graph report is skipped for files that
  define no dynamic definitions and its call-site binding collection is cached across keystroke
  revisions, and the index query cache survives overlay parse commits. Edit-to-publish latency
  on the 7,907-file test mod drops from ~3.0s to 0.4-0.7s per file.
- HIR scope lowering merges inherited rule contexts, so scope inlay hints, typed references,
  and transitions see the effect vocabulary inside country and province history and on_action
  bodies; one vanilla false positive about event-modifier names in on_action bodies is gone.

### Fixed

- `export_to_variable = { value = trigger_value:<trigger> }` no longer flags every
  spelling outside the 34-entry imported whitelist (#34). A new typed-prefix value
  matcher accepts `trigger_value:` followed by any known trigger whose compare forms
  include an int, float, or bool operand — vanilla's multi-form aliases
  (`land_forcelimit`, `diplomatic_reputation`, …) resolve through their numeric rows,
  while token-returning triggers (`primary_culture`) and unknown names stay rejected.
  The open rule covers the `export_to_variable` value sites (effect context,
  `variable_arithmetic_trigger`, and `new_diplomatic_actions` AI acceptance);
  completion offers the qualifying `trigger_value:` spellings. The imported enum
  now keeps only its bare keywords — the 34 hardcoded `trigger_value:*` and 6
  `modifier:*` entries were removed as redundant (`modifier:` values remain open
  through the dynamic-value prefix bypass). The four sibling variable effects
  (`set_variable` and friends) stay literal-plus-variable only: the wiki documents
  `trigger_value:` exclusively for `export_to_variable`, and vanilla uses it there
  and nowhere else.
- The wiki's Variables page is now fully covered: the four arithmetic effects
  `round_variable` (new in 1.37), `sqrt_variable`, `random_variable`, and
  `modulo_variable` validate in their documented shapes (including the second
  `which = <var>` that substitutes for `value`), and the `export_to_variable`
  bare-value enum carries the wiki's Exportable Values union — 34 documented
  spellings (`war_exhaustion`, `base_tax`, `monarch_age`, …) that previously
  flagged as invalid now validate, while the undocumented `ruler_adm`/`ruler_age`/
  `ruler_dip`/`ruler_mil` variants are rejected in favour of `ADM`/`DIP`/`MIL`
  and `monarch_age`.
- The remaining imported whitelist patches around the variable model are gone,
  replaced by principled rows. The arithmetic siblings (`multiply_variable`,
  `divide_variable`, `subtract_variable`, `change_variable`) now accept
  `value = <var>` through the dynamic variable set, as `set_variable` already
  did — vanilla's `recruit_foreign_general` AI acceptance feeds
  `OpinionOfFROM`/`ArmyTradtionOfFROM` back in exactly that shape — so those
  hardcoded names, together with the pronoia pair, left the enum: they are
  variable references, not export spellings. The `"0"`/`"1"` constants came out
  the same way, with the AI-acceptance `value` site gaining an integer row
  (mirroring `peace_treaties`) for vanilla's `value = 1` seeds. The
  `check_variable` `which` enum fallback and the two dead `modifier:` export
  rows were dropped alongside the `variable_name` enum they referenced — the
  dynamic variable set and the dynamic-value prefix bypass already cover every
  vanilla site.
- Completion offers statement keys again after a complete single-line block statement
  instead of yielding zero items.
- Nested blocks in contexts that inherit their rules (for example `if` inside country
  history) complete the full inherited vocabulary again instead of only clause keys.
- Scalar invocation (`name = yes`) of a definition whose parameters are all optional is
  accepted as its parameterless form.
- The hover-compare harness scripts fail loudly when an explicit `--server` path does not
  resolve instead of silently falling back to a stale build.

## [0.3.1] - 2026-09-05

This release rebuilds the diagnostic system around a consolidated 16-code table with
script-facing messages, and replaces the runtime macro-expansion machinery with
first-class dynamic-definition rules derived from the workspace itself.

### Added

- Dynamic-definition analysis: scripted triggers/effects are compiled into first-class
  rule rows at scan time. Call sites validate entry scopes and parameters, quoted
  payloads are checked against the rows, branch-active parameters apply per
  `if`/`else_if` branch, and a definition whose inferred entry contract is empty is
  rejected at the definition.
- Closed dynamic kinds reject unknown flags against an engine-set whitelist, and
  completion offers the known flag names for them.
- First-party EU4 rule data corrections: trigger/effect scopes corrected, boolean
  containers added, explicit `any` scopes enforced rule-data-wide, and neighbor/culture
  scope rows widened to match vanilla usage.
- Parameter hovers show the dynamic-definition contract, and unquoted payload arguments
  are flagged.
- Development tooling: `pdx-flag-audit` for parser-based flag accounting, plus an LSP
  wire-traffic trace mode for head-to-head comparisons.

### Changed

- Diagnostics system refactor, message quality round:
  - The diagnostic code table is consolidated from 22 to 16 codes. Removed:
    `UnknownSymbol`, `AmbiguousSymbol`, `UnknownScope`, `InvalidTarget`,
    `TargetWrongScope`, `AnalysisIncomplete`, `UnknownBareValue`,
    `InvalidScopeCommand` (folded into `InvalidValue`, message wording kept), and
    `DynamicCallScopeMismatch` (folded into `WrongScope`, message wording kept);
    `RuleWrongScope` renamed to `WrongScope`; added `UnknownLocalisationKey`,
    `AmbiguousDefinition`, `InvalidDependency`. Removed codes are not aliased -
    configurations naming them fail fast at initialize. See
    `docs/diagnostics.md` for the full migration table.
  - Duplicate definitions resolve later-wins and report one
    `AmbiguousDefinition` warning at the effective definition with a related
    location on the shadowed one.
  - EU4 mission trees validate dependencies in the main pipeline
    (`InvalidDependency`): missing required missions, illegal placements
    (slot/row rule), cycles, and zero positions.
  - Messages describe the script, never the rule index: rule provenance,
    matcher vocabulary (`int`, `enum[...]`), "bare value"/"value clause"
    wording are gone from every user-facing surface; constraints render on
    `expected:` lines; cardinality findings anchor on the block's opening
    brace or the over-quota entry; parser errors quote the offending token;
    quoted-script limit messages state their numbers.
  - Did-you-mean suggestions (with quick fixes) cover unknown keys, enum
    values, localisation keys, symbols, scope names/commands, and
    localisation commands.
  - Diagnostics carry editor tags: over-quota and always-true findings are
    `Unnecessary` (dimmed); usage of deprecated declarations is `Deprecated`
    (strikethrough). `codeDescription` links every code to
    `docs/diagnostics.md`; code actions associate with their diagnostic and
    did-you-mean fixes are preferred; `publishDiagnostics` carries the
    document version; the list-truncation marker anchors on the last
    retained diagnostic instead of (0,0).
- "Macro" terminology is retired across code and user surfaces in favor of dynamic
  definitions.

### Fixed

- Hover value-matcher labels no longer double-wrap in code spans.
- Nested dynamic-rule derivation threads the parent path correctly.
- Dispatch-key checks are context-aware for dynamic rules.
- Completion no longer falls back to generic suggestions for non-enumerable parameter
  values.
- Thread-local snapshot caches are keyed by host identity, so concurrent hosts observe
  their own snapshots.
- Logical paths are derived from URIs for pathless overlays.

### Removed

- The runtime macro-expansion machinery: validation now runs against derived
  dynamic-definition rows, and the baked scripted-definition rows are gone from the
  EU4 rule data.

## [0.3.0] - 2026-09-02

This release ships a profile-driven performance round (whole-workspace validation CPU -68%, peak
memory -66%, diagnostics output unchanged), hardens language-server shutdown and watched-file
handling, and reworks EU4 vanilla installation discovery around launcher metadata.

### Added

- A profile-driven optimization round across the parser, HIR, rules, index, analysis, and LSP:
  whole-workspace validation CPU drops 68% and peak memory drops 66% on the head-to-head
  benchmark harness, and the single-threaded validation pass drops 78% (157.3s to 35.0s on a
  7,907-file workshop mod) while diagnostics output stays byte-identical.
- The workspace scan no longer blocks `initialize`; whole-workspace validation runs in parallel
  and document edits are validated immediately.
- Closed-file syntax trees are evicted after validation and reparsed on demand, index vectors
  shrink to their exact size, and localisation cache previews are retained only for preferred
  languages.
- Completion covers the remaining EU4 root entries, including `on_action`, and the first-party
  rules ship `on_action` scope documentation.
- VS Code gains dependency-management commands: **ParadoxCode: Add Dependency** opens a
  folder-and-cache wizard that appends the ordered `paradoxcode.dependencies` entry, with
  matching Remove and Open Dependency Settings commands.
- Installation discovery now resolves launcher metadata first: Steam library roots from the
  Windows registry plus per-library `appmanifest` install directories (with build ids), Epic
  Games manifest install locations, and GOG registry entries. WSL setups additionally probe
  Windows Steam libraries under `/mnt`, accepting foreign-platform executable markers there
  only.
- Multiple validated installations are resolved deterministically (explicit root > launcher
  metadata > common-location guess, then newest marker executable) instead of aborting setup;
  alternatives are reported and can be overridden with `--source`.
- The user configuration records how an installation was resolved (`resolved_via`) and the
  launcher-reported `game_build`, and discovered paths no longer carry Windows verbatim
  (`\\?\`) prefixes.

### Changed

- Editor configuration lives in the VS Code extension's `paradoxcode.*` settings; the shared
  project file is gone.
- VS Code's whole-workspace diagnostics default to off; opened documents are always validated
  either way, and the resolved setting is forwarded unconditionally so untouched
  configurations no longer fall back to the server-side default.
- An unavailable user-level Vanilla cache rebuild now records the resolved source so the
  recovery path never searches twice.

### Fixed

- Language-server shutdown drains every queued workspace worker and in-flight watched-file
  republication before exiting, eliminating shutdown-race interleavings; the queued ready pass
  survives partial watched-file batches, and identical overlay diagnostics are no longer
  republished after a scan commit.
- Comments are preserved during encoding recovery.

### Removed

- Deep full-disk installation search and the `pdx setup vanilla --deep` flag. Unfound
  installations are selected explicitly with `--source` (or probed under `--root`).

## [0.2.0] - 2026-08-28

This release brings the CWTools-inspired analysis and workspace infrastructure into the
EU4-first engine while keeping the first-party JSON rule source authoritative.

### Added

- Code actions and diagnostic quick fixes, scope-transition inlay hints, and ranged/delta semantic
  token requests.
- Workspace validation, explicit reindexing, bounded workspace diagnostics, diagnostic suppression,
  and configurable workspace scan filters.
- Scripted-localisation indexing and completion, persistent syntax-tree caching, and expanded EU4
  profile/rule coverage for file categories, scopes, events, and decisions.

### Changed

- Workspace loading, per-file indexing, immutable snapshots, and analysis query caching now reuse
  shared parsed data and scale better across large mods.
- The persistent parse cache uses the maintained `postcard` codec; its schema and namespace were
  advanced so existing entries are safely rebuilt.
- Windows definition and diagnostic URIs are normalized consistently across editor clients.

### Fixed

- Diagnostics and completion retain the correct semantic context after assignments and nested blocks,
  including scripted-localisation and mixed-scope cases.

## [0.1.4] - 2026-08-26

Maintenance release focused on EU4 rule coverage, semantic completion quality, and editor
integration reliability.

### Added

- The first-party EU4 rule source is organized into catalog, semantic, supporting-table, and
  profile data with a single canonical `rule_hash` covering the complete logical model.
- Common EU4 path whitelists and event modifier keys are covered by the profile and rule data.

### Changed

- Completion candidates are ranked by contextual and macro-expansion relevance, with scripted macro
  highlighting and completion kept aligned.
- VS Code retriggers completion after assignments and block edits, while the CI matrix keeps
  Windows release builds parallel with Windows test and lint checks.

### Fixed

- Completion and semantic analysis no longer lose context after assignments or nested blocks, and
  the legacy LSP compatibility path has been removed in favor of the current initialization contract.

## [0.1.3] - 2026-08-22

Maintenance release focused on formatting canonicalization, completion quality, and hover
reliability.

### Added

- Detailed startup and index-cache progress logs in `pdx-ls` make server bootstrap state visible to
  clients.
- Decision completion and hover work again after the semantic-context traversal rework.

### Changed

- The formatter canonicalizes the fixed script keywords `AND`/`OR`/`NOT` and
  `ROOT`/`FROM`/`PREV`/`THIS` to capitals, including scope references used as values; quoted text
  is not rewritten.
- Completion now suggests the uppercase intrinsic spellings (`THIS`, `ROOT`, `FROM`, `PREV`) and no
  longer offers lowercase variants.
- Control-flow keywords are highlighted as code keywords, with aligned semantic token layers.
- Hover is faster and safer: known-key sets are memoized per snapshot revision and richer
  formatting is cached.

## [0.1.2] - 2026-08-20

Maintenance release focused on editor readiness, workspace loading performance, and release
reliability.

### Added

- The language server emits a `pdx/ready` notification after initial workspace and index setup,
  allowing clients to distinguish a completed startup from a finished protocol handshake.
- Mission-tree preview titles now resolve from active workspace localisation definitions, including
  Mod overrides.
- Dependency index caches can be installed in one batched operation without changing source-priority
  semantics.

### Changed

- Mission-tree preview rendering now caches layout measurements, avoids unnecessary work outside the
  viewport, and coalesces high-frequency redraws for large trees.
- VS Code shows an explicit loading/ready state, refreshes open files after the server is ready, and
  serializes language-server restarts to avoid stale clients.
- Release downloads use longer timeouts and retry transient network or HTTP failures before archive
  verification.
- Release workflow no longer publishes to the VS Code Marketplace automatically; the VSIX remains
  attached to the GitHub Release for manual publication.

### Fixed

- Completion snippet indentation in generated insert text.
- Completion inside `if`/`limit` clause blocks where the clause context was lost.
- VS Code token scopes and highlighting for EU4 localisation and script.

## [0.1.1] - 2026-08-19

Maintenance release focused on the publish pipeline and release correctness.

### Fixed

- VS Code extension now ships its runtime dependencies in the packaged VSIX.
- CLI binaries derive their version from package metadata instead of duplicated constants.
- Release workflow reruns are idempotent: re-running the workflow on an existing tag no longer
  corrupts or duplicates release assets.

### Changed

- VS Code Marketplace publishing now uses OIDC-based trusted publishing
  (`vsce publish --oidc`); no long-lived `VSCE_PAT` secret is required.

## [0.1.0] - 2026-08-19

Initial alpha release of the game-neutral `pdx-lsp` engine with an EU4-first profile.

### Added

- Loss-aware parsers for Paradox script and EU4 localisation; CSV handled as opaque/syntax-only.
- Typed HIR lowering with `UnknownConstruct` recovery; syntax errors never block analysis.
- Frozen per-file index shards and immutable workspace snapshots with targeted watched-file updates.
- Workspace resolution across unsaved buffers, the current Mod, ordered dependency Mods, and a
  persistent local Vanilla index with automatic EU4 installation discovery.
- Validated first-party EU4 rule database (JSON source compiled by `pdx-bake`) replacing CWT
  imports; `rule_hash`-based integrity for generated artifacts.
- LSP features over real JSON-RPC: diagnostics, completion, hover, go-to-definition, references,
  document/workspace symbols, conflict-aware rename, and safe document formatting with
  cancellation and stale-result protection.
- `pdx` CLI: `setup vanilla`, `index vanilla`, and `index dependency` commands.
- VS Code extension with zero-configuration, checksum-verified server setup and a mission-tree
  preview.
- Release workflow for five native `pdx-ls` target archives with checksum sidecars, GitHub
  Releases, and Marketplace publishing.
- Fuzz targets for script/localisation parsing, incremental edits, typed CST walks, HIR lowering,
  formatting, line indexing, and first-party rule parsing.

[Unreleased]: https://github.com/danxiaogu520/ParadoxCode/compare/v0.4.1...HEAD
[0.4.1]: https://github.com/danxiaogu520/ParadoxCode/compare/v0.4.0...v0.4.1
[0.4.0]: https://github.com/danxiaogu520/ParadoxCode/compare/v0.3.8...v0.4.0
[0.3.8]: https://github.com/danxiaogu520/ParadoxCode/compare/v0.3.7...v0.3.8
[0.3.7]: https://github.com/danxiaogu520/ParadoxCode/compare/v0.3.6...v0.3.7
[0.3.6]: https://github.com/danxiaogu520/ParadoxCode/compare/v0.3.5...v0.3.6
[0.3.5]: https://github.com/danxiaogu520/ParadoxCode/compare/v0.3.3...v0.3.5
[0.3.3]: https://github.com/danxiaogu520/ParadoxCode/compare/v0.3.2...v0.3.3
[0.3.2]: https://github.com/danxiaogu520/ParadoxCode/compare/v0.3.1...v0.3.2
[0.3.1]: https://github.com/danxiaogu520/ParadoxCode/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/danxiaogu520/ParadoxCode/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/danxiaogu520/ParadoxCode/compare/v0.1.4...v0.2.0
[0.1.4]: https://github.com/danxiaogu520/ParadoxCode/compare/v0.1.3...v0.1.4
[0.1.3]: https://github.com/danxiaogu520/ParadoxCode/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/danxiaogu520/ParadoxCode/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/danxiaogu520/ParadoxCode/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/danxiaogu520/ParadoxCode/releases/tag/v0.1.0
