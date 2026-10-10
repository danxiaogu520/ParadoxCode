# Contributing to ParadoxCode

Start with the [product overview](README.md) and [governance](GOVERNANCE.md).
This guide owns development setup, validation choices, and documentation maintenance.

## Build and debug

Use the Rust minimum declared in [Cargo.toml](Cargo.toml) or a newer stable toolchain,
and the Node.js version selected by [CI](.github/workflows/quick-ci.yml) for extension work.
Use the committed lockfiles. No machine-local Cargo alias or commit hook is required.

```sh
cargo build --locked --workspace
cargo test --locked --workspace --all-targets
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
npm ci --prefix editors/vscode --no-audit --no-fund
npm --prefix editors/vscode run check
```

Run focused tests during development, such as `cargo test -p ide completion` or
`cargo test -p pdc format_command`. For extension debugging, open `editors/vscode` in VS Code
and use its [checked-in launch configuration](editors/vscode/.vscode/launch.json).
Server integration and configuration belong to the [server guide](crates/pdc/README.md)
and [extension guide](editors/vscode/README.md).

## Architecture

Source text passes through loss-aware syntax, rule-aware HIR, per-file index shards,
immutable workspace snapshots, editor-neutral queries, and the LSP adapter.
EU4 spellings and data belong to the [first-party rule package](rules/README.md);
shared analysis mechanisms belong to the engine and analysis layers.

Module and crate rustdoc owns implementation responsibilities and public API contracts.
The table below is generated from Cargo manifests: it lists direct runtime workspace
dependencies, rather than implying that the workspace is a single linear chain.

<!-- generated:crate-dependencies:start -->

| Crate | Direct runtime workspace dependencies |
| --- | --- |
| [transcode](crates/transcode/src/lib.rs) | — |
| [text](crates/text/src/lib.rs) | — |
| [parser](crates/parser/src/lib.rs) | `text` |
| [rules](crates/rules/src/lib.rs) | `text` |
| [game](crates/game/src/lib.rs) | `engine`, `parser`, `rules`, `text` |
| [vfs](crates/vfs/src/lib.rs) | `parser`, `rules`, `text`, `transcode` |
| [hir](crates/hir/src/lib.rs) | `parser`, `rules`, `text`, `vfs` |
| [index](crates/index/src/lib.rs) | `hir`, `parser`, `rules`, `text`, `vfs` |
| [engine](crates/engine/src/lib.rs) | `hir`, `index`, `parser`, `rules`, `text`, `transcode`, `vfs` |
| [ide](crates/ide/src/lib.rs) | `engine`, `game`, `hir`, `parser`, `rules`, `text`, `transcode`, `vfs` |
| [pdc](crates/pdc/src/lib.rs) | `engine`, `game`, `ide`, `parser`, `rules`, `text`, `transcode` |
| [tools](crates/tools/src/lib.rs) | `engine`, `game`, `ide`, `parser`, `pdc`, `rules`, `text` |
<!-- generated:crate-dependencies:end -->

Keep user-facing protocol conversion in `pdc`, VS Code UI in the extension, and
semantic decisions in `ide`/HIR/rules. Generic structured-view graph and writeback mechanisms
live in `engine`; game layout policy lives in `game`. Extend existing responsibilities
before introducing another layer.

## Engineering conventions

- Workspace Cargo lints forbid `unsafe`.
- User-controlled input returns errors; it must not trigger `unwrap`/`expect` panics.
- Files, documents, roots, and symbols use stable identities across requests.
- Background work cooperates with cancellation or has explicit resource bounds.
- Malformed source remains available for analysis through loss-aware syntax and unknown nodes.
- `rules/eu4/` is the first-party rule authority. Builds check and embed its compiled IR;
  runtime analysis does not load external rule paths.

Choose tests by behavior: parser recovery/format preservation, HIR definitions and scopes,
engine overlay/cache invalidation, IDE query results, or real LSP transport and lifecycle.
Use repository-owned minimal fixtures. Fixed fuzz crashes become regression corpus inputs.
Rule authors start with the [rule-package workflow](rules/README.md).

## Validation

Each validation class has one authority:

| Class | Authority | Purpose |
| --- | --- | --- |
| Developer feedback | Local checkout | Focused tests, deterministic local groups, local Vanilla exploration |
| Merge gate | [CI](.github/workflows/quick-ci.yml) | Required `Conclusion` check on the reviewed commit |
| Scheduled audit | [Security](.github/workflows/security.yml), [Performance](.github/workflows/performance.yml) | Advisory refresh and optimized benchmark runs |
| Release gate | [Candidate workflow](.github/workflows/release-candidate.yml), [promotion](.github/workflows/release.yml) | Verify complete artifacts before creating a formal tag, then publish those same bytes |
| Manual acceptance | Maintainer following [RELEASING.md](RELEASING.md) | Clean-profile install and Marketplace publication |

