//! Development and release tooling for the ParadoxCode repository.
//!
//! `tools` is the xtask-style entry point used by CI and maintainers:
//! `cargo run --locked -p tools -- check policy --root .`. It is never
//! published and is not part of the `pdc` language-server distribution.

pub mod args;
#[cfg(feature = "analysis")]
pub mod audit;
#[cfg(feature = "analysis")]
pub mod check;
pub mod ci;
#[cfg(feature = "analysis")]
pub mod cli;
#[cfg(not(feature = "analysis"))]
#[path = "cli_light.rs"]
pub mod cli;
mod cli_release;
pub mod delivery;
#[cfg(feature = "analysis")]
pub mod documentation;
#[cfg(feature = "analysis")]
pub mod e2e;
#[cfg(feature = "analysis")]
pub mod editor;
#[cfg(feature = "analysis")]
mod editor_contract;
#[cfg(feature = "analysis")]
pub mod gates;
#[cfg(feature = "analysis")]
pub mod lsp;
#[cfg(feature = "analysis")]
pub mod perf;
pub mod process;
pub mod release;
pub mod report;
