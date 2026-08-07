# Fuzz targets

The fuzz workspace is intentionally separate from the production Cargo workspace. It keeps
libFuzzer dependencies out of the editor-facing parser crates while covering parser, edit, formatter
and HIR invariants.

Run a short smoke session after installing `cargo-fuzz` and a nightly toolchain:

```text
cargo +nightly fuzz run parse-script -- -runs=1000
cargo +nightly fuzz run parse-localisation -- -runs=1000
cargo +nightly fuzz run incremental-edits -- -runs=1000
cargo +nightly fuzz run format-script -- -runs=1000
cargo +nightly fuzz run lower-hir -- -runs=1000
```

The fuzz workspace can also be type-checked without sanitizer linking:

```text
cargo check --locked --manifest-path fuzz/Cargo.toml --bins
```

`cargo-fuzz` uses nightly for sanitizer instrumentation. The CI smoke runs on Ubuntu; Windows
requires the platform's AddressSanitizer runtime to be installed.

The targets assert that all ranges stay within the source, incremental results match a full
reparse, generic and profile-aware HIR lowering retain bounded structural, recovery, semantic,
and local-parameter facts, and formatting preserves the non-trivia token sequence.
