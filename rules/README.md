# First-party EU4 rules

`rules/eu4/` is the only authoritative EU4 rule source. It is a strict versioned JSON bundle reviewed as project source; no CWT file, external database, game file or network resource participates in compilation.

## Source layout

- `manifest.json`: source format, game identity and target EU4 version;
- `catalog.json`: file categories, symbol descriptors and normalized records;
- `semantic-rules.json`: ordered semantic rule alternatives;
- `enum-values.json`: static enum members;
- `type-root-keys.json` and `type-root-scopes.json`: root selection and initial scope;
- `type-descriptors.json`: path, name and type-selection metadata;
- `localisation-bindings.json`: type-instance localisation templates and subtype conditions.

The current source format is `5`; the generated runtime SQLite schema is `16`.

## Validate and build

`pdx-bake` validates the fixed source layout and writes a temporary artifact/manifest for development or release checks:

```text
cargo run --locked -p pdx-rules --bin pdx-bake -- build \
  --source rules/eu4 \
  --output target/rules/eu4.pdxrules \
  --manifest target/rules/manifest.json
```

The generated SQLite artifact is never hand-maintained or committed. Official `pdx` and `pdx-ls` binaries embed the JSON source, compute its canonical `rule_hash`, and materialize a validated SQLite artifact in the user-local cache on cache miss or hash mismatch. The runtime exposes no rule override or external source path.

Every rule change must pass source schema/invariant validation, deterministic canonical hashing, SQLite round-trip, manifest/checksum verification and affected analysis/LSP tests. See [RFC 0013](../docs/rfc/0013-first-party-rule-source.md) and [the rule matrix](../docs/rule-semantics-matrix.md).
