# ParadoxCode Agent Guide

This document defines the working conventions for agents and developers contributing to ParadoxCode. The goal: every change advances the established architecture and leaves behind verifiable, maintainable, traceable results.

## 0. Persistent Project Authorization & Product Direction

The project owner has authorized agents to take ongoing responsibility for ParadoxCode's technical execution, including planning, architecture decisions, implementation, refactoring, testing, performance verification, and documentation maintenance. Except for destructive operations, external releases, credential/billing actions, and decisions that materially change the product direction, agents should exercise reasonable technical judgment and continue forward without waiting for approval on every point.

Default collaboration rules:

- Protect the user's existing and uncommitted changes; do not clean up, overwrite, or roll back without direction.
- Prefer small, reviewable, revertible changes; avoid unverifiable monolithic rewrites.
- Separate behavior-preserving refactors from new features.
- Do not mark a phase as complete if automated verification has not passed.
- Decide ordinary implementation details autonomously; ask the user only for destructive operations, external releases, or major product disagreements.
- Record results, design changes, verification, residual risks, and next steps for each phase.
- Documentation status must reflect true end-to-end capability; do not substitute skeletons or unit tests for product closure.

The product direction is fixed as "generic `pdx-lsp` engine, EU4-first":

- The current version only requires making EU4 support complete, reliable, and releasable.
- The workspace, snapshot, index, analysis queries, LSP runtime, CLI, and release infrastructure should remain game-agnostic.
- EU4 paths, scopes, commands, symbols, and special semantics should be concentrated in the EU4 profile/module.
- No commitment to delivery timelines for other games; no complex plugin ABIs or speculative trait layers for a second implementation that does not yet exist.
- Prefer expressing game differences through rule data and profiles; only extract behavioral interfaces when a second real game proves the data approach insufficient.
- When adding other games, the core engine should be reusable without rewriting the workspace, index, LSP, or concurrency model.

## 1. Understand the Project First

Before starting any implementation, read in this order:

1. `docs/architecture.md` — data flow, crate dependencies, concurrency, identity, and error recovery;
2. RFCs directly related to the current task;
3. `docs/rfc/0013-first-party-rule-source.md` — first-party rule source, compilation, and the boundary prohibiting external rule input.

## 2. Default Agent Responsibilities

You are not here just to "write the code" — you are responsible for delivering a verifiable vertical slice within the existing design boundaries:

- First, confirm which Phase, crate, and RFC the task belongs to.
- First, check the current state and any existing uncommitted changes before deciding the edit scope.
- Implement in the smallest increment possible; avoid introducing frameworks without performance evidence.
- Add tests or fixtures that prove the behavior in the same change.
- If the implementation changes architecture constraints, public APIs, rule schemas, the CLI, or configuration, update the relevant documentation.
- Finally, run verification proportionate to the change, and clearly report any checks not run and residual risks.

When there is no implementation code yet, establish a minimal skeleton and verification path first; do not stack a full language-service feature directly.

## 3. Authority & Design Changes

When designs conflict, resolve by the following priority:

1. The user's current explicit requirements;
2. Accepted RFCs and `docs/architecture.md`;
3. Phase constraints from the current project state;
4. Unaccepted proposals, research notes, and implementation convenience.

If an accepted boundary must change:

- First, write up the problem, alternatives, impact, and migration cost.
- Update the relevant RFC's status, decision, and acceptance criteria.
- Then update the implementation plan and code.
- Call out in the delivery report that this is a design change, not an ordinary refactor.

Do not silently relax rules, swallow unknown input, bypass source-root resolution, or cram business logic into the LSP layer just to make tests pass.

## 4. Architecture Boundaries

The following dependency direction must be maintained:

```text
pdx-text
  -> pdx-parser -> pdx-engine -> pdx-analysis -> pdx-lsp
pdx-game (with EU4 profile)
pdx-rules -> pdx-bake
pdx-rules + pdx-game -> pdx-engine / pdx-analysis
```

The EU4 bootstrap/profile has been merged into the `eu4` module of `pdx-game`.

Layer responsibilities:

