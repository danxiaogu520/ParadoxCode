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
- `CONTRIBUTING.md#validation` assigns every local check, remote gate, audit, and manual acceptance step to
  one lifecycle owner.
- Private security advisories are the only tracker for undisclosed vulnerabilities.

## Changes and decisions

All repository changes use pull requests, including maintainer changes. The `Conclusion` status
check must pass before squash merge. Non-trivial changes should have an issue that records the
problem, chosen approach, and any deferred work. Architectural decisions may be captured in that
issue or in an active proposal in the repository root when they need code review.
Completed decision and acceptance records stay in issues, PRs, and immutable Git history.

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

Version tags and published releases are immutable. The release workflow uses the repository's
short-lived `GITHUB_TOKEN`; Marketplace credentials are kept outside the repository and used only
for the audited manual publishing step. Maintainers follow `RELEASING.md` for every release.
Candidate preparation has read-only GitHub permissions and creates no formal tag. Complete CI
receipts may be reused across squash commits with identical tested trees and check definitions;
the required main check validates that evidence. Formal tags are created only after a candidate's
complete payload, remote staging uploads and publication prerequisites pass. Promotion publishes
the same artifact bytes; formal tag reservation and immutable publication are the final mutations.

Licensed game installations are local development inputs, not repository or CI assets. Vanilla
files, excerpts, diagnostic reports, and sweep output must not be uploaded to GitHub Actions,
pull requests, issues, or Releases. The local Vanilla sweep informs development but never grants
merge or release authority; repository-owned regression fixtures are the durable CI evidence.

Account recovery codes, Marketplace ownership recovery, and any future signing keys must be held
offline in a maintainer-controlled credential vault. They must never be committed, placed in issue
text, or exposed to a repository workflow.

The [PR autosync workflow](.github/workflows/pr-autosync.yml) separately uses the repository-scoped
`AUTOMERGE_TOKEN`. The maintainer owns its least-privilege permissions, expiry/renewal, and recovery.
Its failure pauses branch refresh/auto-merge; repair the credential and rerun autosync, keeping the
normal required checks intact.

## Continuity

Until a second maintainer is appointed, the current maintainer keeps repository and publisher
recovery instructions current. A future backup maintainer should be granted the minimum GitHub and
Marketplace roles needed to recover releases, then exercise the recovery procedure before being
treated as an operational backup.
