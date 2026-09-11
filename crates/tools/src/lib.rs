//! Development and release tooling for the ParadoxCode repository.
//!
//! `tools` is the xtask-style entry point used by CI and maintainers:
//! `cargo tools -- check policy --root .` (via the `.cargo/config.toml`
//! alias; the long spelling is `cargo run -p tools -- ...`). It is never
//! published and is not part of the `pdc` language-server distribution.

pub mod check;
pub mod cli;
pub mod gates;
pub mod release;
