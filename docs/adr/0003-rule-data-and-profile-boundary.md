# Keep Declarative Rule Data Separate from EU4 Semantic Interpretation

The first-party JSON source remains authoritative for static rule data: file classification,
matchers, cardinality, scopes named by rules, type descriptors, documentation, and resolution
metadata. The EU4 profile remains responsible for algorithmic interpretation and runtime-derived
facts that cannot be represented as a static row, including scope-link behavior, control-flow and
iterator interpretation, transparent logic wrappers, dynamic WorkspaceIndex members, and parser
token extraction. This boundary is proposed for adoption because the current implementation already
has both layers; without an explicit boundary, the same semantic fact can drift between JSON and
`eu4.rs`.

**Status**: accepted

**Considered Options**: Put all EU4 behavior into JSON; rejected for runtime-derived workspace
facts and context-sensitive algorithms. Put all semantics into Rust; rejected because it would
make the first-party rule source non-authoritative and produce unreviewable hand-maintained tables.

**Consequences**: The JSON source remains the only editable source for static rule rows and
metadata. A profile-only fact must be justified as algorithmic, context-sensitive, or derived from
the current workspace; it must not silently duplicate a JSON row. Static profile fallbacks may
remain for bootstrap/test rule sets when the selected artifact does not contain the corresponding
metadata, but the official first-party artifact takes precedence.
