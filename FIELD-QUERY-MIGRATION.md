# EU4 field-query migration inventory

Status: phase-one implementation and remaining decision inventory. This is a migration
record, not a second rule authority. Rule-language semantics belong to
[the language guide](crates/rules/LANGUAGE.md); EU4 declarations remain in
[rules/eu4](rules/eu4). Audit baseline: `bbb8545`, 2026-10-05.

## Migrated consumers

| Source / schema | Position | Declared domain | Boundary |
| --- | --- | --- | --- |
| `core/trigger.json`, `trigger__has_global_modifier_value` | `which` value | `keysof<modifier,scope_accepts=country,shape=scalar>` | Country modifier declarations, including their dynamic patterns |
| `core/trigger.json`, `trigger__has_local_modifier_value` | `which` value | `keysof<modifier,scope_accepts=province,shape=scalar>` | Province modifier declarations |
| `core/effect.json`, `effect__export_to_variable` | `value`, `trigger_value:` hole | Scalar trigger keys with an int/float/bool branch, explicit `trigger_value_export` capability, and no-argument call eligibility | Keeps 522 declared fixed names and adds workspace-defined scalar scripted triggers; numeric operands alone do not establish exportability |
| `core/trigger.json`, `trigger__variable_arithmetic_trigger__export_to_variable` | Same | Same | Nested variable-arithmetic consumer is migrated too |
| `common/diplomatic_actions_new.json`, `new_diplomatic_action_body__ai_acceptance__add_entry__export_to_variable` | Same | Same | Preserves the existing additional `int` seed alternative |
| All three general export schemas above | `value`, `modifier:` hole | `keysof<modifier,shape=scalar>` | Validates declared modifier spelling; effective `who`/current-scope filtering is deferred |
| `core/effect.json`, `effect__trigger_switch` | `on_trigger` value and case key | Scalar trigger key projection and sibling-selected scalar value projection, both requiring no-argument call eligibility | Uses the shared field-query mechanism; switch control flow remains a separate field attribute |

Paths in this table are relative to `rules/eu4/`.
All three export trigger holes use this query:

```text
keysof<trigger,shape=scalar,value_kind_any=(int|float|bool),capability=trigger_value_export,call_args=none>
```

The old `numeric_or_bool_trigger` enum is removed. `export_to_variable_data` remains:
its game event-scope values are not a projection of trigger field names.
`variable:{scalar}` retains its previous behavior.

## Trigger-domain audit and historical reconciliation

At baseline `bbb8545`, the previous enum contains 524 distinct spellings. The
`trigger` schema has 962 exact field names, 788 with at least one scalar overload.
Selecting exact scalar fields whose value expression has **any** primitive `int`,
`float`, or `bool` branch also produces 524 names. Ranges are included, and the
primitive need not be the first or only union branch. Equal counts conceal a real
mismatch: the intersection contains 522 names.

| Difference | Exact spelling | Current source evidence |
| --- | --- | --- |
| Previous enum only | `highest_supply_limit_in_area` | No exact field in current rules. It can resolve through the scalar `ref<scripted_trigger>` pattern when a workspace supplies a definition. No actual definition of this name is present in the tracked repository. |
| Previous enum only | `was_never_end_game_tag_trigger` | No current exact field. The repository-owned scripted-helper fixture defines this name and verifies dynamic definition navigation. |
| Primitive-kind query only | `advisor_exists` | Its operand is `ref<advisor_id> \| int[0..]`; a numeric identifier does not establish exportability. |
| Primitive-kind query only | `is_advisor_employed` | Same advisor-ID operand union and same distinction. |

### What the complete Git history establishes

