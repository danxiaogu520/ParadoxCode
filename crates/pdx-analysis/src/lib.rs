//! Editor-neutral diagnostics and language-feature queries.
//!
//! The analysis crate owns all semantic decisions. `pdx-lsp` only converts the DTOs in this
//! module to protocol values, which keeps the same behaviour available to the CLI and tests.

mod completion;
mod diagnostics;
mod hover;
mod navigation;
mod resolution;
mod semantic;
mod support;
mod types;

pub use completion::{complete, complete_with_cancellation, completion, completion_resolve};
pub use diagnostics::{
    analyze, analyze_document, analyze_source_file, diagnostics, diagnostics_with_cancellation,
};
pub use hover::{hover, hover_with_cancellation};
pub use navigation::{
    definition, definition_with_cancellation, document_symbols, document_symbols_with_cancellation,
    prepare_rename, prepare_rename_with_cancellation, references, references_with_cancellation,
    rename, rename_with_cancellation, workspace_symbols, workspace_symbols_with_cancellation,
};
pub use types::{
    AnalysisResult, CancellationToken, Cancelled, CompletionItem, CompletionKind, CompletionResult,
    Diagnostic, DiagnosticCode, FileAnalysis, Hover, Location, PrepareRenameResult, ReferenceInfo,
    RenameError, RenameFailure, Symbol, WorkspaceEditPlan, WorkspaceSymbol, WorkspaceTextEdit,
};

// These crate-visible re-exports keep the existing in-crate test and helper paths stable while
// allowing the implementation to live in responsibility-oriented modules.
#[allow(unused_imports)]
pub(crate) use completion::{
    CompletionMemberCache, SemanticCompletionContext, add_semantic_key_items,
    scope_expression_candidates, semantic_completion_context,
};
#[cfg(test)]
pub(crate) use resolution::ALL_SEMANTICS_CALLS;
#[allow(unused_imports)]
pub(crate) use semantic::{
    parameter_names_for_owner, repeated_scope_register_depth, resolve_scope_expression_context,
    scope_context_from_hir, semantic_child_scope, semantic_root_context,
    semantic_selected_alternative, semantic_selected_transition,
};
#[allow(unused_imports)]
pub(crate) use support::{ScopeContext, ScriptProperty, input_for_document};
#[cfg(test)]
mod tests;
