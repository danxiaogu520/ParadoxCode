# First-party EU4 rules

`eu4/` is the maintained rule source. [eu4/game.json](eu4/game.json) owns the game identity,
source format, target game version, and profile configuration. Other JSON files are discovered
recursively in normalized path order. The runtime receives checked, embedded IR.

## Authoring

Start with the [language semantics](../crates/rules/LANGUAGE.md). The generated
[source-field reference](../crates/rules/SOURCE-REFERENCE.md) comes from the compiler's Rust
source model. `rulec schema` generates a JSON Schema on demand for editor completion and validation.
Use them for exact JSON shapes, required fields, serialization defaults, and editor validation.
The semantic guide explains composition, matching, symbols, scopes, and control flow.
Game-domain data belongs beside its related file category; shared vocabulary lives under `eu4/core/`.

## Check and regenerate

From the repository root:

```sh
cargo run --locked -p rules --bin rulec -- check rules/eu4
cargo run --locked -p rules --bin rulec -- fmt rules/eu4 --check
cargo run --locked -p tools -- rules bake --source rules/eu4
cargo run --locked -p tools -- documentation write
```

`rulec --help` is the CLI reference. `rulec schema` writes `target/rules-language.schema.json`
by default; use `--output PATH` to choose an editor's schema location. This local output is not
checked into the repository.
Bake performs semantic checks before replacing a valid artifact and writes only the compiled IR
to `target/rules/compiled.ir.json` by default. CI checks deterministic compilation and compares
the result directly with the embedded IR. The running LSP supplies its version, rule fingerprint
and arena counts through `pdc/analyzerInfo`; no separate rule manifest is maintained.

## Validate behavior

[Compiler tests](../crates/rules/src/compile.rs) exercise language semantics;
[first-party IR tests](../crates/rules/tests/first_party_ir.rs) cover maintained EU4 behavior.
The documentation checker validates complete `json rule-file` examples from the semantic guide.
Focused parser/HIR/IDE regressions prove downstream behavior. Follow
[local Vanilla acceptance](../CONTRIBUTING.md#local-vanilla-acceptance) when rule behavior changes.
Game data and external `.cwt` corpora are local reference inputs, not redistributed rule sources.
