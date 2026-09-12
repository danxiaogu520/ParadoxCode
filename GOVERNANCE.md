# Project governance

ParadoxCode is currently a single-maintainer open-source project. Governance is intentionally
lightweight, but repository rules enforce the quality and release invariants described here.

## Sources of truth

- GitHub issues hold accepted bugs, feature work, maintenance tasks, and decisions that need a
  durable record.
- Milestones communicate the intended scope of the next release; they are plans, not delivery-date
  commitments.
- `main` is the only integration branch and must remain releasable.
- `CHANGELOG.md` is the user-visible release history, and `RELEASING.md` is the release runbook.
- Private security advisories are the only tracker for undisclosed vulnerabilities.

## Changes and decisions

All repository changes use pull requests, including maintainer changes. The `Conclusion` status
check must pass before squash merge. Non-trivial changes should have an issue that records the
problem, chosen approach, and any deferred work. Architectural decisions may be captured in that
issue or in a focused document under `docs/` when they need to live beside the code.

CODEOWNERS requests the current maintainer for review. Adding another maintainer should split
ownership by subsystem instead of granting every path by default.

## Emergency bypass

The repository administrator may bypass a ruleset only to recover repository access or repair a
broken required check that prevents all pull requests from merging. A bypass must not be used to
ship a failing change or skip a release gate. Create an issue immediately afterward containing:

- the affected commit or tag;
- the reason normal review could not proceed;
- the checks run before and after the bypass; and
- the follow-up that prevents recurrence.

## Releases and credentials

Version tags and published releases are immutable. The workflow uses only the repository's
short-lived `GITHUB_TOKEN`; Marketplace credentials are kept outside the repository and used only
for the audited manual publishing step. Maintainers follow `RELEASING.md` and the runner recovery
runbook for every release.

Account recovery codes, Marketplace ownership recovery, and any future signing keys must be held
offline in a maintainer-controlled credential vault. They must never be committed, placed in issue
text, or stored on the self-hosted runner as repository files.

## Continuity

Until a second maintainer is appointed, the current maintainer keeps the runner reconstruction and
publisher recovery instructions current. A future backup maintainer should be granted the minimum
GitHub and Marketplace roles needed to recover releases, then exercise the recovery runbook before
being treated as an operational backup.
