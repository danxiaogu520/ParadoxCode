# Fuzz targets

The fuzz workspace is intentionally separate from the production Cargo workspace. Phase 3 adds
parser, incremental-edit, and formatter targets while keeping the editor-facing parser crates
free of fuzzing dependencies.

Run a short smoke session after installing `cargo-fuzz`:

```text
cargo fuzz run parse-pdx-script -- -runs=1000
cargo fuzz run parse-localisation -- -runs=1000
cargo fuzz run incremental-edits -- -runs=1000
cargo fuzz run format-pdx-script -- -runs=1000
cargo fuzz run lower-hir -- -runs=1000
```

Without the `cargo-fuzz` wrapper, a non-instrumented invariant smoke run is still available:

```text
cargo run --manifest-path fuzz/Cargo.toml --bin lower-hir --offline -- -runs=100
```

The targets assert that all ranges stay within the source, incremental results match a full
reparse, generic and profile-aware HIR lowering retain bounded structural, recovery, semantic,
and local-parameter facts, and formatting preserves the non-trivia token sequence.
