# tree-sitter-pdx-script

The loss-aware Tree-sitter grammar for EU4 PdxScript. This directory is the
monorepo source of truth for the grammar consumed by the Zed extension.

The grammar intentionally models syntax rather than EU4 semantics. In
particular, it accepts duplicate keys and mixed blocks, while rule-aware
validation remains in the Rust analysis crates.

Run the corpus and parser checks with:

```text
npm install
npm test
```

All corpus examples are original, small fixtures. They cover the syntax
contract in RFC 0002, including incomplete input used during editing.
