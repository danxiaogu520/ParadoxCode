# Template analysis

A `Template` retains its lossless definition body and an immutable arena program.
The program records property dispatch, scalar/script consumption, presence guards,
and recoverable syntax. Rules call this capability `Template`; scalar literal
matching uses `Pattern`. Quoted text remains an ordinary scalar until an actual
Template use consumes it as script.

`BindingInputs` separates supplied keys from known logical text. A supplied editing
hole activates a presence guard; a quoted empty string has known empty text.
Parameter forwarding preserves both pieces of information. The parser's quote
codec exposes a decoded string and byte map without allocating another CST.

The interpreter in [template.rs](src/template.rs) uses an explicit task stack.
Its state includes the definition, binding fragments, supplied keys, scope
registers and query mode. Repeated concrete states and exhausted budgets remain
in `AnalysisCoverage`; they do not prove an EU4 runtime failure. Independent
rejection evidence survives an unfinished traversal. `is_complete()` describes
traversal coverage, while `is_known()` also requires resolved inputs.

[checking.rs](src/checking.rs) owns shared scalar, pattern, scope and control
interpretation. Structural specialization renders a bounded container and lowers
it under the caller-selected schema. Fixed statements and inserted fragments are
checked together, including field counts and complete block forms. Display-only
containers retain syntax and symbol facts without treating their contents as
executed commands.

[template_scope.rs](src/template_scope.rs) projects entry requirements from the
same program using explicit Any/All continuations and concrete binding/scope
states. Unknown guards remain conditional. Runtime `OR` still checks every child
for static legality. Unresolved script prefixes prevent the projection from
assuming that the suffix keeps its original lexical or scope context.

IDE diagnostics, hover and completion request entry scopes for the definitions
they actually consume. Completion filters candidate names before requesting
their contracts. Definition diagnostics likewise request recursion coverage only
for the definitions in that document. Interactive queries do not build a report
for every workspace Template; the shared program and exact environment memo
still provide the same per-definition evidence and invalidation.

[block_checking.rs](src/block_checking.rs) checks each overloaded schema against
one complete container before choosing it. A proved choice retains its own scope
and symbol namespace. Unresolved alternatives retain possible schema contexts;
only common symbol, reference and scope facts enter the authoritative HIR. A
failure reports the whole overload requirement, rather than combining different
children's successful alternatives into a fictitious valid block.

[template_relations.rs](src/template_relations.rs) inverts finite strings into
binding rows. Repeated parameter occurrences share one value. Relations are
joined before projecting a focused completion value, so separate usages cannot
choose incompatible values for another parameter. Search work is bounded and
cancellable. Open scalar domains provide concrete checking and supported
suggestions; a finite suggestion list is not an exhaustive language definition.

[template_text.rs](src/template_text.rs) keeps generated ranges, definition
origins, root binding dependencies and exact byte maps where recoverable. The
map composes across forwarded quoted arguments. Diagnostics may fall back to the
whole argument when a precise projection is unavailable; edit consumers must not
turn that fallback into an arbitrary byte edit.

The adopted text profile performs one substitution pass. Inserted parameter
markers retain `TextInterpretation` uncertainty because no game execution
observation establishes a rescan rule. Owned text/call references and exhaustive
finite-domain checks live in [the audit tools](../tools/src/audit/templates/mod.rs).
Licensed source inputs, cache snapshots and performance reports remain local
under `target/`.

The semantic memo belongs to one immutable facts environment and has a byte bound.
The snapshot adapter compares exact overlay membership, attributes, Template sources
and ranges, overlay masking and fact coverage. It reuses results across caller edits
that preserve these facts, including negative lookup answers. Rules/index/source-root
changes invalidate the pool; texture generations participate in its identity.
At most four environments retain 16 MiB memos, with an 8 MiB total identity bound.
Oversized identities use only the current document view; unfinished resource queries
are not memoized. Parameter-site memo keys retain binding source ranges. Complete-container
instances use root-relative maps and share by bindings, presence, schema and scope;
source locations are supplied separately for each caller. Persistent compatibility uses the
workspace LSP release version.

Script body and item consumption share the complete parent instance. List inserts
are parsed as items and every value is checked; an inserted list is not treated
as one scalar. Consumer cursor/range projections use the same root byte maps and
quote-carrier ancestry as diagnostics. Ordinary strings are decoded as scalar
text without starting a script query.

The IDE validates a completion's actual source edit in an isolated frontend.
Generated snippet holes are explicit trial ranges, rather than user-text marker
heuristics. Script quote layers and snippet literal escaping compose separately;
completion resolve retains its rule identity and refuses to reinterpret stale
Template evidence. Completion data also retains the schema and overload identities
from the actual trial instance, and whether that interpretation remains conditional.
Unresolved callees retain an opaque argument map and a coverage
limit, never a statement schema guessed from their bindings.

[template_instance.rs](src/template_instance.rs) owns complete-container specialization
for both index discovery and IDE queries. Navigation, references, coloring and scope
hints project that same instance through exact root byte maps. A generated name
spanning multiple bindings cannot become an edit of an arbitrary argument. Rename
requires known reference discovery and an invertible selection; unfinished closure
returns an explicit rejection. Shared instances use the immutable view's bounded memo.
Unbound definition-side parameters are deferred bindings, rather than unfinished
lexical reference discovery. An actual invocation hole, a resource frontier or
a noninvertible generated selection still prevents a complete rename plan.

Secondary script parsing checks bytes before source allocation and checks nodes,
nesting and cancellation during token, comment, whitespace and recovery scans.
Limited parses return a frontier instead of publishing budget damage as user syntax
errors. Inserted markers carry local text-interpretation uncertainty; independent
sibling rejection evidence remains available.

[index fact discovery](../index/src/fact_stabilization.rs) records positive and
negative lookups per file. Rounds read immutable candidates, and only readers of
changed facts enter subsequent rounds. Disk edits rebuild their transitive reader
component from base declarations before solving, including outgoing generated facts.
This prevents deleted seeds from sustaining cycles. Oscillation compares exact fact
contents and aborts the transaction; histories and work are bounded. Persistent
refresh uses the same mechanism. [Overlay discovery](../index/src/overlay_facts.rs)
uses the same immutable-round and reader scheduling contract. It reuses syntax,
rebuilds dependent declarations from base input, and atomically publishes the
solved documents. Oscillation, work limits and cancellation discard speculative
facts and retain the latest syntax with FactStability coverage. Missing symbols
in that unfinished view remain Unknown during scalar and Pattern validation.

Rendered text carries the first unmaterialized byte at each truncation frontier.
Parser recovery beyond a frontier cannot become a user syntax error; independent
rejection evidence before it survives.

Consumed named call keys remain in the render trace after their bodies expand.
Navigation and rename use their exact root binding ranges. Cursor queries on a
removed key use its shared pre-expansion frame and still validate the real edit
against the complete root instance. Call frames are counted in memo byte costs.

[Pattern search](../rules/src/pattern.rs) shares a work/state/depth budget across
unions, nested Patterns and failed splits. It schedules one hole split at a time
and checks cancellation within search. Exhaustion is Unknown with PatternSearch
coverage, while independent rejection witnesses remain usable. Source projections
use semantic hole slices; multiple accepting slices retain Interpretation and do
not become arbitrary reference or rename ranges.
