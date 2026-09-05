# Diagnostic reference

Every diagnostic ParadoxCode publishes carries one of the codes below. The
`codeDescription` link in your editor points at the matching section here.

All messages are written for the script in front of you: they name the
offending token, state the violated constraint on an `expected:` line, and
attach a `related:` location when another spot in the workspace explains the
finding. Internal rule provenance never appears in messages.

## SyntaxError

The file could not be parsed: an unclosed block or string, a stray
delimiter, an operator without a value, or a malformed localisation entry.
The range covers the incomplete construct (for a missing value, the `key =`
that never received one).

## UnknownKey

The key is not valid where it appears. The message names the container
(`unknown key 'x' in a 'trigger' block`) and offers a did-you-mean
correction, including a rename quick fix, when exactly one sibling key is
close. Conditions inside effect blocks and effects inside trigger blocks
are called out explicitly.

## UnknownLocalisationKey

A localisation reference does not resolve to any key in the merged
localisation of the workspace. This is a warning because the game renders
the raw key spelling instead of failing. A did-you-mean suggestion is
included when a close key exists.

## AmbiguousDefinition

Two definitions share one name (for example two events with the same id, or
two sprites with the same name). Resolution follows later-wins: the last
definition is the effective one, and the diagnostic points at it with a
related location on the earlier definition it shadows. Rename one of the
two.

## InvalidValue

The value does not satisfy the constraint of its key: an enum member that
does not exist, a number outside its bounds, an unknown scope target, an
unrecognised list member. The `expected:` line states the constraint (for
example `a whole number between 0 and 255` or `one of 'yes' or 'no'`), and
a did-you-mean quick fix is attached when exactly one accepted value is
close. Usage of a declaration the game data marks deprecated renders with
strikethrough.

## Cardinality

A required key or list entry is missing, or a key/list appears more often
than allowed. Block findings anchor on the opening brace of the block that
misses the entry; over-quota findings anchor on the entry past the quota
and render as unnecessary (dimmed) code, since removing that entry is the
fix.

## WrongScope

The key is valid, but not in the scope where it appears (for example a
province-only effect used from country scope). The `expected:` line lists
the scopes the key works from. Move the call into an appropriate scope
block, or use a scope command to change the current scope first.

## InvalidScopeCommand

A `scope = <target>` command names a target that is not a recognised scope
command. A did-you-mean suggestion is included for close scope names.

## DynamicDefinitionCycle

Scripted triggers/effects form an invocation cycle. The message lists the
cycle path; break it by removing one edge.

## InvalidDependency

A mission-tree dependency is structurally illegal. Legal placement: `A
requires B` holds when B sits directly above A in the same slot, or
immediately left of A in the row above. Variants: a required mission that
does not exist anywhere in the workspace (missing, error), a placement that
violates the rule (position, warning), a cycle of mutual requirements
(cycle, error).

## LogicalContainer

A logical block (`OR`, `AND`, `NOT`, ...) is used where a plain trigger list
is expected, or the reverse. Move the logical block one level in or out.

## ConstantCondition

A condition is always true or always false at its position (for example
testing a value the enclosing block already fixed). The flagged test is
redundant and renders as unnecessary code.

## MissingLimit

An `if`/`else` chain or similar construct lost its `limit` block. The
diagnostic anchors on the block that is missing it.

## EmptyBlock

A block that must contain content is empty. The range covers the empty
braces.

## OrphanElse

`else` or `else_if` appears without a preceding `if`. Either add the `if`
or dedent the branch into one.

## EmptyScopeContract

A scripted trigger/effect declares an entry scope contract but its body
never establishes that scope. The contract can never be satisfied; fix the
body or drop the contract.

## ModifierScopeMismatch

A modifier is applied in a scope that cannot carry it (a province modifier
on a country, for example). The `expected:` line lists the valid scopes.

## DynamicCallScopeMismatch

A scripted trigger/effect requires an entry scope that the call site does
not provide. Enter the required scope before the call.

# Migration from pre-refactor codes

The old 22-code table was consolidated to 18. Old codes are gone: they are
not aliases, and configurations that name them fail fast at initialize.

| Old code | New behaviour |
| --- | --- |
| `UnknownSymbol` | localisation references became `UnknownLocalisationKey` (warning); every other unresolved symbol is `InvalidValue` |
| `AmbiguousSymbol` | never diagnosed at use sites; the definition site gets `AmbiguousDefinition` (warning) and resolution is later-wins |
| `UnknownScope` | folded into `InvalidValue` (unknown target) and `WrongScope` (known target, wrong scope) |
| `InvalidTarget` | `InvalidValue` or `WrongScope`, by whether the target resolves |
| `TargetWrongScope` | `WrongScope` |
| `AnalysisIncomplete` | removed; analysis is bounded and always terminates |
| `UnknownBareValue` | `InvalidValue` on the exact value token with an `expected:` constraint |
| `RuleWrongScope` | renamed to `WrongScope` |

New codes: `UnknownLocalisationKey`, `AmbiguousDefinition`,
`InvalidDependency`.
