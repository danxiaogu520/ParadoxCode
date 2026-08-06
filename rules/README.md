# First-party EU4 rules

`rules/eu4/` is ParadoxCode's only authoritative EU4 rule source. It is a strict, versioned JSON
source tree maintained by project developers and reviewed like code. No CWT file, external rule
database, game file, or network resource participates in compilation.

The source layout is:

- `manifest.json`: source format, game identity, and supported EU4 version;
- `catalog.json`: file categories, symbol policies, and normalized records;
- `semantic-rules.json`: executable key/value/scope/cardinality alternatives;
- `enum-values.json`: static enum members;
- `type-root-keys.json` and `type-root-scopes.json`: root selection and initial scope;
- `type-descriptors.json`: path and definition extraction metadata;
- `localisation-bindings.json`: type-instance localisation key bindings and subtype conditions.

Compile and validate it with:

```text
cargo run -p pdx-rules --bin pdx-bake -- build \
  --source rules/eu4 \
  --output target/rules/eu4.pdxrules \
  --manifest target/rules/manifest.json
```

`eu4.pdxrules` is a generated artifact used in developer/release validation and in the user-local
runtime cache; it is not committed to the repository. Official `pdx` and `pdx-ls` binaries embed
the first-party JSON source bundle, compile the artifact on cache miss or hash mismatch, and expose
no rule override. Every source change must include an appropriate regression fixture and pass the
source-to-artifact round-trip check. `rule_hash` identifies the canonical logical model;
`artifact_sha256` protects a generated SQLite artifact when it is materialized.

The current source uses source format 5 and generates schema 16. It contains 8,535 semantic rule
alternatives and targets EU4 1.37.5. The generated SQLite artifact is kept in build or user cache
locations, not in Git.