| Layer | Allowed to be responsible for | Must NOT be responsible for |
| --- | --- | --- |
| `pdx-text` | offsets, line index, UTF-8/UTF-16, URI/path primitives | EU4 rules, workspace state |
| `pdx-parser` | loss-aware CST for Paradox script and localisation, incremental parse, syntax errors, safe formatter | game rule database, disk scanning, LSP types |
| `pdx-rules` | generic source compiler core, rule schema, canonical view, `rule_hash`, read-only runtime API | concrete game name tables, external rule parsers, LSP, dynamic Mod symbols |
| `pdx-game` | data-driven install flags, cross-platform discovery, minimal validation, user-level local config, EU4 profile | semantic rules, workspace index, editor API |
| `pdx-bake` | developer CLI for strict first-party JSON validation and temporary artifact/manifest generation | CWT input, network sync, user rule overrides |
| `pdx-engine` | HIR lowering, VFS, overlay, source roots, parse cache, index shards, snapshot | LSP protocol types |
| `pdx-analysis` | snapshot-oriented diagnostics/completion/hover/navigation/rename queries | direct disk reads, editor client |
| `pdx-lsp` | lifecycle, capability negotiation, protocol conversion, cancellation, publish diagnostics | EU4 name tables, business queries, rule interpretation |
| `editors/zed` | language metadata, queries, server fetch/verify/launch, config forwarding | symbol extraction, scope derivation, EU4 rule implementation or rule artifact distribution |

`AnalysisHost` is the owner of mutable state; requests read immutable `AnalysisSnapshot` values and must not hold the host lock during queries. Background results must validate document version or snapshot identity before committing.

## 5. Implementation Invariants

### Data & Identity

- Source priority is fixed: unsaved overlay > current Mod > ordered dependency Mods > Vanilla.
- `SourceRootId`, `SourceFileId`, `DocumentId`, `SymbolId` use stable identities.
- Do not use absolute path strings as cross-request symbol identities.
- Do not use CST node pointers as cross-request identities.
- Each file independently generates and replaces its index shard.
- Overridden definitions may be retained for explanation but must not become active navigation targets.
- The Vanilla cache is built or updated only on first configuration, explicit user refresh, or an automatic rule-hash mismatch rebuild at LSP startup; it does not auto-refresh on file changes.

### Error Recovery

- Syntax errors do not prevent producing a local CST.
- Lowering produces `UnknownConstruct` for unrecognized nodes; do not panic.
- Unknown scopes are retained as `Unknown` to avoid cascading errors.
- The rule compiler must fail on unknown fields, duplicate identities, invalid cardinality/severity, or artifact round-trip differences.
- The formatter returns zero edits with a clear reason when a CST is unsafe (`ERROR` node present).

### Rule Database

- `rules/eu4/*.json` is the sole authoritative rule source; generated SQLite artifacts are never hand-maintained.
- Official `pdx`/`pdx-ls` binaries embed the first-party JSON source and materialize a validated SQLite artifact in the user cache.
- The official runtime accepts no external rule path, download, search, or user override.
- The runtime does not read, download, or import any external rule source.
- `.cwt` files must not serve as input for rule compilation, testing, runtime execution, or updates.
- `rule_hash` and release manifests are generated integrity metadata, not a second rule authority.
- `rule_hash` hashes canonical logical content, not SQLite file bytes.
- The hash is unaffected by rowids, insertion order, page layout, indexes, VACUUM, timestamps, or import logs.
- Dynamic scripted effects, triggers, buildings, and similar members come from the `WorkspaceIndex` and must not be hardcoded into core crates or the extension.
- Imports must preserve source order, duplicate keys, and alternative identity; do not collapse into a plain map first.

## 6. Recommended Workflow

### Before Starting

1. `rg --files` to find relevant implementations, fixtures, scripts, and documentation.
2. Check `git status`; if the worktree is not a valid Git checkout, do not initialize or clean up the repository state on your own.
3. Read the RFCs and existing tests associated with the task.
4. Write down the minimal scope, expected invariants, and verification commands for this change.
5. If pre-existing changes are found, preserve their content and only edit the areas the task requires.

The checkout uses the repository-versioned `.githooks/pre-commit` as the local quality gate. Run
`bash scripts/install-git-hooks.sh` after the first clone; when an agent discovers that `core.hooksPath` does not point to `.githooks`, it should install them itself. Normal commits run `git commit` directly, letting the hook invoke `scripts/check-quality-gates.sh` — there is no need to manually repeat the full suite of commands before committing. Only run the `core`, `grammars`, `zed`, or `release` groups individually when diagnosing a specific failure; CI continues to be responsible for environment-specific gates such as Windows, MSRV, and dependency policy.

### Git Publishing Convention

- The default publishing target is the repository's `main` branch.
- When the user asks to commit or push, work directly on local `main` and push with `git push origin main`.
- Do not create `agent/*` or other temporary branches, and do not open a pull request, unless the user explicitly requests one.
- Before publishing, inspect the worktree and stage only changes within the requested scope.
- If remote `main` has commits not present locally, stop and report the divergence; do not switch to a branch or pull-request workflow as a workaround.
- After publishing, verify that `HEAD` and `origin/main` match and that the worktree is clean.

### During Implementation

