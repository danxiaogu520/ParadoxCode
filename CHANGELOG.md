# Changelog

All notable changes to ParadoxCode are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

### Fixed

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

[Unreleased]: https://github.com/danxiaogu520/ParadoxCode/compare/v0.3.2...HEAD
[0.3.2]: https://github.com/danxiaogu520/ParadoxCode/compare/v0.3.1...v0.3.2
[0.3.1]: https://github.com/danxiaogu520/ParadoxCode/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/danxiaogu520/ParadoxCode/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/danxiaogu520/ParadoxCode/compare/v0.1.4...v0.2.0
[0.1.4]: https://github.com/danxiaogu520/ParadoxCode/compare/v0.1.3...v0.1.4
[0.1.3]: https://github.com/danxiaogu520/ParadoxCode/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/danxiaogu520/ParadoxCode/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/danxiaogu520/ParadoxCode/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/danxiaogu520/ParadoxCode/releases/tag/v0.1.0
