//! Game-independent PDX rules runtime and first-party compiler.
//!
//! This crate owns rules validation, the canonical logical hash, the typed IR, and the
//! first-party artifact compiler (`bake`). Production rules are checked and lowered at build
//! time, then embedded as a deterministic IR artifact.

/// Checked, deterministic Rules IR artifacts for build-time embedding.
pub mod bake;

/// Rules-v2 language front end (see `crates/rules/LANGUAGE.md`): the type-expression
/// mini-syntax, the source model, and the compile-time semantic checks used by
/// the production IR compiler.
pub mod bundle;
pub mod compile;
pub mod expr;
pub mod format;
pub mod ir;
pub mod lower;
pub mod pattern;
pub mod source;
pub mod template;

mod catalog;
mod hash;
mod matcher;
mod profile;
mod runtime;

pub use catalog::{FileCategory, FileResolutionPolicy, ParserKind};
pub use hash::RuleHash;
pub use matcher::FileMatcher;
pub use profile::{
    GameProfile, ProfileConditionalDefinitionRule, ProfileContainerDefinitionRule,
    ProfileContainerValueDefinitionRule, ProfileDefinitionRule, ProfileExecutablePaths,
    ProfileHoverCardSpec, ProfileInstallSpec, ProfileMatchMode, ProfileMemberNameSuffixRule,
    ProfileMissionNodeFields, ProfileMissionTreeFields, ProfileMissionViewSpec,
    ProfileReferenceRule, ProfileRootEntryInsertion, ProfileRootEntrySource, ProfileRootEntrySpec,
    ProfileRootScopeRule, ProfileScopeCompatibility, ProfileTextMatcher,
    ProfileTokenDefinitionRule, ProfileValueDefinitionRule, SourceEncoding,
};
pub use runtime::{RuleSet, RulesError};

#[cfg(test)]
mod catalog_tests;
