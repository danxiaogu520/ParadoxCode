//! Game-independent PDX rules runtime and first-party compiler.
//!
//! This crate owns the normalized runtime model, validation, the canonical logical hash, and
//! the first-party rule compiler (`bake`). Rules ship as the embedded JSON source bundle and
//! are compiled straight into the in-memory query indexes; there is no persisted rules
//! artifact.

pub mod rulec;

mod canonical;
mod matcher;
mod model;
mod profile;
mod runtime;

pub use canonical::RuleHash;
pub use matcher::{FileMatcher, KeyMatcher, TemplateParameter, TypedPrefixOperand, ValueMatcher};
pub use model::{
    DynamicDefinitionDescriptor, DynamicDefinitionUsage, FileCategory, FileResolutionPolicy,
    ParserKind, RuleRecord, RuleShape, RulesModel, SemanticModel, SemanticRule, SymbolBinding,
    SymbolBindingCondition, SymbolDescriptor, SymbolResolutionPolicy, TypeDescriptor,
    TypeRootScope, entry_wrapper_reroutes,
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
