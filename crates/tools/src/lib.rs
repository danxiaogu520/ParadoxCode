//! Development and release tooling for the ParadoxCode repository.
//!
//! `tools` is the xtask-style entry point used by CI, scripts, and maintainers:
//! `cargo run -p tools -- check policy --root .`. It is never published and is not
//! part of the `pdc` language-server distribution.

pub mod check;
pub mod cli;
pub mod release;
