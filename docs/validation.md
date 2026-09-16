# Validation ownership

ParadoxCode separates fast developer feedback, merge gates, scheduled audits, release gates, and
manual acceptance. A check belongs to exactly one authority. Passing a broader-looking local
command never substitutes for a required remote check.

## Validation classes

| Class | Authority | Purpose | Examples |
| --- | --- | --- | --- |
| Developer feedback | Local checkout | Shorten the edit/debug loop | Targeted Rust tests, `npm run check`, rule bake, golden tests |
| Merge gate | GitHub Actions | Decide whether a pull request may enter `main` | The required `Conclusion` check |
| Scheduled audit | GitHub Actions | Detect changes outside a patch | Advisory databases, production npm audit, optimized benchmarks |
| Release gate | Tag workflow | Build and verify redistributable artifacts | Provenance, version agreement, five server packages, VSIX, checksums |
| Manual acceptance | Maintainer | Exercise interactions that automation does not represent well | Clean-profile VSIX install and Marketplace publication |

The full Vanilla sweep is developer feedback, not a merge or release gate. Europa Universalis IV
files are licensed local inputs and must not be copied to GitHub-hosted runners, Actions artifacts,
logs, pull requests, or Releases. Full sweep reports contain paths and source excerpts and therefore
remain under the ignored local `performance-results/` directory.

## Validation asset inventory

| Asset | Trigger | Responsibility | Blocking authority |
| --- | --- | --- | --- |
| `cargo tools gates <group>` | Developer choice | Deterministic feedback for one technical area | None outside the local command |
| `editors/vscode/scripts/sweep.mjs` | Risk-driven local run | Licensed-data diagnostic and performance exploration | None outside the local command |
| `.github/workflows/ci.yml` | Pull request and `main` push | Clean-checkout, cross-platform, policy, packaging, and deterministic dependency validation | `Conclusion` blocks merge |
| `.github/workflows/security.yml` | Weekly or manual | Refresh advisory-backed Rust and production npm audits | Audit signal; maintainer triage |
| `.github/workflows/performance.yml` | Weekly or manual | Run optimized repository-owned benchmarks | Audit signal; maintainer triage |
| `.github/workflows/release.yml` | Protected version tag or manual retry | Verify provenance; build, verify, and publish eleven redistributable assets | Blocks publication only |
| Clean-profile VS Code test and Marketplace upload | After GitHub Release | Human acceptance of installation, startup, and distribution | Blocks Marketplace publication |

The retired remote sweep workflow, self-hosted runner guard/runbook, committed fingerprint baseline,
and diagnostics history have no owner because they are no longer project assets. Reintroducing any
of them requires an explicit governance decision and a redistribution-safe design.

## Normal change lifecycle

### While developing

Run the narrowest check that proves the current change. Typical examples are:

```bash
cargo test -p ide completion
cargo test -p pdc format_command
npm --prefix editors/vscode run check
```

Formatting and focused tests should pass before a commit. A local commit does not require the full
workspace suite, cross-platform builds, network vulnerability scans, benchmarks, or a Vanilla
sweep. Keep refactors, behavior changes, and release/process changes separately reviewable.

### Before opening or updating a pull request

Run every affected technical group. For most Rust changes this starts with `core-fast`:

| Changed area | Local command |
| --- | --- |
| Rust behavior or public Rust API | `cargo tools gates core-fast` |
| VS Code extension or webview | `cargo tools gates vscode` |
| Repository policy, manifests, workflows, or docs named by policy | `cargo tools gates policy` |
| Embedded rules or distribution contract | `cargo tools gates artifact` |
| Fuzz manifests or targets | `cargo tools gates fuzz` |
| Performance-sensitive implementation | Targeted benchmark or `cargo tools gates perf` |

`cargo tools gates` (the `all` local group) runs every default deterministic local group. The
optimized `perf` group remains opt-in. `all` is useful before a large pull request but is not
required for every commit and does not mean "ready to release." Security audits, cross-platform
coverage, nightly fuzz smoke, and publishing remain remote responsibilities.

The pull-request description records the exact commands run and residual risk. Do not check a
generic "all local gates" box without listing evidence.

### Pull request and main

CI runs on every pull request and every push to `main`. It owns clean-checkout formatting, clippy,
rustdoc, Linux and Windows tests, MSRV, the Windows optimized build, VS Code contracts and package,
nightly fuzz smoke, repository/release policy, deterministic dependency policy, and typo checks.
The aggregate `Conclusion` job is the only required merge status.

`main` must remain releasable. A failed post-merge `Conclusion` is treated as a regression and gets
a focused repair pull request; it is never bypassed to prepare a release.

### Release preparation and tag

A release-preparation pull request updates the workspace and VS Code versions, lockfiles,
`CHANGELOG.md`, and any current user documentation. It passes the ordinary `Conclusion` check.
No local report, fingerprint, benchmark, or game installation grants release eligibility.

After merge, an annotated protected tag starts the Release workflow. The workflow verifies tag
provenance and the successful `Conclusion` for the tagged commit, builds five native server
archives and their checksum sidecars, builds the VSIX, verifies the eleven-file payload, creates a
draft, compares the uploaded names with the verified payload, and publishes it once. Published tags
and assets are immutable.

Marketplace upload and a clean-profile installation are manual acceptance steps after the GitHub
Release succeeds.

## Local Vanilla sweep

Use the sweep when a change can alter workspace-wide diagnostics, symbolization, indexing, or rule
interpretation. It is normally expected for changes to:

- `rules/eu4/` or the rule compiler/matcher;
- diagnostic emission, resolution, scopes, dynamic definitions, or file classification;
- parsers or HIR lowering in ways that can affect existing game files;
- Vanilla cache construction or workspace-wide query behavior.

It is normally unnecessary for documentation, CI-only changes, isolated UI work, formatting, or a
behavior-preserving refactor already covered by focused tests.

Build the exact binary first and pass it explicitly:

```bash
cargo build --locked --release -p pdc --bin paradoxcode
node editors/vscode/scripts/sweep.mjs \
  --server target/release/paradoxcode.exe \
  --vanilla-source "C:/path/to/Europa Universalis IV" \
  --label local-change
```

The sweep refuses implicit server discovery and fails if the selected binary's active embedded
rules hash differs from the checkout manifest. Its summary records the binary SHA-256, version,
Git commit, and dirty-worktree state. `--previous <summary.json>` reports diagnostic and performance
drift but does not turn that drift into a repository or release gate.

Do not commit or upload the generated reports. Record only a short human conclusion in the pull
request when relevant, without Vanilla paths, source excerpts, or report attachments. Every defect
found by a sweep should be reduced to the smallest repository-owned regression fixture that can run
in remote CI.

## Scheduled audits

The weekly Security workflow refreshes Rust advisories and production npm vulnerability data. The
weekly Performance workflow runs optimized benchmarks. These workflows report repository health;
because their inputs can change without a commit, they do not block an unrelated pull request.
Dependency-changing pull requests should still examine their current results before merge, and a
release must not knowingly ship an exploitable high-severity advisory.

## Failure ownership

- A local check failure belongs to the developer and never changes remote state.
- A PR `Conclusion` failure blocks merge until the PR is repaired.
- A scheduled audit failure is triaged by the maintainer, who opens or updates a
  maintenance/security issue when action is required.
- A release workflow failure leaves the tag unpublished. Repair transient infrastructure and rerun
  the same unpublished tag; repair product defects through a pull request and a new patch tag.
- A manual acceptance failure pauses Marketplace publication and is recorded as an issue with the
  affected GitHub Release.
