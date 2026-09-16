## Summary

<!-- What does this change do, and why? Keep it short; the commit message is the long story. -->

## Scope

<!-- Which crate/component does this touch? Keep changes focused and separately reviewable. -->

- [ ] This change is scoped to the files it needs and does not touch unrelated code.

## Testing

<!--
List exactly what you ran and its result; do not substitute a generic "all gates" claim. Examples:
- `cargo tools gates core` — passed
- `cargo test -p ide rename` — passed
- Local Vanilla sweep — not applicable (no diagnostic/rule/index behavior changed)
-->

- [ ] The affected local groups from `docs/validation.md` passed.
- [ ] New behavior is covered by tests or fixtures in this PR.
- [ ] No licensed game files, excerpts, sweep reports, or machine-local paths are included.

<!--
If this changes rules, diagnostics, parsing/HIR semantics, indexing, or workspace-wide queries,
state whether a local Vanilla sweep was run and summarize only the conclusion. Never attach the
game-derived report.
-->

## Design and invariants

<!--
Call out anything reviewers should verify:
- Does this respect the architecture boundaries in README.md (no EU4 logic leaking into generic layers)?
- Any behavior changes, migration notes, or follow-up work?
- Any residual risks or checks that could not be run locally?
-->

- [ ] No design boundaries were crossed without a documented reason in this PR.

## Release notes

<!-- Whether this is a user-visible change (feat/fix) that should be added to CHANGELOG.md. -->
- [ ] User-visible change; CHANGELOG.md updated in this PR.
- [ ] Internal change; no CHANGELOG entry required.