Select the affected local group with `cargo run --locked -p tools -- gates GROUP`.
The available groups and their commands are implemented in
[the gate runner](crates/tools/src/gates.rs); `cargo run --locked -p tools -- --help`
shows the CLI. Rust behavior normally calls for `core-fast`, extension work for `vscode`,
repository/docs changes for `policy`, embedded/distribution changes for `artifact`, and
fuzz changes for `fuzz`. Performance-sensitive work also runs a targeted benchmark or `perf`.
The default local run includes deterministic groups; optimized `perf` remains opt-in.

Before committing, run formatting and focused checks. Before opening/updating a PR, run affected
groups and record exact commands, outcomes, and residual risks. Passing local groups does not
replace clean-checkout, cross-platform CI or authorize publication. A local commit does not
require a full Vanilla sweep, benchmark suite, or network advisory scan.

CI runs a deliberately reduced Linux-only merge gate on PRs and `main`. It checks formatting,
type-checks every workspace crate with all features, runs parser/text/encoding library regressions
and lightweight CI/release-control regressions, compiles the extension, runs five JavaScript
behavior suites, and audits production npm dependencies. `Conclusion` requires both jobs to succeed;
failed, cancelled or skipped jobs fail the gate.

This trades breadth for a roughly two-minute feedback target, not a guarantee for cold caches or
slow hosted runners. Routine CI omits Windows, broad HIR/rules/engine/IDE/LSP suites, integration
and doc tests, Clippy, MSRV, rustdoc, fuzzing, repository/artifact policy, VSIX packaging and Cargo
dependency/typo checks. Run affected local groups before review; regressions outside the core gate
can otherwise reach main.

[Full CI](.github/workflows/ci.yml) is manually dispatched on the exact reviewed main commit before
release preparation. It retains cross-platform quality jobs, source-bound receipts and validated
VSIX artifacts. Modest test optimization and bounded fixtures target roughly five-minute feedback,
including setup, without guaranteeing cold-cache or hosted-runner timing. Linux runs the complete
maintained Rust suite in isolated nextest processes. Windows runs parser/text/encoding tests,
selected URI/UNC/Unicode, cache, disk and lifecycle regressions, and a real-binary LSP smoke test.
It deliberately no longer repeats every analyzer unit test or doctest on Windows. Full Windows
unit parity remains available locally. Windows validation uses unoptimized test/build profiles;
Linux tests use modest optimization. CI-only overrides leave development and releases unchanged.

Test selection removes repeated file-type completion matrices, peripheral EU4 rule examples,
detailed trace/cache-progress variants, and duplicate semantic transport examples. Goldens, core
templates/scopes, navigation/rename, lifecycle/cancellation, corruption/recovery and release safety
remain. The deep Template fixture has 1,024 calls, the overlay fixture eight files, and cache shrink
uses disposable SQLite pages instead of 20,000 parsed events. Full-corpus acceptance remains;
double-bake determinism uses a small representative package. Full CI covers the maintained suite,
not every historical scenario. Quick CI creates no release evidence.

For pre-merge validation of Full CI itself, maintainers can explicitly push the exact reviewed SHA
to a `ci/full-validation/NAME` branch. Ordinary PRs never trigger this suite. These validation-only
push receipts are rejected for release preparation, which still requires manual Full CI on main.
A failing post-merge `Conclusion` receives a focused repair PR. Scheduled audit failures are
triaged by the maintainer into an actionable issue; they do not block unrelated PRs.
Release and manual acceptance failures follow the recovery steps in [RELEASING.md](RELEASING.md).

### Local Vanilla acceptance

