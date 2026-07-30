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
- `type-descriptors.json`: path and definition extraction metadata.

Compile and validate it with:

```text
cargo run -p pdx-bake -- build \
  --source rules/eu4 \
  --output rules/eu4.pdxrules \
  --manifest rules/manifest.json
```

`eu4.pdxrules` and the release manifest are generated artifacts. Official `pdx` and `pdx-ls`
binaries embed that artifact at compile time and expose no rule override. Every source change must
include an appropriate regression fixture and regenerate both files. `rule_hash` identifies the
canonical logical model; `artifact_sha256` protects the generated SQLite bytes.

The current artifact uses schema 13 and source format 2. It contains 8,537 semantic rule
alternatives and targets EU4 1.37.5.
