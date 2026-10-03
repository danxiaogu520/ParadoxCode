//! Development and release tooling for the ParadoxCode repository.
//!
//! `tools` is the xtask-style entry point used by CI and maintainers:
//! `cargo run --locked -p tools -- check policy --root .`. It is never
//! published and is not part of the `pdc` language-server distribution.

pub mod args;
pub mod audit;
pub mod check;
pub mod ci;
pub mod cli;
pub mod documentation;
pub mod e2e;
pub mod editor;
mod editor_contract;
pub mod gates;
pub mod lsp;
pub mod perf;
pub mod process;
pub mod release;
pub mod report;
