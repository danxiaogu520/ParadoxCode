# Diagnostic reference

This guide owns diagnostic explanations and remedies. The code/default-severity index below
is generated from `ide::DiagnosticCode`; descriptions explain behavior without defining a second registry.
The `codeDescription` link in your editor points at the matching section here.
VS Code's own transparent-encoding diagnostics are described alongside the server categories.

<!-- generated:diagnostic-index:start -->

| Code | Registry default severity |
| --- | --- |
| [SyntaxError](#syntaxerror) | Error |
| [AnalysisIncomplete](#analysisincomplete) | Information |
| [UnknownKey](#unknownkey) | Error |
| [UnknownLocalisationKey](#unknownlocalisationkey) | Warning |
| [AmbiguousDefinition](#ambiguousdefinition) | Warning |
| [InvalidValue](#invalidvalue) | Error |
| [UnknownTexturePath](#unknowntexturepath) | Error |
| [Cardinality](#cardinality) | Error |
| [WrongScope](#wrongscope) | Error |
| [DynamicDefinitionCycle](#dynamicdefinitioncycle) | Information |
| [InvalidDependency](#invaliddependency) | Error |
| [LogicalContainer](#logicalcontainer) | Warning |
| [ConstantCondition](#constantcondition) | Warning |
| [MissingLimit](#missinglimit) | Warning |
| [EmptyBlock](#emptyblock) | Warning |
| [OrphanElse](#orphanelse) | Error |
| [EmptyScopeContract](#emptyscopecontract) | Error |
| [ModifierScopeMismatch](#modifierscopemismatch) | Information |
| [LocalisationNotTranscoded](#localisationnottranscoded) | Warning |
| [LocalisationMixedEncoding](#localisationmixedencoding) | Error |
| [LocalisationEscapeRefused](#localisationescaperefused) | Error |
| [LocalisationUnencodableCodePoint](#localisationunencodablecodepoint) | Warning |
| [LocalisationBrokenEscapeSequence](#localisationbrokenescapesequence) | Warning |
| [ScriptLegacyEscapeVariant](#scriptlegacyescapevariant) | Hint |

Publication and position-specific severity can differ from registry defaults. The explanations below describe those cases.
<!-- generated:diagnostic-index:end -->

Localisation documents participate in parsing and workspace indexing but do not
publish LSP diagnostics. The transparent-encoding file provider can still report
read/save safety failures described in the localisation sections below.

All messages are written for the script in front of you: they name the
offending token, state the violated constraint on an `expected:` line, and
attach a `related:` location when another spot in the workspace explains the
finding. Internal rule provenance never appears in messages.

## SyntaxError

The script file could not be parsed: an unclosed block or string, a stray
delimiter, or an operator without a value.
The range covers the incomplete construct (for a missing value, the `key =`
that never received one).

## AnalysisIncomplete

Template analysis stopped at a resource or recovery boundary. The message names the unfinished
work; already proven errors remain visible. This information does not declare the script invalid
or claim that unchecked statements passed. Completion and hover retain the same coverage state.

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

A dependent field query may instead report a missing, duplicate, or invalid sibling
selector, such as `on_trigger` in a switch. Supply exactly one scalar selector that
names a compatible source field. Its branch values are checked only against that
selected field's value domain. A selector error is reported once per containing
block instead of repeating unrelated errors for each branch. Unbound Template
selectors defer this check until their arguments are available.

Queries with `call_args=none`, such as scalar trigger exports and switch selectors,
also require a selected Template helper to accept an empty argument set. This uses
the same required-parameter and active-branch inference as a direct invocation;
guarded optional parameters and literal fallback branches can still be valid.
Unresolved helper eligibility remains deferred instead of proving an invalid value
or offering an unverified callable completion.

## UnknownTexturePath

A `texturefile`/`texturefile1`–`3`, `alphamaskfile`, `effectfile`, or mesh
`file` value in a `.gfx` file does not resolve to any existing texture.
Resolution mirrors the engine: the spelling is normalized (case, forward and
back slashes, doubled separators), looked up across the workspace mod, the
game installation, and DLC pack directories pack-relative, then falls back
between the `.tga` and `.dds` spellings, and finally probes the game root
directly — so stale-but-working references stay silent and only truly
dangling paths are flagged. A did-you-mean correction suggests a sibling
file from the same directory when one is close. The sprite renders as
nothing in game, so the finding is an error; it can be muted per code with
`paradoxcode.diagnosticIgnoreCodes`.

## Cardinality

A required key or list entry is missing, or a key/list appears more often
than allowed. Block findings anchor on the key that owns the block
(`some_block = { ... }`); the file root, which owns no key, anchors on its
opening brace. Over-quota findings anchor on the entry past the quota
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

The shared Template query revisited the same binding and scope state when analysing
this definition with no supplied arguments. This is an informational, unresolved
analysis result. A potential name-only call cycle does not prove that every
invocation recurses: guards and argument changes can make concrete calls finite.
Inspect the actual call's AnalysisIncomplete evidence for its own bindings.

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

The server no longer publishes this code: raw localisation documents carry
no LSP diagnostics at all (see the note at the top), which retired the old
release-path check for readable CJK under `localisation/…/replace/…`.
Transcode release-tree files deliberately (**ParadoxCode: Encode File (EU4dll
Escape Form)**, available with `transparentEncoding` turned off) or keep
readable sources in the master tree outside `replace/`. The decoded-view
provider no longer attaches it either: a `pdcloc://` view over
readable bytes now carries the informational `LocalisationWillTranscodeOnSave`
hint instead, because quoted CJK is escape-encoded on the next save.

## LocalisationWillTranscodeOnSave

Emitted by the VS Code decoded-view provider, never by the server: the file
is plain readable text whose quoted strings contain CJK. Editing continues in
readable form, and the next save writes the scoped escaped form (comments and
code stay readable UTF-8 on disk).

## LocalisationMixedEncoding

An EU4dll escape marker sits outside every quoted string — in code or comment
position. Scoped transcoding only ever escapes inside strings, so such a
marker is damage: no transformation is applied and the file is shown as-is.
The error anchors on the marker itself. Move it into a string, repair it into
a triple, or delete it (or restore the file from paratranz, the single source
of truth for correct content).

## LocalisationBrokenEscapeSequence

An orphan escape marker inside a quoted string: a `0x10`–`0x13` marker whose
two payload bytes are missing or damaged. Decoding passes orphans through
untouched, so the character after the marker in a decoded view is wrong. Each
orphan is flagged individually; fix the triple or delete the stray marker.

## LocalisationUnencodableCodePoint

A character inside a quoted string of a configured script file cannot survive
the EU4 transcoder: code points in U+0100–U+0FFF are silently mangled into
triples (and back incorrectly), and code points beyond the BMP are destroyed.
Encoding the string would corrupt these characters, so they are flagged per
character. The 27 CP1252-mapped Latin letters (ä, é, ß, …) stay single bytes
and are allowed. Characters outside strings — comments and code — stay
verbatim UTF-8 under scoped saving and are never flagged.

## LocalisationEscapeRefused

Emitted by the VS Code extension's save gate, never by the server: the editor
buffer holds escape markers inside its quoted strings (encoding them again
would double-encode), or in-span characters the ecosystem cannot round-trip,
and the write was refused. The document on disk is untouched. Undo the paste
or decode the text first.

## ScriptLegacyEscapeVariant

The script file decodes correctly, but its escape triples belong to a
historical EU4dll escape-set variant rather than the canonical paratranz set:
a decode → re-encode round trip does not reproduce the bytes. Nothing is
broken today; the hint tells you that the next save through ParadoxCode
normalizes the triples to the canonical set (content is preserved either way,
and paratranz remains the judge of correctness).
