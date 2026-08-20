## Summary

<!-- What does this change do, and why? Keep it short; the commit message is the long story. -->

## Scope

<!-- Which crate/component does this touch? Keep changes focused and separately reviewable. -->

- [ ] This change is scoped to the files it needs and does not touch unrelated code.

## Testing

<!--
List exactly what you ran and its result. Examples:
- `bash scripts/check-quality-gates.sh core` — passed
- `cargo test -p pdx-analysis rename` — passed
-->

- [ ] Local quality gates passed (the pre-commit hook runs them automatically).
- [ ] New behavior is covered by tests or fixtures in this PR.
- [ ] CI is expected to pass for the touched groups (core / grammars / zed / vscode / release).

## Design and invariants

<!--
Call out anything reviewers should verify:
- Does this respect the architecture boundaries in AGENTS.md (no EU4 logic leaking into generic layers)?
- Any behavior changes, migration notes, or follow-up work?
- Any residual risks or checks that could not be run locally?
-->

- [ ] No design boundaries were crossed without a documented reason in this PR.

## Release notes

<!-- Whether this is a user-visible change (feat/fix) that should be added to CHANGELOG.md. -->
- [ ] User-visible change; CHANGELOG.md updated in this PR.
- [ ] Internal change; no CHANGELOG entry required.