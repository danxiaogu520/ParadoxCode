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
does not exist, a number outside its bounds, an unknown scope or
scope-command target, an unrecognised list member. The `expected:` line
states the constraint (for example `a whole number between 0 and 255` or
`one of 'yes' or 'no'`), and a did-you-mean quick fix is attached when
exactly one accepted value is close. Usage of a declaration the game data
marks deprecated renders with strikethrough.

## Cardinality

A required key or list entry is missing, or a key/list appears more often
than allowed. Block findings anchor on the opening brace of the block that
misses the entry; over-quota findings anchor on the entry past the quota
and render as unnecessary (dimmed) code, since removing that entry is the
fix.

## WrongScope

The key is valid, but not in the scope where it appears (for example a
province-only effect used from country scope, or a scripted trigger/effect
called from a scope its entry contract rejects). The `expected:` line lists
the scopes the key works from; for dynamic definitions it lists the entry
contract. Move the call into an appropriate scope block, or use a scope
command to change the current scope first.

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

## LocalisationNotTranscoded

The file sits on the game read path (`localisation/…/replace/…`) but contains
readable CJK text: without the escape triples the EU4dll patch expects, the
game renders mojibake. Transcode the file (`ParadoxCode: Transcode localisation
file`) or keep readable sources in the master tree outside `replace/`. Files
under other `localisation/` directories are master copies by convention and
are never flagged. A decoded `pdxloc://` view suppresses this code: readable
text is the point of that view, and saving re-encodes the shard behind it.

## LocalisationMixedEncoding

The file mixes readable CJK with EU4dll escape triples, or carries stray
escape markers too sparse to be a transcoded file. Neither encode nor decode
is a safe transformation, so none is applied — resolve the file by hand (or
restore it from paratranz, the single source of truth for correct content).
The error anchors on the first marker or CJK character, whichever comes first.

## LocalisationBrokenEscapeSequence

The file is a transcoded file overall (three or more intact escape triples, no
readable CJK) but contains orphan escape markers: a `0x10`–`0x13` marker whose
two payload bytes are missing or damaged. Decoding passes orphans through
untouched, so the character after the marker in a decoded view is wrong. Each
orphan is flagged individually; fix the triple or delete the stray marker.

## LocalisationUnencodableCodePoint

A character in the file cannot survive the EU4 transcoder: code points in
U+0100–U+0FFF are silently mangled into triples (and back incorrectly), and
code points beyond the BMP are destroyed. Re-transcoding the file would
corrupt these characters, so they are flagged per character. The check is
profile-aware: in script (`.txt`) files the 27 CP1252-mapped Latin letters
(ä, é, ß, …) stay single bytes and are allowed; in localisation (`.yml`) files
they are refused.

## LocalisationEscapeRefused

Emitted by the VS Code extension's save gate, never by the server: you edited
a file that classifies as escaped (or mixed) in a context where the save would
double-encode or corrupt it, and the write was refused. The document on disk
is untouched. Decode the file first (open the decoded view) or fix the mixed
content by hand.

## ScriptLegacyEscapeVariant

The script file decodes correctly, but its escape triples belong to a
historical EU4dll escape-set variant rather than the canonical paratranz set:
a decode → re-encode round trip does not reproduce the bytes. Nothing is
broken today; the hint tells you that the next save through ParadoxCode
normalizes the triples to the canonical set (content is preserved either way,
and paratranz remains the judge of correctness).

# Migration from pre-refactor codes

The old 22-code table was consolidated to 16. Old codes are gone: they are
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
| `InvalidScopeCommand` | folded into `InvalidValue`; the message still reads "invalid scope command target ..." |
| `DynamicCallScopeMismatch` | folded into `WrongScope`; the message still names the entry contract |

New codes: `UnknownLocalisationKey`, `AmbiguousDefinition`,
`InvalidDependency`.

The EU4dll transcode pipeline later added `LocalisationNotTranscoded`,
`LocalisationMixedEncoding`, `LocalisationBrokenEscapeSequence`,
`LocalisationUnencodableCodePoint`, `LocalisationEscapeRefused` (extension
save gate only), and `ScriptLegacyEscapeVariant`.
