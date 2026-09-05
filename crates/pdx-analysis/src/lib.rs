//! Editor-neutral diagnostics and language-feature queries.
//!
//! The analysis crate owns all semantic decisions. `pdx-lsp` only converts the DTOs in this
//! module to protocol values, which keeps the same behaviour available to the CLI and tests.

mod completion;
mod diagnostics;
mod dynamic_contracts;
mod dynamic_cycles;
mod dynamic_rules;
mod hover;
mod inlay;
mod lints;
mod localisation;
mod messages;
mod mission;
mod modifier_scope;
mod navigation;
mod quick_fix;
mod quoted_script;
mod resolution;
mod semantic;
mod semantic_tokens;
mod suggest;
mod support;
mod types;

pub use completion::{complete, complete_with_cancellation, completion, completion_resolve};
pub use diagnostics::{
    analyze, analyze_document, analyze_source_file, diagnostics, diagnostics_with_cancellation,
    source_file_diagnostics_with_cancellation, text_diagnostics_with_cancellation,
};
pub use hover::{hover, hover_with_cancellation};
pub use inlay::{MAX_SCOPE_INLAY_HINTS, scope_inlay_hints_with_cancellation};
pub use localisation::{
    scripted_localisation_names, scripted_localisation_names_with_cancellation,
};
pub use navigation::{
    definition, definition_with_cancellation, document_symbols, document_symbols_with_cancellation,
    prepare_rename, prepare_rename_with_cancellation, references, references_with_cancellation,
    rename, rename_with_cancellation, workspace_symbols, workspace_symbols_with_cancellation,
};
pub use quick_fix::{CodeFix, MAX_QUICK_FIXES, quick_fixes_with_cancellation};
pub use resolution::localisation_values_by_key;
pub use semantic_tokens::{
    semantic_tokens, semantic_tokens_in_range_with_cancellation, semantic_tokens_with_cancellation,
};
pub use types::{
    AnalysisResult, CancellationToken, Cancelled, CompletionItem, CompletionKind, CompletionResult,
    Diagnostic, DiagnosticCertainty, DiagnosticCode, DiagnosticProvenance, DiagnosticTag,
    FileAnalysis, Hover, Location, PrepareRenameResult, QuickFix, ReferenceInfo, RelatedLocation,
    RenameError, RenameFailure, ScopeInlayHint, SemanticToken, SemanticTokenType, Severity, Symbol,
    WorkspaceEditPlan, WorkspaceSymbol, WorkspaceTextEdit,
};

// These crate-visible re-exports keep the existing in-crate test and helper paths stable while
// allowing the implementation to live in responsibility-oriented modules.
#[cfg(test)]
pub(crate) use completion::semantic_completion_context;
#[cfg(test)]
pub(crate) use completion::{
    CompletionMemberCache, SemanticCompletionContext, add_semantic_key_items,
    scope_expression_candidates,
};
#[cfg(test)]
pub(crate) use resolution::ALL_SEMANTICS_CALLS;
#[cfg(test)]
pub(crate) use semantic::{
    SemanticTransitionInput, parameter_names_for_owner, repeated_scope_register_depth,
    resolve_scope_expression_context, scope_context_from_hir, semantic_child_scope,
    semantic_root_context, semantic_selected_alternative, semantic_selected_transition,
};
#[cfg(test)]
pub(crate) use support::{ScopeContext, ScriptProperty, input_for_document};
#[cfg(test)]
mod tests;
