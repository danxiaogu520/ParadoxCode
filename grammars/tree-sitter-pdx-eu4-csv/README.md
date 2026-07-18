# tree-sitter-pdx-eu4-csv

This is a small editor-only grammar for the EU4 CSV language entry in Zed. It
provides delimiter, quoted-cell, and row structure highlighting; the Rust
`pdx-syntax` CSV facade remains the authoritative parser for ranges, dialect,
and diagnostics. This grammar is never used as a PdxScript fallback.

The grammar accepts comma, semicolon, and tab delimiters so a project can use
the dialect selected by its rules metadata.