The checkout is non-shallow. Searches used `git log --all -S` and `-G`, `git grep`
at the relevant revisions, and direct inspection of deleted and renamed source files.
Before [RulesIr migration PR #141](https://github.com/danxiaogu520/ParadoxCode/pull/141),
commit `e771357ec848171aed60ae88750c446e140f9303` had both legacy aliases and exact
semantic rows:

- [`was_never_end_game_tag_trigger`](https://github.com/danxiaogu520/ParadoxCode/blob/e771357ec848171aed60ae88750c446e140f9303/rules/eu4/semantic/contexts/trigger.json#L12166-L12192):
  scalar `bool`, country scope, with a description about never having been an end-game tag.
- [`highest_supply_limit_in_area`](https://github.com/danxiaogu520/ParadoxCode/blob/e771357ec848171aed60ae88750c446e140f9303/rules/eu4/semantic/contexts/trigger.json#L16739-L16763):
  scalar `bool`, province scope, no accompanying description.

Both already existed in the first-party JSON sources at commit
`4f3fdce741dbbb7e5b4efc3bb065b76c2fb387e6` (2026-07-22). PR #141 deletes the old
catalog and semantic files and includes both spellings in its new export enum;
neither becomes a fixed field in the new `trigger` schema.

That proves a change of representation. It does **not** by itself prove lost game
support. The [same PR's validation record](https://github.com/danxiaogu520/ParadoxCode/blob/2626b0a1341a72478f754c171edd53d4d8537b07/docs/phase5-validation.md#L514-L518)
explicitly records fixing 18 Vanilla scripted functions shadowed by 68 exact-field
declarations. Its [scripted-helper regression](https://github.com/danxiaogu520/ParadoxCode/blob/2626b0a1341a72478f754c171edd53d4d8537b07/crates/ide/src/tests/ir.rs#L915-L959)
defines `was_never_end_game_tag_trigger` in `common/scripted_triggers/helper.txt`,
requires navigation to that definition, and rejects unknown-key/value/scope diagnostics.
This is strong evidence of intentional dynamic treatment for that name. There is no
name-specific tracked test or audit explanation for `highest_supply_limit_in_area`,
so its historical classification remains unconfirmed here. No fixed declaration is
restored merely because an old alias existed.

The [Vanilla acceptance record](https://github.com/danxiaogu520/ParadoxCode/blob/2626b0a1341a72478f754c171edd53d4d8537b07/docs/phase5-validation.md#L596-L618)
used the EU4 1.37.5 text corpus, indexing 8,681 files and diagnosing 8,670. The final
record compares complete definition/reference multisets and error identities.
Scripted-trigger files supply definitions through `def<scripted_trigger>`; when those
definitions are loaded, deleting an incorrect fixed alias can preserve validation
while restoring navigation through the dynamic pattern. Exact fields otherwise shadow
that pattern. Therefore passing Vanilla tests is consistent with this representation
change. Per-name raw reports and the licensed corpus were deliberately untracked and
are unavailable in this checkout; no claim about the missing name's actual game
definition is inferred from aggregate passing counts.

### What the old export evaluator actually accepted

The old data used a generic `typed_prefix` with context `trigger` and operand family
`numeric_or_bool`. However, its [runtime implementation](https://github.com/danxiaogu520/ParadoxCode/blob/e771357ec848171aed60ae88750c446e140f9303/crates/ide/src/semantic.rs#L1839-L1884)
looked up **exact** top-level rules only and accepted primitive Int/Float/Bool matchers.
Its [completion implementation](https://github.com/danxiaogu520/ParadoxCode/blob/e771357ec848171aed60ae88750c446e140f9303/crates/ide/src/completion/candidates.rs#L76-L105)
also required `KeyMatcher::Exact`. Dynamic scripted-trigger patterns were excluded,
even though a scalar bool scripted pattern was present in the old rules.

An independent set comparison of those old top-level exact primitive rows yields
524 names, exactly equal to the post-migration `numeric_or_bool_trigger` enum.
Thus the enum preserved the old export spelling set; it was not evidence that the
old evaluator already supported all dynamic scripted triggers. The advisor commands
previously took only `advisor_id`; PR #141 [explicitly added nonnegative numeric
advisor IDs](https://github.com/danxiaogu520/ParadoxCode/blob/2626b0a1341a72478f754c171edd53d4d8537b07/docs/phase5-validation.md#L575-L578).
A broad modern primitive-kind query would include them as an unintended consequence
of that operand improvement.

### Implemented boundary

The user's explicit requirement is to support scripted-trigger exports. The source
therefore applies `capabilities: ["trigger_value_export"]` to:

- The 522 existing fixed names shared by the old export domain and current trigger
  fields, in both `schemas.trigger.fields` and the existing `keys__trigger` mixin.
- The scalar `ref<scripted_trigger>` / `bool` pattern in `schemas.trigger.patterns`.
  The separate block pattern receives no capability.

This keeps exportability beside the declaration that owns its name, scope and value
matcher. It removes the parallel 524-name enum and uses no compatibility literals or
second registry. Fixed metadata records previous export acceptance, not independent
engine verification. Dynamic scripted support is the explicitly requested extension.
Both historically special spellings now require actual workspace definitions like
any other scripted trigger; neither is an unconditional grandfathered export name.
Unknown names and the advisor-ID commands remain excluded.

The trigger schema has eight scalar dynamic pattern families. Only
`ref<scripted_trigger>` is added to exports. The other seven remain untagged:
`ref<advisor_type>`, `ref<building>`, `ref<idea_group>`, `ref<institution>`,
`ref<religion>`, `ref<subject_type>`, and `ref<trade_good>`. Their primitive operands
are not evidence for exportability. Ordinary scalar trigger queries, including
switch selection, still use their declared domains where applicable.

## No-argument call eligibility

Export trigger names and switch selectors/case domains opt into `call_args=none`.
This is a separate constraint from `shape=scalar`: shape selects the source field's
scalar overload, while eligibility checks the selected Template definition itself.
The shared direct-invocation argument analysis rejects definitions requiring a
parameter block, preserves zero-argument and optional/default-branch helpers, and
keeps unavailable or incomplete analysis deferred. Ordinary reference-name queries
without this filter remain unchanged. No game-specific helper-name list is used.

## Modifier coverage

The source modifier schema has 700 exact scalar fields: 604 country and 96 province.
All seven dynamic modifier families are country-scoped and retained as matchers,
not flattened into a static list:

1. `monthly_{ref<government_mechanic_power>}`
2. `{ref<estate strip_prefix estate_>}_influence_modifier`
3. `{ref<estate strip_prefix estate_>}_loyalty_modifier`
4. `{ref<estate strip_prefix estate_>}_privilege_slots`
5. `{ref<faction>}_influence`
6. `{ref<government_mechanic_power>}_gain_modifier`
7. `ref<estate_modifier>`

Country queries retain all seven; province queries correctly exclude them according
to their existing source scopes. Unscoped export queries retain both exact scope
families and all seven dynamic families. Reference resolution and affix stripping
continue to use the symbol facts and matcher definitions that own those behaviors.
Unknown suffixes must not be admitted by a replacement `scalar` escape hatch.

## Effective export scope: explicit remaining decision

The existing general effect export `who` declaration permits country or province
scope and documents the distinction. The VAT export permits country/tag, while
the diplomatic-action form permits country. This migration preserves these existing
`who` and `with` grammars; no missing province alternative is guessed into them.

The [Paradox development diary mirrored on Steam](https://store.steampowered.com/news/posts/?appids=236850&enddate=1493118163&feed=steam_community_announcements)
demonstrates `who = FROM` choosing a VAT export source, with equivalent pseudocode
reading manpower from `FROM` rather than `THIS`. That supports treating `who` as
semantically relevant. It does not independently establish the full modifier-specific
country/province, absent-`who`, or unresolved-scope rules. The EU4 wiki Variables and
Effects pages were unavailable during this audit, and no licensed game corpus was
available in this checkout.

A country-only `modifier:` query would therefore reject declared province modifiers
without justification. The implemented unscoped projection validates the name while
leaving effective-scope diagnostics for a later evidence-backed change. That change
needs an authoritative modifier export specification or game-backed tests for
explicit country, explicit province, omitted `who`, and unresolved/dynamic `who`, plus
a generic query context selector that can represent those outcomes. It must not use
a spelling-specific Rust exception.

## Inspected but not migrated

- `common/peace_treaties.json`, `peace_treaty_body__ai_weight__export_to_variable`:
  its value is intentionally `int` and its variable name is fixed to `ai_value`;
  there is no generic `trigger_value:` or `modifier:` domain to replace.
- Four `trigger_switch` control declarations in the effect mixin, effect schema,
  and history schemas share `effect__trigger_switch` as their body. They keep their
  control-flow metadata; field queries do not replace switch execution semantics.
- Government-power consumers (`add_government_power`,
  `add_government_power_scaled_to_seats`, `freeze_government_power`,
  `set_government_power`, `unfreeze_government_power`, `government_power_frozen`,
  and `has_government_power`) already use `ref<government_mechanic_power>`.
  Restricting a power to a sibling-selected mechanic requires a new ownership
  relation and authoritative data. It is outside this phase.
- Enum aliases and unions remain ordinary expressions. No enum alias system,
  secondary modifier enum, or separate export registry is introduced.

## Validation responsibilities

The source audit establishes that the old exact-rule export domain equals the old
524-name enum, and that both current fixed capability name sets equal their 522-name
intersection with the current trigger schema. The changed domain deliberately replaces
the two unconditional legacy spellings with actual dynamic scripted definitions.
Downstream regressions should cover defined and undefined scripted triggers, both
advisor-ID exclusions, all three export consumers, country/province modifier selectors,
every dynamic modifier family, unknown dynamic members, and the shared switch path. Compiler, HIR, and IDE behavior tests own
runtime proof; a source-text count alone does not establish it.

A recursive raw-JSON audit confirms canonical formatting and only the intended
capability, value-domain, switch-domain, documentation and enum-deletion changes.
The modifier declarations, peace-treaty integer-only export, `who`/`with` grammars,
and switch control metadata are unchanged. The fixed capability set occurs on 524
scalar overloads in each of the schema and mixin, representing 522 distinct names.

Run the rule check and formatter, relevant compiler/HIR/IDE tests, documentation
check, and the applicable local gates described in [CONTRIBUTING.md](CONTRIBUTING.md).
The implementation report records actual command outcomes. A full Vanilla sweep
requires a separately available licensed corpus and is not implied by these fixtures.

The rule formatter also normalized the pre-existing key order in `core/traits.json`; its trait declarations are unchanged.