- Keep each change compilable, or at least locally verifiable.
- Use stable names for public APIs, diagnostic codes, symbol kinds, and schema rule IDs.
- User-supplied paths, file contents, and configuration errors must be returned explicitly; do not escape with `unwrap`/`expect`.
- `unsafe` is forbidden by default; if a dependency genuinely requires it, encapsulate it in the smallest boundary and document the safety contract.
- Background tasks must be cancellable, or have clear resource and time bounds.
- Do not implement speculative features or complex plugin systems for low-priority games; only retain extension points proven useful by the core engine and EU4 profile boundary.
- EU4 name tables and special rules belong in the EU4 profile/rules package, not in the generic LSP layer or the Zed extension.

### Before Completing

- Add or update tests at the appropriate level: unit, golden, integration, corpus, property/fuzz.
- For parser/formatter changes, use original fixtures; avoid copying Vanilla or reference-repo corpora.
- For rules/compiler changes, run source schema, foreign key, stable ID, canonical hash, and round-trip validation.
- For LSP changes, run real JSON-RPC transport tests.
- For Zed changes, run manifest/build smoke tests or whatever is feasible.
- Report the updated status of the affected phase or component.
- Report verification results, checks not run, and residual risks.

## 7. Testing Strategy

Choose tests at the level of the change; do not test only the innermost functions:

- `pdx-text` — offsets, line endings, UTF-16, URI/path;
- `pdx-parser` — typed CST, error extraction, incremental edits, error recovery, trivia safety, token preservation, idempotence;
- `pdx-rules` — schema, read-only loading, foreign keys, hash stability, and runtime invariants;
- `pdx-bake` — strict source schema, stable identity, invariants, deterministic hash, artifact round-trip;
- `pdx-engine` — scope transitions, unknown context, typed lowering, root order, overlay, override resolution, shard replacement, snapshot;
- `pdx-analysis` — diagnostics, completion, definition, references, hover, rename;
- `pdx-lsp` — real JSON-RPC, capability fallback, out-of-order versions, cancellation, stale diagnostics;
- Zed — manifest, Wasm/build, file recognition, and server launch smoke tests.

MVP fuzzing must cover at minimum: Script/localisation parse, incremental edit equivalence, typed CST walk, HIR lowering, formatter, line index, and first-party rule source parsing. EU4 CSV files are opaque resources in v0.1 and do not require a parser fuzz target. Any discovered crash must enter the regression corpus after being fixed.

## 8. Copyright, Security & Data Boundaries

- Do not commit or redistribute Vanilla EU4 files or user-local Vanilla caches.
- Rule artifacts only contain data confirmed redistributable, plus necessary provenance.
- The manifest records source format, target game version, artifact schema, canonical hash, and artifact checksum.
- Mod/rule configuration is never executed as arbitrary code.
- Scanning is bounded by file size, nesting depth, path escaping, and resource consumption limits.
- Compiler input paths are fixed, reproducible, stably ordered, and must reject duplicate logical identities.
- Full validation must complete before atomic publish; never leave a half-written database behind.

## 9. Delivery Report Format

When completing a task, the report should briefly contain:

1. Result: what was implemented and which files were involved;
2. Design: whether existing RFCs were followed, whether there were design changes;
3. Verification: which commands were run and what the results were;
4. Incomplete: which checks were not run and why;
5. Risks: known limitations, follow-up suggestions, and whether the next Phase is affected.

If the task is blocked, complete all checks that do not depend on external input first, then describe the blocking point, alternative paths attempted, and the minimal decision needed. Do not use silent degradation to mask missing rules, missing dependencies, or uncertain resolution results.

## 10. Common Documentation Entry Points

- [Documentation Index](docs/README.md)
- [Overall Architecture](docs/architecture.md)
- [System Boundaries & Crate Dependencies](docs/rfc/0001-system-boundaries.md)
- [Syntax, CST & Incremental Parsing](docs/rfc/0002-syntax-cst.md)
- [Workspace / VFS](docs/rfc/0003-workspace-vfs.md)
- [EU4 Rule Artifact](docs/rfc/0004-eu4-rules-schema.md)
- [HIR & Scope](docs/rfc/0005-hir-scope.md)
- [Symbol / Reference Index](docs/rfc/0006-symbol-index.md)
- [Diagnostics & Completion](docs/rfc/0007-diagnostics-completion.md)
- [Safe Formatting](docs/rfc/0008-formatter.md)
- [LSP Runtime](docs/rfc/0009-lsp-runtime.md)
- [Zed Integration](docs/rfc/0010-zed-integration.md)
- [Testing & Quality Gates](docs/rfc/0011-testing-quality.md)
- [First-Party Rule Source & Compiler](docs/rfc/0013-first-party-rule-source.md)
- [Generic PDX Engine & EU4-First](docs/rfc/0012-generic-engine-eu4-first.md)
