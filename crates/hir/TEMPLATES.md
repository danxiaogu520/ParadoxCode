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

The semantic memo belongs to one immutable facts view and has a byte bound.
Snapshot adapters currently use conservative document-domain invalidation;
negative lookups, overlays, rules and source-root changes must invalidate any
result that reads them. Cached keys still include binding source ranges until
all projected results can safely omit them. Persistent compatibility uses the
workspace LSP release version.
