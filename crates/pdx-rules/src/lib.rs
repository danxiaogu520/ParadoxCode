//! Game-independent PDX rules schema, runtime, and first-party compiler.
//!
//! This crate owns the normalized runtime model, read-only loading, validation, the canonical
//! logical hash, and the first-party rule compiler (`pdx-bake`). The SQLite layout is
//! deliberately boring so the runtime remains inspectable without an authoring-format parser.

pub mod rulec;

mod canonical;
mod matcher;
mod model;
mod profile;
mod runtime;
mod sqlite;

pub use canonical::RuleHash;
pub use matcher::{FileMatcher, KeyMatcher, ValueMatcher};
pub use model::{
    FileCategory, FileResolutionPolicy, LocalisationBinding, LocalisationBindingCondition,
    ParserKind, RuleRecord, RuleShape, RulesModel, ScriptedMacroDescriptor, ScriptedMacroUsage,
    SemanticModel, SemanticRule, SymbolDescriptor, SymbolResolutionPolicy, TypeDescriptor,
};
pub use profile::{
    GameProfile, ProfileConditionalDefinitionRule, ProfileContainerDefinitionRule,
    ProfileContainerValueDefinitionRule, ProfileDefinitionRule, ProfileMatchMode,
    ProfileMemberNameSuffixRule, ProfileReferenceRule, ProfileRootScopeRule,
    ProfileScopeCompatibility, ProfileTextMatcher, ProfileTokenDefinitionRule,
    ProfileValueDefinitionRule, SourceEncoding,
};
pub use runtime::{RuleSet, RulesError};

/// The first runtime schema version reserved for the generated rule database.
pub const CURRENT_SCHEMA_VERSION: u32 = 20;

#[cfg(test)]
mod tests;