Run a local full sweep when rules, diagnostics, scope/resolution, parser/HIR semantics,
index construction, or workspace queries can change existing game behavior.
Documentation, isolated UI changes, and behavior-preserving refactors covered by focused tests
normally do not need it. The [audit guide](crates/tools/README.md#diagnostics-and-semantic-evidence)
owns invocation and report handling; the [performance lab](lab/perf/README.md) owns local comparisons.

Licensed game data, excerpts, caches, binary snapshots, and raw reports stay in ignored local
directories. They must not be copied to Actions, issues, PRs, or Releases. Record only a short
redistribution-safe conclusion and reduce defects to repository-owned fixtures.
Local Vanilla acceptance informs development; remote `Conclusion` retains merge authority.

## Changes and review

Persistent index and parse caches use the workspace LSP version. Release versions are immutable;
semantic or cache-format changes require a new version before publication. During development
at the same version, rebuild Vanilla/dependency indexes and clear the persistent parse
cache before validating changed parser or analyzer behavior.

Open an issue for substantial work to record the problem, accepted scope, and deferred work.
Keep refactors, behavior changes, and release/process work independently reviewable.
Use draft PRs while implementation or review evidence is incomplete: the
[autosync workflow](.github/workflows/pr-autosync.yml) enables auto-merge for eligible maintainer PRs.

Use Conventional Commit subjects for commits and PR titles: `type(scope): summary`.
Use `!` and a `BREAKING CHANGE:` trailer for breaking changes. Explain the concrete problem,
resulting behavior, validation, and material residual risks in the PR description.
Branch protection, merging, and ownership rules live in [GOVERNANCE.md](GOVERNANCE.md).

## Local generated output

Repository-generated reports, benchmark snapshots, profiles, experiment binaries/caches, and
release staging belong under `target/`. Diagnostic clients use `target/diagnostic-reports/`,
performance exports use `target/performance-results/`, the lab uses `target/perf/`, and release
packaging uses `target/dist/`. The shared tools path helper enforces the report-output boundary.
The existing `data/` corpus remains an input. On-demand JSON Schema output also belongs under
`target/`, as does explicitly baked IR. Documentation projections and owned regression/fuzz
fixtures remain beside their authoritative source.

## Documentation ownership

Stable documentation lives beside the implementation it explains. Temporary design plans and
active proposals belong in the repository root, with an explicit proposal status. Root READMEs
provide orientation and links; component guides explain use and stable concepts. Keep both root
language entries aligned in purpose without copying configuration tables, version numbers,
or protocol details.

| Information | Authoritative source | Reader view |
| --- | --- | --- |
| Published releases and changes | GitHub Releases; `CHANGELOG.md` | Root badges and release links |
| Rust/tool dependencies | Cargo manifests and lockfiles | Generated crate table above |
| Extension settings, commands, agent tools | `editors/vscode/package.json` and NLS bundles | [Generated reference](editors/vscode/REFERENCE.md) |
| Rule source fields and serialization | `crates/rules/src/source.rs` | On-demand JSON Schema (`rulec schema`) and generated [source reference](crates/rules/SOURCE-REFERENCE.md) |
| Rule-language semantics and rationale | [LANGUAGE.md](crates/rules/LANGUAGE.md), compiler tests | Semantic guide; complete examples are validated by the documentation check |
| Script diagnostic identifiers/default severity | `ide::DiagnosticCode` | Generated index in [diagnostic guide](crates/ide/DIAGNOSTICS.md) |
| CLI options and bounds | CLI implementations and `--help` | Runnable help; guides explain workflows |
| Public API and module responsibilities | Rust/TypeScript source documentation | Rustdoc and source |
| UI translation choices | [Localisation guide](editors/vscode/l10n/README.md) and translation bundles | UI and component prose |
| Local measurement and semantic review | [Performance lab](lab/perf/README.md), [audit tools](lab/audit/README.md) | Local developer evidence |
| Decisions and acceptance history | Issues, PRs, immutable Git history | Links from the relevant change |

Regenerate derived references with:

```sh
cargo run --locked -p tools -- documentation write
cargo run --locked -p tools -- documentation check
```

The documentation check rejects stale generated output, missing local links/anchors, missing
registered diagnostic sections, and invalid complete rule-language examples. It runs inside the
existing `policy` check in CI. Generated files/sections are projections, not independently edited
sources. When facts change, update their source and regenerate in the same PR.

Write stable responsibilities, usage, and design reasons by hand. Avoid reproducing field defaults,
limits, dependency lists, and current source line numbers. Examples involving local game paths
must remain illustrative; they are never executed by remote documentation checks.

Use issue/PR discussion for implementation decisions and acceptance records. Completed migration
plans and logs are available in [the merged RulesIr PR](https://github.com/danxiaogu520/ParadoxCode/pull/141)
and the [immutable migration snapshot](https://github.com/danxiaogu520/ParadoxCode/tree/2626b0a1341a72478f754c171edd53d4d8537b07/docs).
Keep temporary design plans and active proposals in the repository root;
[the Template proposal](TEMPLATE-PROPOSAL.md) is not a production contract.

Report security issues through [SECURITY.md](SECURITY.md).
