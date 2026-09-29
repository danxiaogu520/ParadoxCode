//! Game-independent PDX rules runtime and first-party compiler.
//!
//! This crate owns the normalized runtime model, validation, the canonical logical hash, and
//! the first-party rule compiler (`bake`). Rules ship as the embedded JSON source bundle and
//! are compiled straight into the in-memory query indexes; there is no persisted rules
//! artifact.

pub mod rulec;

/// Rules-v2 language front end (see `docs/rules-language.md`): the type-expression
/// mini-syntax, the source model, and the compile-time semantic checks. It is not
/// wired to the legacy runtime model yet; the switch happens in one cut.
pub mod compile;
pub mod expr;
pub mod source;

mod canonical;
mod matcher;
mod model;
mod profile;
mod runtime;

pub use canonical::RuleHash;
pub use matcher::{FileMatcher, KeyMatcher, TemplateParameter, TypedPrefixOperand, ValueMatcher};
pub use model::{
    DynamicDefinitionDescriptor, DynamicDefinitionUsage, FileCategory, FileResolutionPolicy,
    ParserKind, RuleShape, RulesModel, SemanticModel, SemanticRule, SymbolBinding,
    SymbolBindingCondition, TypeDescriptor, TypeRootScope, entry_wrapper_reroutes,
};
pub use profile::{
    GameProfile, ProfileConditionalDefinitionRule, ProfileContainerDefinitionRule,
    ProfileContainerValueDefinitionRule, ProfileDefinitionRule, ProfileHoverCardSpec,
    ProfileMatchMode, ProfileMemberNameSuffixRule, ProfileReferenceRule, ProfileRootEntryInsertion,
    ProfileRootEntrySource, ProfileRootEntrySpec, ProfileRootScopeRule, ProfileScopeCompatibility,
    ProfileTextMatcher, ProfileTokenDefinitionRule, ProfileValueDefinitionRule, SourceEncoding,
};
pub use runtime::{RuleSet, RulesError};

#[cfg(test)]
mod tests;
