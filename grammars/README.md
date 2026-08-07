# Tree-sitter grammar

`grammars/tree-sitter-eu4` is the repository source of truth for the editor-side EU4 Script grammar.
It is used by Zed highlighting, indentation, outline and grammar corpus checks. The runtime parser in
`crates/pdx-parser` is a separate pure-Rust Script/Localisation parser and does not link Tree-sitter C.

Localisation is parsed by the Rust server during workspace indexing; this directory does not provide a
localisation grammar or a CSV grammar.

Run the grammar checks from the repository root:

```text
bash scripts/check-grammars.sh
```

The check runs `npm ci` when the local Tree-sitter CLI is absent, regenerates the parser, executes the
corpus tests and runs the grammar recovery check. Generated Node modules and native parser build
artifacts are ignored; grammar source, generated parser source and original corpus cases remain
reviewable.
