//! Editor-neutral diagnostics and language-feature queries.
//!
//! The analysis crate owns all semantic decisions.  `pdx-lsp` only converts the DTOs in this
//! module to protocol values, which keeps the same behaviour available to the CLI and tests.

#[cfg(test)]
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
#[cfg(test)]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicBool, Ordering};

use pdx_engine::hir::{
    HirFile, HirReference, HirReferenceOrigin, Scope, ScopeState, ScopeValue,
    semantic_root_context as hir_semantic_root_context,
};
use pdx_engine::{
    AnalysisSnapshot, Definition, DocumentId, DocumentSource, ParsedSource, SourceFileId,
};
use pdx_parser::{CstKind, CstNode, FileFormat, ParsedFile, SyntaxError};
use pdx_rules::{GameProfile, KeyMatcher, RuleShape, SymbolResolutionPolicy, ValueMatcher};
use pdx_text::{LogicalPath, TextRange, TextSize};

/// Shared cooperative-cancellation state for editor-neutral analysis queries.
///
/// Clones observe the same flag. Query implementations check it while traversing workspace and
/// semantic rule data so protocol adapters can stop obsolete work without introducing protocol types here.
#[derive(Clone, Debug)]
pub struct CancellationToken {
    cancelled: Arc<AtomicBool>,
    #[cfg(test)]
    remaining_checkpoints: Arc<AtomicUsize>,
}

impl Default for CancellationToken {
    fn default() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            #[cfg(test)]
            remaining_checkpoints: Arc::new(AtomicUsize::new(usize::MAX)),
        }
    }
}

impl CancellationToken {
    /// Creates an uncancelled token.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Marks this token and every clone as cancelled.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    /// Reports whether cancellation has been requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    fn checkpoint(&self) -> Result<(), Cancelled> {
        #[cfg(test)]
        if self
            .remaining_checkpoints
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |remaining| {
                if remaining != usize::MAX && remaining > 0 {
                    Some(remaining - 1)
                } else {
                    None
                }
            })
            .is_err_and(|remaining| remaining == 0)
        {
            self.cancel();
        }
        if self.is_cancelled() {
            Err(Cancelled)
        } else {
            Ok(())
        }
    }

    #[cfg(test)]
    fn cancel_after(checkpoints: usize) -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            remaining_checkpoints: Arc::new(AtomicUsize::new(checkpoints)),
        }
    }
}

/// Marker returned when a cooperative analysis query stops early.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Cancelled;

fn uncancelled<T>(result: Result<T, Cancelled>) -> T {
    match result {
        Ok(value) => value,
        Err(Cancelled) => unreachable!("a fresh cancellation token cannot be cancelled"),
    }
}

/// Stable categories emitted by semantic analysis.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum DiagnosticCode {
    /// Syntax diagnostics from a format-specific parser.
    Syntax,
    /// A property key is not accepted by the current semantic context.
    UnknownKey,
    /// A symbol reference has no definition candidate.
    UnknownSymbol,
    /// A symbol reference has more than one valid definition candidate.
    AmbiguousSymbol,
    /// An explicit scope expression is not recognised.
    UnknownScope,
    /// A scalar or block does not satisfy the selected semantic matcher.
    InvalidValue,
    /// A semantic rule cardinality constraint was violated.
    Cardinality,
    /// A key or value is known to the semantic rule set but is used from the wrong game scope.
    WrongScope,
}

impl DiagnosticCode {
    /// Returns the stable wire-facing code.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Syntax => "pdx-parser",
            Self::UnknownKey => "pdx-unknown-key",
            Self::UnknownSymbol => "pdx-unknown-symbol",
            Self::AmbiguousSymbol => "pdx-ambiguous-symbol",
            Self::UnknownScope => "pdx-unknown-scope",
            Self::InvalidValue => "pdx-invalid-value",
            Self::Cardinality => "pdx-cardinality",
            Self::WrongScope => "pdx-wrong-scope",
        }
    }

    /// Returns the conservative LSP severity number (1 error, 2 warning).
    #[must_use]
    pub const fn severity(self) -> u8 {
        match self {
            Self::UnknownKey => 2,
            Self::Syntax
            | Self::UnknownSymbol
            | Self::AmbiguousSymbol
            | Self::UnknownScope
            | Self::InvalidValue
            | Self::Cardinality => 1,
            Self::WrongScope => 1,
        }
    }
}

/// An editor-neutral diagnostic.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Diagnostic {
    /// Stable category.
    pub code: DiagnosticCode,
    /// LSP severity (1 error, 2 warning, 3 information).
    pub severity: u8,
    /// Source range.
    pub range: TextRange,
    /// Human-readable message.
    pub message: String,
}

/// An editor-neutral source location.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Location {
    /// The open document containing the location, when applicable.
    pub document: Option<DocumentId>,
    /// The indexed disk file containing the location, when applicable.
    pub file: Option<SourceFileId>,
    /// Logical path relative to its source root.
    pub path: Option<LogicalPath>,
    /// Source range of the location.
    pub range: TextRange,
}

/// A document or workspace symbol.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Symbol {
    /// Symbol spelling.
    pub name: String,
    /// Rule-defined symbol kind, such as `event` or `localisation`.
    pub kind: String,
    /// Full symbol range.
    pub range: TextRange,
    /// Exact name range used by navigation.
    pub selection_range: TextRange,
    /// Symbol location.
    pub location: Location,
}

/// A completion item returned by the editor-neutral query layer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompletionItem {
    /// Label shown to the user.
    pub label: String,
    /// Stable broad item kind.
    pub kind: CompletionKind,
    /// Short semantic detail.
    pub detail: String,
    /// Optional documentation.
    pub documentation: Option<String>,
    /// Range to replace.
    pub replacement_range: TextRange,
    /// Text inserted into the document.
    pub insert_text: String,
    /// Lower values sort first.
    pub sort_score: u32,
    /// Whether the item is deprecated.
    pub deprecated: bool,
}

/// Broad completion item categories independent of LSP enum values.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum CompletionKind {
    /// A command or property key.
    Key,
    /// A scalar value or enum member.
    Value,
    /// A symbol from the workspace index.
    Symbol,
    /// A localisation key.
    Localisation,
}

/// Completion query output.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompletionResult {
    /// Snapshot revision used by the query.
    pub revision: u64,
    /// Replacement-aware candidates.
    pub items: Vec<CompletionItem>,
}

/// Hover information returned by the query layer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Hover {
    /// Plain-text content; clients may render it as Markdown.
    pub contents: String,
    /// Token range that produced the hover.
    pub range: Option<TextRange>,
}

/// The exact identifier range accepted by a prepare-rename request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PrepareRenameResult {
    /// The name token that the editor should select.
    pub range: TextRange,
    /// The current spelling shown in clients that support a placeholder.
    pub placeholder: String,
}

/// One editor-neutral text edit in a rename plan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceTextEdit {
    /// The source location to replace.  Rename only produces locations in writable current-Mod
    /// files or open overlays.
    pub location: Location,
    /// The replacement identifier.
    pub new_text: String,
}

/// A complete, immutable rename transaction plan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceEditPlan {
    /// Snapshot revision used to produce the plan.
    pub revision: u64,
    /// Non-overlapping edits, grouped by target by protocol adapters and ordered backwards within
    /// each target so a client can safely apply them.
    pub edits: Vec<WorkspaceTextEdit>,
}

/// Reasons a semantic rename is refused.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum RenameError {
    /// The cursor is not on a known definition or reference.
    NoSymbol,
    /// The cursor resolves to no definition.
    Unresolved,
    /// The cursor or a target name has multiple valid definitions.
    Ambiguous,
    /// The selected definition belongs to Vanilla, a dependency, or another read-only source.
    ReadOnly,
    /// The requested replacement is not a single PDX identifier token.
    InvalidName,
    /// The replacement would create a same-priority or otherwise disallowed definition conflict.
    Conflict,
}

/// Failure from a cancellable rename query.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RenameFailure {
    /// The caller cancelled the query before it completed.
    Cancelled,
    /// Semantic safety checks rejected the requested rename.
    Rejected(RenameError),
}

impl From<RenameError> for RenameFailure {
    fn from(error: RenameError) -> Self {
        Self::Rejected(error)
    }
}

impl std::fmt::Display for RenameError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::NoSymbol => "cursor is not on a renameable symbol",
            Self::Unresolved => "symbol has no unique definition",
            Self::Ambiguous => "symbol has multiple definitions",
            Self::ReadOnly => "symbol is defined in a read-only source",
            Self::InvalidName => "new name is not a valid PDX identifier",
            Self::Conflict => "new name conflicts with another definition",
        })
    }
}

/// A fully analysed file snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileAnalysis {
    /// Snapshot revision used by the query.
    pub revision: u64,
    /// Open document identity, if this is an overlay query.
    pub document: Option<DocumentId>,
    /// Disk file identity, if one exists.
    pub file: Option<SourceFileId>,
    /// Parsed frontend, if the file is a supported text frontend.
    pub format: Option<FileFormat>,
    /// Current conservative scope.
    pub scope: Scope,
    /// Diagnostics for this file.
    pub diagnostics: Vec<Diagnostic>,
    /// Definitions declared by this file.
    pub symbols: Vec<Symbol>,
    /// References found in this file.
    pub references: Vec<ReferenceInfo>,
}

/// A semantic reference exposed for tests and editor-neutral consumers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReferenceInfo {
    /// Reference kind.
    pub kind: String,
    /// Referenced spelling.
    pub name: String,
    /// Source location.
    pub location: Location,
}

/// A workspace-symbol result with the same shape as a document symbol.
pub type WorkspaceSymbol = Symbol;

/// Result shell retained for compatibility with the original analysis facade.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnalysisResult {
    /// Snapshot revision used to produce the result.
    pub revision: u64,
    /// Conservative scope until a file is lowered.
    pub scope: Scope,
    /// Diagnostics from currently open overlays.
    pub diagnostics: Vec<Diagnostic>,
}

/// Runs diagnostics for all open overlays.  Disk-only files are intentionally excluded from push
/// diagnostics; they still participate in navigation and workspace-symbol queries.
#[must_use]
pub fn analyze(snapshot: &AnalysisSnapshot) -> AnalysisResult {
    let mut diagnostics = Vec::new();
    for document in snapshot.documents().values() {
        if document.source() != DocumentSource::Overlay {
            continue;
        }
        if let Some(analysis) = analyze_document(snapshot, document.id()) {
            diagnostics.extend(analysis.diagnostics);
        }
    }
    diagnostics.sort_by_key(|diagnostic| {
        (
            diagnostic.range.start(),
            diagnostic.range.end(),
            diagnostic.code,
        )
    });
    AnalysisResult {
        revision: snapshot.revision(),
        scope: Scope::Unknown,
        diagnostics,
    }
}

/// Analyses one open or disk-backed document.
#[must_use]
pub fn analyze_document(
    snapshot: &AnalysisSnapshot,
    document: &DocumentId,
) -> Option<FileAnalysis> {
    let input = input_for_document(snapshot, document)?;
    Some(analyze_input(snapshot, &input))
}

/// Analyses one indexed disk file.
#[must_use]
pub fn analyze_source_file(
    snapshot: &AnalysisSnapshot,
    file: SourceFileId,
) -> Option<FileAnalysis> {
    let input = input_for_source_file(snapshot, file)?;
    Some(analyze_input(snapshot, &input))
}

/// Returns diagnostics for one document, or an empty vector for unsupported/nonexistent files.
#[must_use]
pub fn diagnostics(snapshot: &AnalysisSnapshot, document: &DocumentId) -> Vec<Diagnostic> {
    uncancelled(diagnostics_with_cancellation(
        snapshot,
        document,
        &CancellationToken::new(),
    ))
}

/// Returns diagnostics while cooperatively stopping when `cancellation` is marked.
pub fn diagnostics_with_cancellation(
    snapshot: &AnalysisSnapshot,
    document: &DocumentId,
    cancellation: &CancellationToken,
) -> Result<Vec<Diagnostic>, Cancelled> {
    cancellation.checkpoint()?;
    let Some(input) = input_for_document(snapshot, document) else {
        return Ok(Vec::new());
    };
    analyze_input_with_cancellation(snapshot, &input, cancellation)
        .map(|analysis| analysis.diagnostics)
}

/// Computes key, value, localisation, and symbol completion.
#[must_use]
pub fn complete(
    snapshot: &AnalysisSnapshot,
    document: &DocumentId,
    position: TextSize,
) -> CompletionResult {
    uncancelled(complete_with_cancellation(
        snapshot,
        document,
        position,
        &CancellationToken::new(),
    ))
}

/// Computes completion with cooperative cancellation checkpoints.
pub fn complete_with_cancellation(
    snapshot: &AnalysisSnapshot,
    document: &DocumentId,
    position: TextSize,
    cancellation: &CancellationToken,
) -> Result<CompletionResult, Cancelled> {
    cancellation.checkpoint()?;
    let Some(input) = input_for_document(snapshot, document) else {
        return Ok(CompletionResult {
            revision: snapshot.revision(),
            items: Vec::new(),
        });
    };
    let replacement_range = word_range(&input.source, position);
    let prefix = input
        .source_text(replacement_range)
        .unwrap_or_default()
        .to_owned();
    let value_context = completion_value_context(&input, position);
    let mut items = Vec::new();
    let mut member_cache = CompletionMemberCache::default();
    let semantic_context = semantic_completion_context(snapshot, &input, position);
    if let Some(context) = semantic_context.as_ref() {
        cancellation.checkpoint()?;
        if value_context {
            if let Some(property) = context.property.as_ref() {
                add_semantic_value_items(
                    snapshot,
                    context,
                    property,
                    &mut member_cache,
                    &mut items,
                    replacement_range,
                    &prefix,
                );
            }
        } else {
            add_semantic_key_items(
                snapshot,
                context,
                &mut member_cache,
                &mut items,
                replacement_range,
                &prefix,
            );
        }
    }
    if items.is_empty() && value_context {
        add_scalar_items(&mut items, replacement_range, &prefix);
        for (kind_name, definition_name) in completion_definitions(snapshot, &prefix, cancellation)?
        {
            cancellation.checkpoint()?;
            let kind = if kind_name == "localisation" {
                CompletionKind::Localisation
            } else {
                CompletionKind::Symbol
            };
            let detail = format!("{kind_name} symbol");
            push_completion(
                &mut items,
                CompletionItem {
                    label: definition_name.clone(),
                    kind,
                    detail,
                    documentation: None,
                    replacement_range,
                    insert_text: definition_name,
                    sort_score: if kind_name == "localisation" { 20 } else { 30 },
                    deprecated: false,
                },
                &prefix,
            );
        }
    } else if items.is_empty() {
        for key in known_keys(snapshot) {
            cancellation.checkpoint()?;
            push_completion(
                &mut items,
                CompletionItem {
                    label: key.clone(),
                    kind: CompletionKind::Key,
                    detail: "PDX property".to_owned(),
                    documentation: None,
                    replacement_range,
                    insert_text: key,
                    sort_score: 10,
                    deprecated: false,
                },
                &prefix,
            );
        }
        for (kind_name, definition_name) in completion_definitions_for_kinds(
            snapshot,
            &prefix,
            &["scripted_effect", "scripted_trigger"],
            cancellation,
        )? {
            cancellation.checkpoint()?;
            if matches!(kind_name.as_str(), "scripted_effect" | "scripted_trigger") {
                push_completion(
                    &mut items,
                    CompletionItem {
                        label: definition_name.clone(),
                        kind: CompletionKind::Symbol,
                        detail: format!("{kind_name} command"),
                        documentation: None,
                        replacement_range,
                        insert_text: format!("{definition_name} = {{\n    $0\n}}"),
                        sort_score: 15,
                        deprecated: false,
                    },
                    &prefix,
                );
            }
        }
    }
    if items.is_empty() {
        // Recovery nodes and a partially typed new identifier must still expose useful
        // candidates. An empty filtered set is less useful than a small conservative fallback.
        add_scalar_items(&mut items, replacement_range, "");
        if !value_context {
            for key in known_keys(snapshot).into_iter().take(32) {
                cancellation.checkpoint()?;
                items.push(CompletionItem {
                    label: key.clone(),
                    kind: CompletionKind::Key,
                    detail: "PDX property".to_owned(),
                    documentation: None,
                    replacement_range,
                    insert_text: key,
                    sort_score: 50,
                    deprecated: false,
                });
            }
        }
    }
    items.sort_by_key(|item| (item.sort_score, item.label.to_ascii_lowercase()));
    items.dedup_by(|left, right| left.label == right.label && left.kind == right.kind);
    cancellation.checkpoint()?;
    Ok(CompletionResult {
        revision: snapshot.revision(),
        items,
    })
}

#[derive(Clone, Debug)]
struct SemanticCompletionContext {
    context: String,
    parent_path: Vec<String>,
    structural_containers: Vec<(String, Vec<String>)>,
    alternative_containers: Vec<SemanticCompletionContainer>,
    scope: ScopeContext,
    property: Option<ScriptProperty>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SemanticCompletionContainer {
    context: String,
    parent_path: Vec<String>,
    scope: ScopeContext,
}

fn semantic_completion_context(
    snapshot: &AnalysisSnapshot,
    input: &ParsedInput,
    position: TextSize,
) -> Option<SemanticCompletionContext> {
    let ParsedContent::Text(parsed) = &input.parsed;
    for root in script_properties(input, parsed.root()) {
        let Some(context) = semantic_root_context(snapshot, &root.key, input.path.as_ref()) else {
            continue;
        };
        let Some(block_range) = root.block_range else {
            continue;
        };
        if !contains(block_range, position) {
            continue;
        }
        let scope = semantic_initial_scope(snapshot, input, &context, &root.key, root.key_range);
        return Some(semantic_completion_container(
            snapshot,
            input.hir.as_deref(),
            context,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            root.block,
            root.bare_values,
            scope,
            position,
        ));
    }
    None
}

#[allow(clippy::too_many_arguments)] // Recursive traversal carries immutable HIR and semantic state.
fn semantic_completion_container(
    snapshot: &AnalysisSnapshot,
    hir: Option<&HirFile>,
    context: String,
    parent_path: Vec<String>,
    structural_containers: Vec<(String, Vec<String>)>,
    alternative_containers: Vec<SemanticCompletionContainer>,
    properties: Vec<ScriptProperty>,
    _bare_values: Vec<(String, TextRange)>,
    scope: ScopeContext,
    position: TextSize,
) -> SemanticCompletionContext {
    for property in &properties {
        let Some(block_range) = property.block_range else {
            continue;
        };
        if !contains(block_range, position) {
            continue;
        }
        let transparent_wrapper = context.eq_ignore_ascii_case("trigger")
            && snapshot
                .game_profile()
                .is_transparent_scope_wrapper(&property.key);
        let next_rules = semantic_rules_for_container(snapshot, &context, &parent_path, &scope)
            .into_iter()
            .filter(|rule| {
                !matches!(rule.shape, RuleShape::LeafValue)
                    && semantic_rule_key_matches(snapshot, rule, &parent_path, &property.key)
                    && semantic_scope_allows(rule, &scope)
            })
            .collect::<Vec<_>>();
        let cached_child_fact = cached_scope_fact_for_property(
            snapshot,
            hir,
            &context,
            &parent_path,
            property,
            &next_rules,
            None,
            &scope,
            transparent_wrapper,
        );
        if let Some(fact) = cached_child_fact {
            let structural_containers = completion_structural_containers(
                snapshot,
                &context,
                &parent_path,
                &property.key,
                transparent_wrapper,
                &fact.context,
                &fact.parent_path,
                &scope,
            );
            return semantic_completion_container(
                snapshot,
                hir,
                fact.context.clone(),
                fact.parent_path.clone(),
                structural_containers,
                Vec::new(),
                property.block.clone(),
                property.bare_values.clone(),
                scope_context_from_hir(snapshot.game_profile_handle(), &fact.state),
                position,
            );
        }
        let mut destinations = Vec::<SemanticCompletionContainer>::new();
        for rule in next_rules {
            let (destination_context, destination_path) =
                rule.child_context.as_deref().map_or_else(
                    || {
                        let mut path = parent_path.clone();
                        if !transparent_wrapper {
                            path.push(property.key.clone());
                        }
                        (context.clone(), path)
                    },
                    |child_context| (child_context.to_owned(), Vec::new()),
                );
            let destination = SemanticCompletionContainer {
                context: destination_context,
                parent_path: destination_path,
                scope: semantic_child_scope(snapshot, &scope, rule),
            };
            if !destinations
                .iter()
                .any(|known| semantic_completion_containers_equal(known, &destination))
            {
                destinations.push(destination);
            }
        }
        let primary = destinations.first().cloned().unwrap_or_else(|| {
            let mut path = parent_path.clone();
            if !transparent_wrapper {
                path.push(property.key.clone());
            }
            SemanticCompletionContainer {
                context: context.clone(),
                parent_path: path,
                scope: scope.clone(),
            }
        });
        let alternative_containers = destinations.into_iter().skip(1).collect::<Vec<_>>();
        let structural_containers = completion_structural_containers(
            snapshot,
            &context,
            &parent_path,
            &property.key,
            transparent_wrapper,
            &primary.context,
            &primary.parent_path,
            &scope,
        );
        return semantic_completion_container(
            snapshot,
            hir,
            primary.context,
            primary.parent_path,
            structural_containers,
            alternative_containers,
            property.block.clone(),
            property.bare_values.clone(),
            primary.scope,
            position,
        );
    }
    let property = properties
        .into_iter()
        .find(|property| contains(property.range, position));
    SemanticCompletionContext {
        context,
        parent_path,
        structural_containers,
        alternative_containers,
        scope,
        property,
    }
}

fn semantic_completion_containers_equal(
    left: &SemanticCompletionContainer,
    right: &SemanticCompletionContainer,
) -> bool {
    left.context.eq_ignore_ascii_case(&right.context)
        && left.parent_path.len() == right.parent_path.len()
        && left
            .parent_path
            .iter()
            .zip(&right.parent_path)
            .all(|(left, right)| left.eq_ignore_ascii_case(right))
        && left.scope == right.scope
}

#[allow(clippy::too_many_arguments)]
fn completion_structural_containers(
    snapshot: &AnalysisSnapshot,
    context: &str,
    parent_path: &[String],
    property_key: &str,
    transparent_wrapper: bool,
    next_context: &str,
    next_path: &[String],
    scope: &ScopeContext,
) -> Vec<(String, Vec<String>)> {
    let mut structural_path = parent_path.to_vec();
    if !transparent_wrapper {
        structural_path.push(property_key.to_owned());
    }
    let destination_is_structural = next_context.eq_ignore_ascii_case(context)
        && next_path.len() == structural_path.len()
        && next_path
            .iter()
            .zip(&structural_path)
            .all(|(left, right)| left.eq_ignore_ascii_case(right));
    if destination_is_structural
        || semantic_rules_for_container(snapshot, context, &structural_path, scope).is_empty()
    {
        Vec::new()
    } else {
        vec![(context.to_owned(), structural_path)]
    }
}

fn semantic_rules_for_container<'a>(
    snapshot: &'a AnalysisSnapshot,
    context: &str,
    parent_path: &[String],
    _scope: &ScopeContext,
) -> Vec<&'a pdx_rules::SemanticRule> {
    let mut candidates = snapshot
        .rules()
        .semantic_rules_for_context(context)
        .collect::<Vec<_>>();
    if let Some(type_name) = context.strip_prefix("type:") {
        candidates.extend(
            snapshot
                .rules()
                .semantic_rules_for_context(&format!("root:{type_name}")),
        );
        candidates.sort_by(|left, right| left.id.cmp(&right.id));
    }
    candidates
        .into_iter()
        .filter(|rule| semantic_parent_path_matches(snapshot, &rule.parent_path, parent_path))
        .collect()
}

struct SemanticCompletionRule<'rule, 'path> {
    rule: &'rule pdx_rules::SemanticRule,
    parent_path: &'path [String],
    scope: &'path ScopeContext,
}

#[derive(Default)]
struct CompletionMemberCache {
    workspace: BTreeMap<(String, String), Vec<String>>,
    enums: BTreeMap<(String, String), Vec<String>>,
}

impl CompletionMemberCache {
    fn workspace_member_names(
        &mut self,
        snapshot: &AnalysisSnapshot,
        type_name: &str,
        prefix: &str,
    ) -> &[String] {
        let cache_key = (type_name.to_ascii_lowercase(), prefix.to_ascii_lowercase());
        self.workspace.entry(cache_key).or_insert_with(|| {
            let base = type_name
                .split_once('.')
                .map_or(type_name, |(kind, _)| kind);
            let alias = snapshot.game_profile().member_kind_alias(base);
            let mut kinds = vec![type_name, base];
            if let Some(alias) = alias {
                kinds.push(alias);
            }
            kinds.sort_unstable();
            kinds.dedup();
            let mut names = kinds
                .into_iter()
                .flat_map(|kind| snapshot.index().definitions_for_kind(kind))
                .map(|definition| definition.name.clone())
                .filter(|name| starts_with_ignore_ascii_case(name, prefix))
                .collect::<Vec<_>>();
            names.sort_by_key(|name| name.to_ascii_lowercase());
            names.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
            names
        })
    }

    fn enum_member_names(
        &mut self,
        snapshot: &AnalysisSnapshot,
        enum_name: &str,
        prefix: &str,
    ) -> &[String] {
        let cache_key = (enum_name.to_ascii_lowercase(), prefix.to_ascii_lowercase());
        if !self.enums.contains_key(&cache_key) {
            let mut names = snapshot
                .rules()
                .model()
                .semantic
                .enum_values
                .get(enum_name)
                .cloned()
                .unwrap_or_default();
            names.extend(
                self.workspace_member_names(snapshot, enum_name, prefix)
                    .iter()
                    .cloned(),
            );
            names.retain(|name| starts_with_ignore_ascii_case(name, prefix));
            names.sort_by_key(|name| name.to_ascii_lowercase());
            names.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
            self.enums.insert(cache_key.clone(), names);
        }
        self.enums.get(&cache_key).map_or(&[], Vec::as_slice)
    }
}

fn semantic_rules_for_completion<'rule, 'path>(
    snapshot: &'rule AnalysisSnapshot,
    context: &'path SemanticCompletionContext,
) -> Vec<SemanticCompletionRule<'rule, 'path>> {
    let mut rules = semantic_rules_for_container(
        snapshot,
        &context.context,
        &context.parent_path,
        &context.scope,
    )
    .into_iter()
    .map(|rule| SemanticCompletionRule {
        rule,
        parent_path: &context.parent_path,
        scope: &context.scope,
    })
    .collect::<Vec<_>>();
    for (structural_context, structural_path) in &context.structural_containers {
        rules.extend(
            semantic_rules_for_container(
                snapshot,
                structural_context,
                structural_path,
                &context.scope,
            )
            .into_iter()
            .map(|rule| SemanticCompletionRule {
                rule,
                parent_path: structural_path,
                scope: &context.scope,
            }),
        );
    }
    for alternative in &context.alternative_containers {
        rules.extend(
            semantic_rules_for_container(
                snapshot,
                &alternative.context,
                &alternative.parent_path,
                &alternative.scope,
            )
            .into_iter()
            .map(|rule| SemanticCompletionRule {
                rule,
                parent_path: &alternative.parent_path,
                scope: &alternative.scope,
            }),
        );
    }
    rules.sort_by(|left, right| left.rule.id.cmp(&right.rule.id));
    rules.dedup_by(|left, right| {
        left.rule.id == right.rule.id
            && left.parent_path.len() == right.parent_path.len()
            && left
                .parent_path
                .iter()
                .zip(right.parent_path)
                .all(|(left, right)| left.eq_ignore_ascii_case(right))
            && left.scope == right.scope
    });
    rules
}

fn add_semantic_key_items(
    snapshot: &AnalysisSnapshot,
    context: &SemanticCompletionContext,
    member_cache: &mut CompletionMemberCache,
    items: &mut Vec<CompletionItem>,
    replacement_range: TextRange,
    prefix: &str,
) {
    for candidate in semantic_rules_for_completion(snapshot, context) {
        let rule = candidate.rule;
        if matches!(rule.shape, RuleShape::LeafValue)
            || !semantic_scope_allows(rule, candidate.scope)
        {
            continue;
        }
        let documentation = (!rule.documentation.is_empty()).then(|| rule.documentation.join("\n"));
        match &rule.key {
            KeyMatcher::Exact(label) => push_completion(
                items,
                CompletionItem {
                    label: label.clone(),
                    kind: CompletionKind::Key,
                    detail: semantic_rule_detail(rule),
                    documentation,
                    replacement_range,
                    insert_text: label.clone(),
                    sort_score: if rule.required { 2 } else { 5 },
                    deprecated: false,
                },
                prefix,
            ),
            KeyMatcher::Type(type_name) => {
                for label in member_cache.workspace_member_names(snapshot, type_name, prefix) {
                    push_completion(
                        items,
                        CompletionItem {
                            label: label.clone(),
                            kind: CompletionKind::Key,
                            detail: format!("semantic type key <{type_name}>"),
                            documentation: documentation.clone(),
                            replacement_range,
                            insert_text: label.clone(),
                            sort_score: 8,
                            deprecated: false,
                        },
                        prefix,
                    );
                }
            }
            KeyMatcher::Enum(enum_name) => {
                if let Some(labels) =
                    qualified_parameter_names(snapshot, rule, candidate.parent_path)
                {
                    for label in labels {
                        push_completion(
                            items,
                            CompletionItem {
                                label: label.clone(),
                                kind: CompletionKind::Key,
                                detail: format!("semantic enum key enum[{enum_name}]"),
                                documentation: documentation.clone(),
                                replacement_range,
                                insert_text: label,
                                sort_score: 8,
                                deprecated: false,
                            },
                            prefix,
                        );
                    }
                } else {
                    for label in member_cache.enum_member_names(snapshot, enum_name, prefix) {
                        push_completion(
                            items,
                            CompletionItem {
                                label: label.clone(),
                                kind: CompletionKind::Key,
                                detail: format!("semantic enum key enum[{enum_name}]"),
                                documentation: documentation.clone(),
                                replacement_range,
                                insert_text: label.clone(),
                                sort_score: 8,
                                deprecated: false,
                            },
                            prefix,
                        );
                    }
                }
            }
            KeyMatcher::Dynamic(kind) => {
                for label in member_cache.workspace_member_names(snapshot, kind, prefix) {
                    push_completion(
                        items,
                        CompletionItem {
                            label: label.clone(),
                            kind: CompletionKind::Key,
                            detail: format!("semantic dynamic key value_set[{kind}]"),
                            documentation: documentation.clone(),
                            replacement_range,
                            insert_text: label.clone(),
                            sort_score: 8,
                            deprecated: false,
                        },
                        prefix,
                    );
                }
            }
            KeyMatcher::AnyScalar => {}
        }
    }
}

fn add_semantic_value_items(
    snapshot: &AnalysisSnapshot,
    context: &SemanticCompletionContext,
    property: &ScriptProperty,
    member_cache: &mut CompletionMemberCache,
    items: &mut Vec<CompletionItem>,
    replacement_range: TextRange,
    prefix: &str,
) {
    let matching = semantic_rules_for_completion(snapshot, context)
        .into_iter()
        .filter(|candidate| {
            let rule = candidate.rule;
            !matches!(rule.shape, RuleShape::LeafValue)
                && semantic_rule_key_matches(snapshot, rule, candidate.parent_path, &property.key)
                && rule
                    .operator
                    .as_deref()
                    .is_none_or(|operator| property.operator.as_deref() == Some(operator))
        })
        .filter(|candidate| semantic_scope_allows(candidate.rule, candidate.scope))
        .collect::<Vec<_>>();
    for candidate in matching {
        let rule = candidate.rule;
        let documentation = (!rule.documentation.is_empty()).then(|| rule.documentation.join("\n"));
        match &rule.value {
            ValueMatcher::Exact(label) => add_value_completion(
                items,
                label,
                &semantic_value_matcher_label(&rule.value),
                documentation.clone(),
                replacement_range,
                prefix,
            ),
            ValueMatcher::Bool => {
                add_value_completion(
                    items,
                    "yes",
                    "bool",
                    documentation.clone(),
                    replacement_range,
                    prefix,
                );
                add_value_completion(
                    items,
                    "no",
                    "bool",
                    documentation.clone(),
                    replacement_range,
                    prefix,
                );
            }
            ValueMatcher::Int { min, max } => {
                add_numeric_completion(
                    items,
                    min.map(|value| value.to_string()).as_deref(),
                    "int",
                    documentation.clone(),
                    replacement_range,
                    prefix,
                );
                add_numeric_completion(
                    items,
                    max.map(|value| value.to_string()).as_deref(),
                    "int",
                    documentation.clone(),
                    replacement_range,
                    prefix,
                );
            }
            ValueMatcher::Float { min, max } => {
                add_value_completion(
                    items,
                    "0",
                    "float",
                    documentation.clone(),
                    replacement_range,
                    prefix,
                );
                if min.is_some() || max.is_some() {
                    add_value_completion(
                        items,
                        min.as_deref().unwrap_or("1"),
                        "float",
                        documentation.clone(),
                        replacement_range,
                        prefix,
                    );
                    add_value_completion(
                        items,
                        max.as_deref().unwrap_or("1"),
                        "float",
                        documentation.clone(),
                        replacement_range,
                        prefix,
                    );
                }
            }
            ValueMatcher::Type(type_name) => {
                for label in member_cache.workspace_member_names(snapshot, type_name, prefix) {
                    add_value_completion(
                        items,
                        label,
                        &format!("<{type_name}>"),
                        documentation.clone(),
                        replacement_range,
                        prefix,
                    );
                }
            }
            ValueMatcher::Enum(enum_name) => {
                for label in member_cache.enum_member_names(snapshot, enum_name, prefix) {
                    add_value_completion(
                        items,
                        label,
                        &format!("enum[{enum_name}]"),
                        documentation.clone(),
                        replacement_range,
                        prefix,
                    );
                }
            }
            ValueMatcher::Scope(expected) => {
                for label in &snapshot.game_profile().scope_completions {
                    if expected
                        .as_deref()
                        .is_none_or(|scope| snapshot.game_profile().scopes_compatible(label, scope))
                    {
                        add_value_completion(
                            items,
                            label,
                            "scope",
                            documentation.clone(),
                            replacement_range,
                            prefix,
                        );
                    }
                }
            }
            ValueMatcher::Localisation => {
                for label in member_cache.workspace_member_names(snapshot, "localisation", prefix) {
                    add_value_completion(
                        items,
                        label,
                        "localisation",
                        documentation.clone(),
                        replacement_range,
                        prefix,
                    );
                }
            }
            ValueMatcher::Dynamic(kind) => {
                for label in member_cache.workspace_member_names(snapshot, kind, prefix) {
                    add_value_completion(
                        items,
                        label,
                        &format!("value[{kind}]"),
                        documentation.clone(),
                        replacement_range,
                        prefix,
                    );
                }
                if matches!(kind.as_str(), "variable" | "value") {
                    add_value_completion(
                        items,
                        "$0",
                        &format!("value[{kind}]"),
                        documentation.clone(),
                        replacement_range,
                        prefix,
                    );
                }
            }
            ValueMatcher::DynamicSet(_)
            | ValueMatcher::AnyScalar
            | ValueMatcher::Filepath
            | ValueMatcher::Opaque(_) => {}
        }
    }
}

fn add_value_completion(
    items: &mut Vec<CompletionItem>,
    label: &str,
    detail: &str,
    documentation: Option<String>,
    replacement_range: TextRange,
    prefix: &str,
) {
    push_completion(
        items,
        CompletionItem {
            label: label.to_owned(),
            kind: if detail == "localisation" {
                CompletionKind::Localisation
            } else {
                CompletionKind::Value
            },
            detail: detail.to_owned(),
            documentation,
            replacement_range,
            insert_text: label.to_owned(),
            sort_score: 4,
            deprecated: false,
        },
        prefix,
    );
}

fn add_numeric_completion(
    items: &mut Vec<CompletionItem>,
    label: Option<&str>,
    detail: &str,
    documentation: Option<String>,
    replacement_range: TextRange,
    prefix: &str,
) {
    if let Some(label) = label {
        add_value_completion(
            items,
            label,
            detail,
            documentation,
            replacement_range,
            prefix,
        );
    }
}

fn semantic_rule_detail(rule: &pdx_rules::SemanticRule) -> String {
    let shape = match rule.shape {
        RuleShape::Node => "block",
        RuleShape::Leaf => "scalar",
        RuleShape::LeafValue => "bare value",
        RuleShape::ValueClause => "value clause",
    };
    format!("semantic rule {shape}")
}

/// Alias with the noun used by several editor adapters.
#[must_use]
pub fn completion(
    snapshot: &AnalysisSnapshot,
    document: &DocumentId,
    position: TextSize,
) -> CompletionResult {
    complete(snapshot, document, position)
}

/// Computes hover information without reading the full contents of another file.
#[must_use]
pub fn hover(
    snapshot: &AnalysisSnapshot,
    document: &DocumentId,
    position: TextSize,
) -> Option<Hover> {
    uncancelled(hover_with_cancellation(
        snapshot,
        document,
        position,
        &CancellationToken::new(),
    ))
}

/// Computes hover information with cooperative cancellation checkpoints.
pub fn hover_with_cancellation(
    snapshot: &AnalysisSnapshot,
    document: &DocumentId,
    position: TextSize,
    cancellation: &CancellationToken,
) -> Result<Option<Hover>, Cancelled> {
    cancellation.checkpoint()?;
    let Some(input) = input_for_document(snapshot, document) else {
        return Ok(None);
    };
    if let Some((definition, reference)) = local_parameter_target(&input, position) {
        let owner_name = input
            .hir
            .as_deref()
            .and_then(|_| {
                semantic_data(snapshot, &input)
                    .definitions
                    .into_iter()
                    .find(|candidate| candidate.symbol.range == definition.owner_range)
            })
            .map(|definition| definition.name);
        let occurrences = input
            .hir
            .as_deref()
            .map(|hir| {
                hir.parameter_references_for_owner(definition.owner_range)
                    .filter(|reference| reference.name.eq_ignore_ascii_case(&definition.name))
                    .count()
            })
            .unwrap_or(0);
        let optional = input.hir.as_deref().is_some_and(|hir| {
            hir.parameter_conditionals().iter().any(|conditional| {
                !conditional.negated
                    && conditional.name.eq_ignore_ascii_case(&definition.name)
                    && conditional.range.start() >= definition.owner_range.start()
                    && conditional.range.end() <= definition.owner_range.end()
            })
        });
        let syntax = match reference.kind {
            pdx_engine::hir::HirParameterReferenceKind::Substitution => "substitution",
            pdx_engine::hir::HirParameterReferenceKind::Conditional => "conditional",
        };
        let owner = owner_name.map_or_else(
            || "scripted definition".to_owned(),
            |name| format!("scripted definition `{name}`"),
        );
        return Ok(Some(Hover {
            contents: format!(
                "### parameter `{}`\n\n- Local to {owner}; inferred from its first use\n- Arity: `{}`\n- Syntax: `{syntax}`\n- Occurrences in owner: {occurrences}",
                definition.name,
                if optional {
                    "optional"
                } else {
                    "required/inferred"
                },
            ),
            range: Some(reference.name_range),
        }));
    }
    let range = word_range(&input.source, position);
    let Some(word) = input
        .source_text(range)
        .map(|word| word.trim_matches('"').to_owned())
    else {
        return Ok(None);
    };
    if word.is_empty() {
        return Ok(None);
    }
    let semantic = semantic_data(snapshot, &input);
    let mut references = semantic.references.iter().filter(|reference| {
        reference.document.as_ref() == Some(document) && contains(reference.range, position)
    });
    if let Some(first) = references.next() {
        let mut best = hover_for_symbol(snapshot, &first.kind, &first.name, range, cancellation)?;
        if !best.contents.contains("#### Localisation preview") {
            for reference in references {
                let hover = hover_for_symbol(
                    snapshot,
                    &reference.kind,
                    &reference.name,
                    range,
                    cancellation,
                )?;
                if hover.contents.contains("#### Localisation preview") {
                    best = hover;
                    break;
                }
            }
        }
        return Ok(Some(best));
    }
    if let Some(definition) = semantic.definitions.iter().find(|definition| {
        definition.document.as_ref() == Some(document)
            && contains(definition.symbol.selection_range, position)
    }) {
        return Ok(Some(hover_for_symbol(
            snapshot,
            &definition.kind,
            &definition.name,
            range,
            cancellation,
        )?));
    }
    cancellation.checkpoint()?;
    if let Some(details) = semantic_rule_hover_at(snapshot, &input, position) {
        return Ok(Some(Hover {
            contents: format!("### PDX property `{word}`\n\n{details}"),
            range: Some(range),
        }));
    }
    if let Some(details) = semantic_value_hover_at(snapshot, &input, position) {
        return Ok(Some(Hover {
            contents: format!("### PDX value `{word}`\n\n{details}"),
            range: Some(range),
        }));
    }
    if is_property_key_at(&input, position) {
        let known = known_keys(snapshot);
        if known.iter().any(|key| key.eq_ignore_ascii_case(&word)) {
            let contents = semantic_rule_documentation(snapshot, &word).map_or_else(
                || format!("### PDX property `{word}`"),
                |details| format!("### PDX property `{word}`\n\n{details}"),
            );
            return Ok(Some(Hover {
                contents,
                range: Some(range),
            }));
        }
    }
    // Do not manufacture a tooltip for every bare word in a script or comment.  A hover is only
    // useful when the parser/HIR/rules have established a semantic role for the token.
    Ok(None)
}

fn is_property_key_at(input: &ParsedInput, position: TextSize) -> bool {
    input.hir.as_deref().is_some_and(|hir| {
        hir.properties()
            .iter()
            .any(|property| contains(property.key_range, position))
    })
}

fn semantic_rule_hover_at(
    snapshot: &AnalysisSnapshot,
    input: &ParsedInput,
    position: TextSize,
) -> Option<String> {
    let context = semantic_completion_context(snapshot, input, position)?;
    let property = context.property.as_ref()?;
    if !contains(property.key_range, position) {
        return None;
    }
    let candidates = semantic_rules_for_completion(snapshot, &context)
        .into_iter()
        .filter(|candidate| {
            !matches!(candidate.rule.shape, RuleShape::LeafValue)
                && semantic_rule_key_matches(
                    snapshot,
                    candidate.rule,
                    candidate.parent_path,
                    &property.key,
                )
        })
        .collect::<Vec<_>>();
    (!candidates.is_empty()).then(|| semantic_rule_hover_for_candidates(snapshot, &candidates))
}

fn semantic_value_hover_at(
    snapshot: &AnalysisSnapshot,
    input: &ParsedInput,
    position: TextSize,
) -> Option<String> {
    let context = semantic_completion_context(snapshot, input, position)?;
    let property = context.property.as_ref()?;
    let (value, value_range) = property.scalar.as_ref()?;
    if !contains(*value_range, position) {
        return None;
    }
    let candidates = semantic_rules_for_completion(snapshot, &context)
        .into_iter()
        .filter(|candidate| {
            matches!(candidate.rule.shape, RuleShape::Leaf)
                && semantic_rule_key_matches(
                    snapshot,
                    candidate.rule,
                    candidate.parent_path,
                    &property.key,
                )
                && candidate
                    .rule
                    .operator
                    .as_deref()
                    .is_none_or(|operator| property.operator.as_deref() == Some(operator))
        })
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return None;
    }
    let accepted = candidates.iter().any(|candidate| {
        semantic_scope_allows(candidate.rule, candidate.scope)
            && semantic_property_matches(snapshot, candidate.rule, property, candidate.scope)
    });
    Some(format!(
        "- property: `{}`\n- value: `{}`\n- validation: `{}`\n\n{}",
        property.key,
        value,
        if accepted {
            "accepted"
        } else {
            "does not match"
        },
        semantic_rule_hover_for_candidates(snapshot, &candidates)
    ))
}

fn semantic_rule_hover_for_candidates(
    snapshot: &AnalysisSnapshot,
    candidates: &[SemanticCompletionRule<'_, '_>],
) -> String {
    let mut sections = Vec::new();
    if candidates.len() > 1 {
        sections.push(format!(
            "#### {} possible semantic meanings",
            candidates.len()
        ));
    }
    for (index, candidate) in candidates.iter().enumerate() {
        let rule = candidate.rule;
        let title = if candidates.len() > 1 {
            format!("##### Candidate {}", index + 1)
        } else {
            "#### Rule".to_owned()
        };
        let mut details = Vec::new();
        details.push(format!("- context: `{}`", rule.context));
        if !candidate.parent_path.is_empty() {
            details.push(format!("- parent: `{}`", candidate.parent_path.join(".")));
        }
        details.push(format!(
            "- shape: `{}`",
            semantic_rule_shape_label(rule.shape)
        ));
        details.push(format!(
            "- value: `{}`",
            semantic_value_hover_label(&rule.value)
        ));
        if let Some(operator) = rule.operator.as_deref() {
            details.push(format!("- operator: `{operator}`"));
        }
        let child_scope = (rule.push_scope.is_some() || !rule.replace_scope.is_empty())
            .then(|| semantic_child_scope(snapshot, candidate.scope, rule));

        let mut scope_details = Vec::new();
        if !rule.allowed_scopes.is_empty() {
            let allowed = rule
                .allowed_scopes
                .iter()
                .map(|scope| format!("`{scope}`"))
                .collect::<Vec<_>>()
                .join(", ");
            let status = if semantic_scope_allows(rule, candidate.scope) {
                "allowed"
            } else {
                "not allowed"
            };
            scope_details.push(format!("- allowed scopes: {allowed}"));
            scope_details.push(format!(
                "- current scope: `{}` ({status})",
                candidate.scope.current
            ));
        }
        if !rule.allowed_scopes.is_empty() || child_scope.is_some() {
            scope_details.push(format!(
                "- scope registers: {}",
                semantic_scope_register_summary(candidate.scope)
            ));
        }
        if let Some(child_scope) = child_scope.as_ref() {
            scope_details.push(format!(
                "- scope transition: `{}` → `{}`",
                candidate.scope.current, child_scope.current
            ));
            for (register, value) in &rule.replace_scope {
                let resolved = resolve_scope_expression_context(snapshot, candidate.scope, value);
                scope_details.push(format!(
                    "- scope register: `{register}` = `{value}` → `{resolved}`"
                ));
            }
            scope_details.push(format!(
                "- scope registers after: {}",
                semantic_scope_register_summary(child_scope)
            ));
        }

        let mut cardinality_details = Vec::new();
        if rule.required {
            cardinality_details.push("- required".to_owned());
        }
        if let Some(min) = rule.min_occurs {
            cardinality_details.push(format!("- minimum occurrences: {min}"));
        }
        if let Some(max) = rule.max_occurs {
            cardinality_details.push(format!("- maximum occurrences: {max}"));
        }
        if let Some(child_context) = rule.child_context.as_deref() {
            details.push(format!("- child context: `{child_context}`"));
        }
        let mut sections_for_rule = vec![title, details.join("\n")];
        if !scope_details.is_empty() {
            sections_for_rule.push(format!("#### Scope\n\n{}", scope_details.join("\n")));
        }
        if !cardinality_details.is_empty() {
            sections_for_rule.push(format!(
                "#### Cardinality\n\n{}",
                cardinality_details.join("\n")
            ));
        }
        if !rule.source_file.is_empty() && rule.line > 0 {
            sections_for_rule.push(format!(
                "#### Provenance\n\n- rule: `{}:{}`",
                rule.source_file, rule.line
            ));
        }
        if !rule.documentation.is_empty() {
            sections_for_rule.push(format!(
                "#### Documentation\n\n{}",
                rule.documentation.join("  \n")
            ));
        }
        sections.push(sections_for_rule.join("\n\n"));
    }
    sections.join("\n\n")
}

fn semantic_scope_register_summary(scope: &ScopeContext) -> String {
    let mut registers = vec![
        format!("ROOT=`{}`", scope.root),
        format!("THIS=`{}`", scope.current),
    ];
    for (depth, value) in scope.from.iter().enumerate() {
        registers.push(format!("{}=`{value}`", "FROM".repeat(depth + 1)));
    }
    for (depth, value) in scope.previous.iter().enumerate() {
        registers.push(format!("{}=`{value}`", "PREV".repeat(depth + 1)));
    }
    let mut registers = registers.into_iter();
    let Some(first) = registers.next() else {
        return String::new();
    };
    let rest = registers
        .map(|register| format!("  - {register}"))
        .collect::<Vec<_>>();
    if rest.is_empty() {
        first
    } else {
        format!("{first}\n{}", rest.join("\n"))
    }
}

fn semantic_rule_shape_label(shape: RuleShape) -> &'static str {
    match shape {
        RuleShape::Node => "block",
        RuleShape::Leaf => "scalar",
        RuleShape::LeafValue => "bare value",
        RuleShape::ValueClause => "value clause",
    }
}

fn semantic_value_hover_label(matcher: &ValueMatcher) -> String {
    match matcher {
        ValueMatcher::AnyScalar => "any scalar".to_owned(),
        ValueMatcher::Exact(value) => format!("exact `{value}`"),
        ValueMatcher::Bool => "bool (`yes` / `no`)".to_owned(),
        ValueMatcher::Int { min, max } => semantic_numeric_hover_label("integer", *min, *max),
        ValueMatcher::Float { min, max } => {
            let bounds = match (min.as_deref(), max.as_deref()) {
                (Some(min), Some(max)) => format!(" in [{min}, {max}]"),
                (Some(min), None) => format!(" >= {min}"),
                (None, Some(max)) => format!(" <= {max}"),
                (None, None) => String::new(),
            };
            format!("float{bounds}")
        }
        ValueMatcher::Type(value) => format!("symbol type `{value}`"),
        ValueMatcher::Enum(value) => format!("enum `{value}`"),
        ValueMatcher::Scope(value) => value
            .as_deref()
            .map_or_else(|| "scope".to_owned(), |value| format!("scope `{value}`")),
        ValueMatcher::Localisation => "localisation key".to_owned(),
        ValueMatcher::Filepath => "filepath".to_owned(),
        ValueMatcher::Dynamic(value) => format!("dynamic value `{value}`"),
        ValueMatcher::DynamicSet(value) => format!("dynamic value set `{value}`"),
        ValueMatcher::Opaque(value) => format!("opaque `{value}`"),
    }
}

fn semantic_numeric_hover_label<T: std::fmt::Display>(
    kind: &str,
    min: Option<T>,
    max: Option<T>,
) -> String {
    let bounds = match (min, max) {
        (Some(min), Some(max)) => format!(" in [{min}, {max}]"),
        (Some(min), None) => format!(" >= {min}"),
        (None, Some(max)) => format!(" <= {max}"),
        (None, None) => String::new(),
    };
    format!("{kind}{bounds}")
}

fn semantic_rule_documentation(snapshot: &AnalysisSnapshot, key: &str) -> Option<String> {
    let mut rules = snapshot
        .rules()
        .model()
        .semantic
        .rules
        .iter()
        .filter(|rule| match &rule.key {
            KeyMatcher::Exact(expected) => expected.eq_ignore_ascii_case(key),
            _ => false,
        })
        .collect::<Vec<_>>();
    rules.sort_by_key(|rule| (&rule.context, &rule.parent_path, &rule.id));
    let rule = rules.into_iter().find(|rule| {
        !rule.documentation.is_empty()
            || rule.required
            || rule.min_occurs.is_some()
            || rule.max_occurs != Some(1)
            || !rule.allowed_scopes.is_empty()
    })?;
    semantic_rule_documentation_for_rule(rule)
}

fn semantic_rule_documentation_for_rule(rule: &pdx_rules::SemanticRule) -> Option<String> {
    let mut sections = Vec::new();
    if !rule.documentation.is_empty() {
        sections.push(format!(
            "#### Documentation\n\n{}",
            rule.documentation.join("  \n")
        ));
    }

    let mut constraints = Vec::new();
    if rule.required {
        constraints.push("- required".to_owned());
    }
    if let Some(min) = rule.min_occurs {
        constraints.push(format!("- minimum occurrences: {min}"));
    }
    if let Some(max) = rule.max_occurs {
        constraints.push(format!("- maximum occurrences: {max}"));
    }
    if !rule.allowed_scopes.is_empty() {
        constraints.push(format!(
            "- scopes: {}",
            rule.allowed_scopes
                .iter()
                .map(|scope| format!("`{scope}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !constraints.is_empty() {
        sections.push(format!("#### Constraints\n\n{}", constraints.join("\n")));
    }
    if !rule.source_file.is_empty() && rule.line > 0 {
        sections.push(format!(
            "#### Provenance\n\n- rule: `{}:{}`",
            rule.source_file, rule.line
        ));
    }
    (!sections.is_empty()).then(|| sections.join("\n\n"))
}

/// Resolves the symbol at a position. Ambiguous and unresolved references deliberately return no
/// location so a client can never be sent to an arbitrary candidate.
#[must_use]
pub fn definition(
    snapshot: &AnalysisSnapshot,
    document: &DocumentId,
    position: TextSize,
) -> Vec<Location> {
    uncancelled(definition_with_cancellation(
        snapshot,
        document,
        position,
        &CancellationToken::new(),
    ))
}

/// Resolves a definition with cooperative cancellation checkpoints.
pub fn definition_with_cancellation(
    snapshot: &AnalysisSnapshot,
    document: &DocumentId,
    position: TextSize,
    cancellation: &CancellationToken,
) -> Result<Vec<Location>, Cancelled> {
    cancellation.checkpoint()?;
    let Some(input) = input_for_document(snapshot, document) else {
        return Ok(Vec::new());
    };
    if let Some((definition, _)) = local_parameter_target(&input, position) {
        return Ok(vec![local_location(&input, definition.name_range)]);
    }
    let all = all_semantics(snapshot, cancellation)?;
    let Some((kind, name)) = symbol_at(&all, document, position) else {
        return Ok(Vec::new());
    };
    Ok(match resolve_symbol(snapshot, &all, &kind, &name) {
        Resolution::Unique(definition) => vec![definition_selection_location(&definition)],
        Resolution::Ambiguous | Resolution::Missing => Vec::new(),
    })
}

/// Returns resolved references for the symbol at a position.
#[must_use]
pub fn references(
    snapshot: &AnalysisSnapshot,
    document: &DocumentId,
    position: TextSize,
    include_declaration: bool,
) -> Vec<Location> {
    uncancelled(references_with_cancellation(
        snapshot,
        document,
        position,
        include_declaration,
        &CancellationToken::new(),
    ))
}

/// Resolves references with cooperative cancellation checkpoints.
pub fn references_with_cancellation(
    snapshot: &AnalysisSnapshot,
    document: &DocumentId,
    position: TextSize,
    include_declaration: bool,
    cancellation: &CancellationToken,
) -> Result<Vec<Location>, Cancelled> {
    cancellation.checkpoint()?;
    let Some(input) = input_for_document(snapshot, document) else {
        return Ok(Vec::new());
    };
    if let Some((definition, _)) = local_parameter_target(&input, position) {
        let Some(hir) = input.hir.as_deref() else {
            return Ok(Vec::new());
        };
        let mut result = Vec::new();
        if include_declaration {
            result.push(local_location(&input, definition.name_range));
        }
        result.extend(
            hir.parameter_references_for_owner(definition.owner_range)
                .filter(|reference| {
                    reference.name.eq_ignore_ascii_case(&definition.name)
                        && reference.name_range != definition.name_range
                })
                .map(|reference| local_location(&input, reference.name_range)),
        );
        return Ok(result);
    }
    let all = all_semantics(snapshot, cancellation)?;
    let Some((kind, name)) = symbol_at(&all, document, position) else {
        return Ok(Vec::new());
    };
    let Resolution::Unique(target) = resolve_symbol(snapshot, &all, &kind, &name) else {
        return Ok(Vec::new());
    };
    let mut result = Vec::new();
    if include_declaration {
        result.push(definition_selection_location(&target));
    }
    for reference in &all.references {
        cancellation.checkpoint()?;
        if reference.kind != kind || !same_name(&reference.name, &name) {
            continue;
        }
        if let Resolution::Unique(candidate) =
            resolve_symbol(snapshot, &all, &kind, &reference.name)
            && same_location(&candidate.location, &target.location)
        {
            result.push(reference.location());
        }
    }
    result.sort_by_key(|location| {
        (
            location
                .path
                .as_ref()
                .map_or(String::new(), |path| path.as_str().to_owned()),
            location.range.start(),
        )
    });
    result.dedup();
    cancellation.checkpoint()?;
    Ok(result)
}

/// Returns the identifier range when the cursor is on a uniquely resolved, writable symbol.
pub fn prepare_rename(
    snapshot: &AnalysisSnapshot,
    document: &DocumentId,
    position: TextSize,
) -> Result<PrepareRenameResult, RenameError> {
    match prepare_rename_with_cancellation(snapshot, document, position, &CancellationToken::new())
    {
        Ok(result) => Ok(result),
        Err(RenameFailure::Rejected(error)) => Err(error),
        Err(RenameFailure::Cancelled) => {
            unreachable!("a fresh cancellation token cannot be cancelled")
        }
    }
}

/// Prepares a rename while allowing the caller to cancel semantic resolution.
pub fn prepare_rename_with_cancellation(
    snapshot: &AnalysisSnapshot,
    document: &DocumentId,
    position: TextSize,
    cancellation: &CancellationToken,
) -> Result<PrepareRenameResult, RenameFailure> {
    cancellation
        .checkpoint()
        .map_err(|Cancelled| RenameFailure::Cancelled)?;
    let input = input_for_document(snapshot, document).ok_or(RenameError::NoSymbol)?;
    if let Some((_, reference)) = local_parameter_target(&input, position) {
        if !writable_location(snapshot, &local_location(&input, reference.name_range)) {
            return Err(RenameError::ReadOnly.into());
        }
        return Ok(PrepareRenameResult {
            range: reference.name_range,
            placeholder: reference.name.clone(),
        });
    }
    let target = rename_target(snapshot, document, position, cancellation)?;
    let placeholder = input
        .source_text(target.cursor_range)
        .ok_or(RenameError::NoSymbol)?
        .to_owned();
    Ok(PrepareRenameResult {
        range: target.cursor_range,
        placeholder,
    })
}

/// Builds a safe, editor-neutral WorkspaceEdit for a semantic rename.
pub fn rename(
    snapshot: &AnalysisSnapshot,
    document: &DocumentId,
    position: TextSize,
    new_name: &str,
) -> Result<WorkspaceEditPlan, RenameError> {
    match rename_with_cancellation(
        snapshot,
        document,
        position,
        new_name,
        &CancellationToken::new(),
    ) {
        Ok(result) => Ok(result),
        Err(RenameFailure::Rejected(error)) => Err(error),
        Err(RenameFailure::Cancelled) => {
            unreachable!("a fresh cancellation token cannot be cancelled")
        }
    }
}

/// Builds a rename plan with cooperative cancellation checkpoints.
pub fn rename_with_cancellation(
    snapshot: &AnalysisSnapshot,
    document: &DocumentId,
    position: TextSize,
    new_name: &str,
    cancellation: &CancellationToken,
) -> Result<WorkspaceEditPlan, RenameFailure> {
    cancellation
        .checkpoint()
        .map_err(|Cancelled| RenameFailure::Cancelled)?;
    if !valid_rename_name(new_name) {
        return Err(RenameError::InvalidName.into());
    }
    let input = input_for_document(snapshot, document).ok_or(RenameError::NoSymbol)?;
    if let Some((definition, _)) = local_parameter_target(&input, position) {
        if !valid_parameter_name(new_name) {
            return Err(RenameError::InvalidName.into());
        }
        if !writable_location(snapshot, &local_location(&input, definition.name_range)) {
            return Err(RenameError::ReadOnly.into());
        }
        let Some(hir) = input.hir.as_deref() else {
            return Err(RenameError::NoSymbol.into());
        };
        if hir
            .parameter_definitions_for_owner(definition.owner_range)
            .any(|candidate| {
                candidate.name_range != definition.name_range
                    && candidate.name.eq_ignore_ascii_case(new_name)
            })
        {
            return Err(RenameError::Conflict.into());
        }
        let mut edits = Vec::new();
        for reference in hir
            .parameter_references_for_owner(definition.owner_range)
            .filter(|reference| reference.name.eq_ignore_ascii_case(&definition.name))
        {
            cancellation
                .checkpoint()
                .map_err(|Cancelled| RenameFailure::Cancelled)?;
            edits.push(WorkspaceTextEdit {
                location: local_location(&input, reference.name_range),
                new_text: new_name.to_owned(),
            });
        }
        edits.sort_by(|left, right| {
            right
                .location
                .range
                .start()
                .cmp(&left.location.range.start())
                .then_with(|| right.location.range.end().cmp(&left.location.range.end()))
        });
        edits.dedup_by(|left, right| left.location == right.location);
        return Ok(WorkspaceEditPlan {
            revision: snapshot.revision(),
            edits,
        });
    }
    let target = rename_target(snapshot, document, position, cancellation)?;
    let all =
        all_semantics(snapshot, cancellation).map_err(|Cancelled| RenameFailure::Cancelled)?;
    check_rename_conflict(snapshot, &all, &target, new_name, cancellation)?;

    let mut edits = vec![WorkspaceTextEdit {
        location: Location {
            range: target.definition.selection_range,
            ..target.definition.location.clone()
        },
        new_text: new_name.to_owned(),
    }];
    let overlay_files = overlay_file_ids(snapshot);
    for reference in &all.references {
        cancellation
            .checkpoint()
            .map_err(|Cancelled| RenameFailure::Cancelled)?;
        if reference.kind != target.kind || !same_name(&reference.name, &target.name) {
            continue;
        }
        // A document overlay replaces its disk candidate.  Do not return edits for the hidden
        // disk text as that would overwrite user changes when the client applies the WorkspaceEdit.
        if reference.document.is_none()
            && reference
                .file
                .is_some_and(|file| overlay_files.contains(&file))
        {
            continue;
        }
        let Resolution::Unique(candidate) =
            resolve_symbol(snapshot, &all, &target.kind, &reference.name)
        else {
            continue;
        };
        if !same_location(&candidate.location, &target.definition.location)
            || !writable_location(snapshot, &reference.location())
        {
            continue;
        }
        edits.push(WorkspaceTextEdit {
            location: reference.location(),
            new_text: new_name.to_owned(),
        });
    }
    edits.sort_by(|left, right| {
        edit_target_key(&left.location)
            .cmp(&edit_target_key(&right.location))
            .then_with(|| {
                right
                    .location
                    .range
                    .start()
                    .cmp(&left.location.range.start())
            })
            .then_with(|| right.location.range.end().cmp(&left.location.range.end()))
    });
    edits
        .dedup_by(|left, right| left.location == right.location && left.new_text == right.new_text);
    cancellation
        .checkpoint()
        .map_err(|Cancelled| RenameFailure::Cancelled)?;
    Ok(WorkspaceEditPlan {
        revision: snapshot.revision(),
        edits,
    })
}

/// Returns symbols declared by one document.
#[must_use]
pub fn document_symbols(snapshot: &AnalysisSnapshot, document: &DocumentId) -> Vec<Symbol> {
    uncancelled(document_symbols_with_cancellation(
        snapshot,
        document,
        &CancellationToken::new(),
    ))
}

/// Returns document symbols with cooperative cancellation checkpoints.
pub fn document_symbols_with_cancellation(
    snapshot: &AnalysisSnapshot,
    document: &DocumentId,
    cancellation: &CancellationToken,
) -> Result<Vec<Symbol>, Cancelled> {
    cancellation.checkpoint()?;
    let Some(input) = input_for_document(snapshot, document) else {
        return Ok(Vec::new());
    };
    let data = semantic_data(snapshot, &input);
    let parameter_count = input
        .hir
        .as_deref()
        .map_or(0, |hir| hir.parameter_definitions().len());
    let mut result = Vec::with_capacity(data.definitions.len() + parameter_count);
    for definition in data.definitions {
        cancellation.checkpoint()?;
        result.push(definition.symbol);
    }
    if let Some(hir) = input.hir.as_deref() {
        for definition in hir.parameter_definitions() {
            cancellation.checkpoint()?;
            result.push(Symbol {
                name: definition.name.clone(),
                kind: "parameter".to_owned(),
                range: definition.range,
                selection_range: definition.name_range,
                location: local_location(&input, definition.range),
            });
        }
    }
    result.sort_by(|left, right| {
        left.range
            .start()
            .cmp(&right.range.start())
            .then_with(|| left.range.end().cmp(&right.range.end()))
            .then_with(|| left.kind.cmp(&right.kind))
            .then_with(|| left.name.cmp(&right.name))
    });
    Ok(result)
}

/// Returns active workspace symbols using deterministic prefix/fuzzy ranking.
#[must_use]
pub fn workspace_symbols(snapshot: &AnalysisSnapshot, query: &str) -> Vec<WorkspaceSymbol> {
    uncancelled(workspace_symbols_with_cancellation(
        snapshot,
        query,
        &CancellationToken::new(),
    ))
}

/// Returns workspace symbols with cooperative cancellation checkpoints.
pub fn workspace_symbols_with_cancellation(
    snapshot: &AnalysisSnapshot,
    query: &str,
    cancellation: &CancellationToken,
) -> Result<Vec<WorkspaceSymbol>, Cancelled> {
    let all = all_semantics(snapshot, cancellation)?;
    let query = query.trim().to_ascii_lowercase();
    let mut result = Vec::new();
    for definition in &all.definitions {
        cancellation.checkpoint()?;
        let name = definition.name.to_ascii_lowercase();
        let score = if query.is_empty() {
            Some(20)
        } else if name.starts_with(&query) {
            Some(0)
        } else if name.contains(&query) {
            Some(10)
        } else if fuzzy_match(&name, &query) {
            Some(30)
        } else {
            None
        };
        if score.is_none() {
            continue;
        }
        if let Resolution::Unique(active) =
            resolve_symbol(snapshot, &all, &definition.kind, &definition.name)
            && same_location(&active.location, &definition.symbol.location)
        {
            result.push((score.unwrap_or(99), definition.symbol.clone()));
        }
    }
    result.sort_by_key(|(score, symbol)| {
        (
            *score,
            symbol.name.to_ascii_lowercase(),
            symbol.kind.clone(),
        )
    });
    cancellation.checkpoint()?;
    Ok(result.into_iter().map(|(_, symbol)| symbol).collect())
}

#[derive(Clone, Debug)]
struct ParsedInput {
    document: Option<DocumentId>,
    file: Option<SourceFileId>,
    path: Option<LogicalPath>,
    format: FileFormat,
    source: Arc<str>,
    parsed: ParsedContent,
    hir: Option<Arc<HirFile>>,
    profile: Arc<GameProfile>,
}

#[derive(Clone, Debug)]
enum ParsedContent {
    Text(Arc<ParsedFile>),
}

impl ParsedInput {
    fn source_text(&self, range: TextRange) -> Option<&str> {
        let start = usize::try_from(range.start()).ok()?;
        let end = usize::try_from(range.end()).ok()?;
        self.source.get(start..end)
    }
}

#[derive(Clone, Debug)]
struct DefinitionInfo {
    kind: String,
    name: String,
    symbol: Symbol,
    document: Option<DocumentId>,
    file: Option<SourceFileId>,
}

#[derive(Clone, Debug)]
struct ReferenceInternal {
    kind: String,
    name: String,
    range: TextRange,
    document: Option<DocumentId>,
    file: Option<SourceFileId>,
    path: Option<LogicalPath>,
}

impl ReferenceInternal {
    fn location(&self) -> Location {
        Location {
            document: self.document.clone(),
            file: self.file,
            path: self.path.clone(),
            range: self.range,
        }
    }
}

#[derive(Clone, Debug, Default)]
struct SemanticWorkspace {
    definitions: Vec<DefinitionInfo>,
    references: Vec<ReferenceInternal>,
}

#[derive(Clone, Debug)]
struct SemanticFile {
    definitions: Vec<DefinitionInfo>,
    references: Vec<ReferenceInternal>,
}

#[derive(Clone, Debug)]
struct PropertyInfo {
    key: String,
    value: Option<(String, TextRange)>,
}

#[derive(Clone, Debug)]
struct ResolutionDefinition {
    location: Location,
    selection_range: TextRange,
    priority: u64,
}

#[derive(Clone, Debug)]
struct RenameTarget {
    kind: String,
    name: String,
    cursor_range: TextRange,
    definition: ResolutionDefinition,
}

enum Resolution {
    Unique(ResolutionDefinition),
    Ambiguous,
    Missing,
}

fn input_for_document(snapshot: &AnalysisSnapshot, id: &DocumentId) -> Option<ParsedInput> {
    let document = snapshot.document(id)?;
    let path = document
        .path()
        .and_then(|path| logical_path(snapshot, path))
        .or_else(|| {
            id.as_str()
                .split(['/', '\\'])
                .next_back()
                .filter(|name| name.contains('.'))
                .and_then(|name| LogicalPath::parse(name).ok())
        });
    let file = document
        .path()
        .and_then(|path| {
            snapshot
                .source_files()
                .values()
                .find(|file| file.physical_path == path)
        })
        .map(|file| file.id);
    let source = document.text_handle();
    let parsed = document.parsed()?;
    let format = parsed.format();
    let parsed = match parsed {
        ParsedSource::Text(parsed) => ParsedContent::Text(Arc::clone(parsed)),
    };
    let hir = document.hir_handle();
    let profile = snapshot.game_profile_handle();
    Some(ParsedInput {
        document: Some(id.clone()),
        file,
        path,
        format,
        source,
        parsed,
        hir,
        profile,
    })
}

fn input_for_source_file(snapshot: &AnalysisSnapshot, id: SourceFileId) -> Option<ParsedInput> {
    let file = snapshot.source_files().get(&id)?;
    let state = snapshot.file_state(id)?;
    let parsed = match state.parsed()? {
        ParsedSource::Text(parsed) => ParsedContent::Text(Arc::clone(parsed)),
    };
    Some(ParsedInput {
        document: None,
        file: Some(id),
        path: Some(file.logical_path.clone()),
        format: state.parsed()?.format(),
        source: state.source_handle(),
        parsed,
        hir: state.hir_handle(),
        profile: snapshot.game_profile_handle(),
    })
}

fn logical_path(snapshot: &AnalysisSnapshot, path: &Path) -> Option<LogicalPath> {
    snapshot
        .source_roots()
        .iter()
        .filter_map(|root| path.strip_prefix(&root.path).ok())
        .filter_map(|relative| LogicalPath::parse(&relative.to_string_lossy()).ok())
        .min_by_key(|path| path.as_str().len())
        .or_else(|| {
            path.file_name()
                .and_then(|name| LogicalPath::parse(&name.to_string_lossy()).ok())
        })
}

fn analyze_input(snapshot: &AnalysisSnapshot, input: &ParsedInput) -> FileAnalysis {
    uncancelled(analyze_input_with_cancellation(
        snapshot,
        input,
        &CancellationToken::new(),
    ))
}

fn analyze_input_with_cancellation(
    snapshot: &AnalysisSnapshot,
    input: &ParsedInput,
    cancellation: &CancellationToken,
) -> Result<FileAnalysis, Cancelled> {
    cancellation.checkpoint()?;
    let semantic = semantic_data(snapshot, input);
    cancellation.checkpoint()?;
    let resolution = DirectResolutionContext::new(snapshot);
    let mut diagnostics = syntax_diagnostics(input);
    diagnostics.extend(semantic_rule_diagnostics(snapshot, input, cancellation)?);
    let mut unknown_scope_reported = false;
    for property in properties(input) {
        cancellation.checkpoint()?;
        if property.key.eq_ignore_ascii_case("scope")
            && let Some((value, range)) = property.value.as_ref()
            && !input.profile.is_scope(value)
            && !unknown_scope_reported
        {
            diagnostics.push(Diagnostic {
                code: DiagnosticCode::UnknownScope,
                severity: DiagnosticCode::UnknownScope.severity(),
                range: *range,
                message: format!("unknown scope `{value}`"),
            });
            unknown_scope_reported = true;
        }
    }
    for reference in &semantic.references {
        cancellation.checkpoint()?;
        match resolution.resolve(&reference.kind, &reference.name) {
            Resolution::Missing => diagnostics.push(Diagnostic {
                code: DiagnosticCode::UnknownSymbol,
                severity: DiagnosticCode::UnknownSymbol.severity(),
                range: reference.range,
                message: format!("unknown {} symbol `{}`", reference.kind, reference.name),
            }),
            Resolution::Ambiguous => diagnostics.push(Diagnostic {
                code: DiagnosticCode::AmbiguousSymbol,
                severity: DiagnosticCode::AmbiguousSymbol.severity(),
                range: reference.range,
                message: format!("ambiguous {} symbol `{}`", reference.kind, reference.name),
            }),
            Resolution::Unique(_) => {}
        }
    }
    diagnostics.sort_by_key(|diagnostic| {
        (
            diagnostic.range.start(),
            diagnostic.range.end(),
            diagnostic.code,
        )
    });
    diagnostics.dedup_by(|left, right| {
        left.code == right.code
            && left.severity == right.severity
            && left.range == right.range
            && left.message == right.message
    });
    cancellation.checkpoint()?;
    Ok(FileAnalysis {
        revision: snapshot.revision(),
        document: input.document.clone(),
        file: input.file,
        format: Some(input.format),
        scope: Scope::Unknown,
        diagnostics,
        symbols: semantic
            .definitions
            .into_iter()
            .map(|definition| definition.symbol)
            .collect(),
        references: semantic
            .references
            .into_iter()
            .map(|reference| {
                let location = reference.location();
                ReferenceInfo {
                    kind: reference.kind,
                    name: reference.name,
                    location,
                }
            })
            .collect(),
    })
}

#[derive(Clone, Debug)]
struct ScriptProperty {
    key: String,
    key_range: TextRange,
    range: TextRange,
    operator: Option<String>,
    scalar: Option<(String, TextRange)>,
    block_range: Option<TextRange>,
    block: Vec<ScriptProperty>,
    bare_values: Vec<(String, TextRange)>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ScopeContext {
    profile: Arc<GameProfile>,
    root: String,
    current: String,
    from: Vec<String>,
    previous: Vec<String>,
}

impl ScopeContext {
    fn new(profile: Arc<GameProfile>) -> Self {
        Self {
            profile,
            root: "any".to_owned(),
            current: "any".to_owned(),
            from: Vec::new(),
            previous: Vec::new(),
        }
    }
}

fn semantic_rule_diagnostics(
    snapshot: &AnalysisSnapshot,
    input: &ParsedInput,
    cancellation: &CancellationToken,
) -> Result<Vec<Diagnostic>, Cancelled> {
    cancellation.checkpoint()?;
    if input.format != FileFormat::Script || snapshot.rules().model().semantic.rules.is_empty() {
        return Ok(Vec::new());
    }
    let ParsedContent::Text(parsed) = &input.parsed;
    let roots = script_properties(input, parsed.root());
    cancellation.checkpoint()?;
    let mut diagnostics = Vec::new();
    for property in roots {
        cancellation.checkpoint()?;
        let Some(context) = semantic_root_context(snapshot, &property.key, input.path.as_ref())
        else {
            continue;
        };
        let scope =
            semantic_initial_scope(snapshot, input, &context, &property.key, property.key_range);
        if let Some(type_name) = context.strip_prefix("type:")
            && snapshot
                .rules()
                .model()
                .semantic
                .type_descriptors
                .get(type_name)
                .is_some_and(|descriptor| {
                    descriptor.skip_root_paths.iter().any(|path| {
                        path.first().is_some_and(|key| {
                            key.eq_ignore_ascii_case("any")
                                || key.eq_ignore_ascii_case(&property.key)
                        })
                    })
                })
        {
            for child in &property.block {
                let child_scope =
                    semantic_initial_scope(snapshot, input, &context, &child.key, child.key_range);
                validate_semantic_container(
                    snapshot,
                    &context,
                    &[],
                    &child.block,
                    &child.bare_values,
                    &child_scope,
                    input.hir.as_deref(),
                    &mut diagnostics,
                    cancellation,
                )?;
            }
            continue;
        }
        validate_semantic_container(
            snapshot,
            &context,
            &[],
            &property.block,
            &property.bare_values,
            &scope,
            input.hir.as_deref(),
            &mut diagnostics,
            cancellation,
        )?;
    }
    Ok(diagnostics)
}

fn semantic_initial_scope(
    snapshot: &AnalysisSnapshot,
    input: &ParsedInput,
    context: &str,
    root_key: &str,
    key_range: TextRange,
) -> ScopeContext {
    if let Some(state) = input
        .hir
        .as_deref()
        .and_then(|hir| hir.scope_fact(key_range, context))
        .map(|fact| &fact.state)
    {
        return scope_context_from_hir(snapshot.game_profile_handle(), state);
    }
    let mut scope = ScopeContext::new(snapshot.game_profile_handle());
    if let Some(type_name) = context.strip_prefix("type:")
        && let Some(root_scope) = snapshot
            .rules()
            .model()
            .semantic
            .type_root_scopes
            .get(type_name)
            .and_then(|roots| roots.get(root_key))
    {
        scope.root.clone_from(root_scope);
        scope.current.clone_from(root_scope);
        return scope;
    }
    if let Some(root_scope) = snapshot.game_profile().root_scope(root_key) {
        scope.root = root_scope.to_owned();
        scope.current = root_scope.to_owned();
    }
    scope
}

fn scope_context_from_hir(profile: Arc<GameProfile>, state: &ScopeState) -> ScopeContext {
    fn spelling(value: &ScopeValue) -> String {
        match value {
            ScopeValue::Known(scopes) if scopes.len() == 1 => scopes[0].clone(),
            ScopeValue::Known(_) => "any".to_owned(),
            ScopeValue::Unknown => "any".to_owned(),
            ScopeValue::Invalid => "invalid".to_owned(),
        }
    }
    ScopeContext {
        profile,
        root: spelling(&state.root),
        current: state
            .current
            .first()
            .map_or_else(|| "any".to_owned(), spelling),
        from: state.from.iter().map(spelling).collect(),
        previous: state.previous.iter().map(spelling).collect(),
    }
}

fn semantic_root_context(
    snapshot: &AnalysisSnapshot,
    key: &str,
    logical_path: Option<&LogicalPath>,
) -> Option<String> {
    hir_semantic_root_context(snapshot.rules(), logical_path, key)
}

fn script_properties(input: &ParsedInput, parent: &CstNode) -> Vec<ScriptProperty> {
    parent
        .children()
        .iter()
        .filter(|node| node.kind() == CstKind::Property)
        .filter_map(|node| {
            let (key, key_range) = property_key(input, node)?;
            let value = node
                .children()
                .iter()
                .find(|child| child.kind() == CstKind::Value);
            let block_node = value.and_then(|value| {
                value
                    .children()
                    .iter()
                    .find(|child| child.kind() == CstKind::Block)
            });
            let block = block_node.map_or_else(Vec::new, |block| script_properties(input, block));
            let bare_values = block_node.map_or_else(Vec::new, |block| {
                block
                    .children()
                    .iter()
                    .filter(|child| {
                        matches!(child.kind(), CstKind::BareValue | CstKind::QuotedString)
                    })
                    .filter_map(|child| {
                        let raw = input.source_text(child.range())?.trim();
                        let value = raw
                            .strip_prefix('"')
                            .and_then(|value| value.strip_suffix('"'))
                            .unwrap_or(raw)
                            .to_owned();
                        Some((value, child.range()))
                    })
                    .collect()
            });
            let operator = node
                .children()
                .iter()
                .find(|child| child.kind() == CstKind::Operator)
                .and_then(|child| input.source_text(child.range()))
                .map(str::to_owned);
            Some(ScriptProperty {
                key,
                key_range,
                range: node.range(),
                operator,
                scalar: property_scalar(input, node),
                block_range: block_node.map(CstNode::range),
                block,
                bare_values,
            })
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn cached_scope_fact_for_property<'hir>(
    snapshot: &AnalysisSnapshot,
    hir: Option<&'hir HirFile>,
    context: &str,
    parent_path: &[String],
    property: &ScriptProperty,
    matching: &[&pdx_rules::SemanticRule],
    selected_alternative: Option<&str>,
    scope: &ScopeContext,
    transparent_wrapper: bool,
) -> Option<&'hir pdx_engine::hir::ScopeFact> {
    let fact = property
        .block
        .iter()
        .find_map(|child| hir.and_then(|hir| hir.scope_fact_at(child.key_range)))?;

    // HIR cannot inspect the workspace while lowering, so a cached dynamic transition is only
    // authoritative once analysis confirms the member. A missing index member is accepted only
    // when the first-party descriptor's negative/positive key filter proves it structurally.
    let mut transition_matching = matching.to_vec();
    if transition_matching.is_empty() {
        transition_matching = semantic_rules_for_container(snapshot, context, parent_path, scope)
            .into_iter()
            .filter(|rule| {
                !matches!(rule.shape, RuleShape::LeafValue)
                    && semantic_scope_allows(rule, scope)
                    && match &rule.key {
                        KeyMatcher::Type(type_name) => {
                            match workspace_type_member(snapshot, type_name, &property.key) {
                                WorkspaceTypeMember::Present => true,
                                WorkspaceTypeMember::Absent => false,
                                WorkspaceTypeMember::Unknown => {
                                    type_member_provably_valid(snapshot, type_name, &property.key)
                                }
                            }
                        }
                        _ => false,
                    }
            })
            .collect();
    }
    let selected = semantic_selected_transition(
        snapshot,
        &transition_matching,
        selected_alternative,
        context,
        parent_path,
        property,
        scope,
        transparent_wrapper,
    )?;
    let (expected_context, expected_path) = semantic_transition_destination(
        selected,
        context,
        parent_path,
        &property.key,
        transparent_wrapper,
    );
    (fact.context.eq_ignore_ascii_case(&expected_context)
        && fact.parent_path.len() == expected_path.len()
        && fact
            .parent_path
            .iter()
            .zip(expected_path)
            .all(|(actual, expected)| actual.eq_ignore_ascii_case(&expected)))
    .then_some(fact)
}

#[allow(clippy::too_many_arguments)] // Recursive validation carries explicit semantic state.
fn validate_semantic_container(
    snapshot: &AnalysisSnapshot,
    context: &str,
    parent_path: &[String],
    properties: &[ScriptProperty],
    bare_values: &[(String, TextRange)],
    scope: &ScopeContext,
    hir: Option<&HirFile>,
    diagnostics: &mut Vec<Diagnostic>,
    cancellation: &CancellationToken,
) -> Result<(), Cancelled> {
    cancellation.checkpoint()?;
    let rules = semantic_rules_for_container(snapshot, context, parent_path, scope);
    if rules.is_empty() {
        return Ok(());
    }
    let selected_alternative = semantic_selected_alternative(
        snapshot,
        &rules,
        parent_path,
        properties,
        bare_values,
        scope,
    );
    let mut counts = std::collections::BTreeMap::<String, u32>::new();
    for property in properties {
        cancellation.checkpoint()?;
        let fact_scope = hir
            .and_then(|hir| hir.scope_fact(property.key_range, context))
            .map(|fact| scope_context_from_hir(snapshot.game_profile_handle(), &fact.state));
        let scope = fact_scope.as_ref().unwrap_or(scope);
        let key = property.key.to_ascii_lowercase();
        let count = counts.entry(key).or_default();
        *count = count.saturating_add(1);
        let transparent_wrapper = context.eq_ignore_ascii_case("trigger")
            && snapshot
                .game_profile()
                .is_transparent_scope_wrapper(&property.key);
        let matching = rules
            .iter()
            .filter(|rule| {
                !matches!(rule.shape, RuleShape::LeafValue)
                    && semantic_rule_key_matches(snapshot, rule, parent_path, &property.key)
            })
            .copied()
            .collect::<Vec<_>>();
        if matching.is_empty() && transparent_wrapper {
            // EU4 logical wrappers (AND/OR/NOT) do not introduce a new rule context or
            // scope. Their children are validated as siblings of the wrapper itself.
        } else if matching.is_empty() {
            diagnostics.push(Diagnostic {
                code: DiagnosticCode::UnknownKey,
                severity: DiagnosticCode::UnknownKey.severity(),
                range: property.key_range,
                message: format!(
                    "unexpected key `{}` in rule context `{context}`",
                    property.key
                ),
            });
        } else {
            let scoped_matching = matching
                .iter()
                .filter(|rule| semantic_scope_allows(rule, scope))
                .copied()
                .collect::<Vec<_>>();
            if scoped_matching.is_empty() {
                diagnostics.push(Diagnostic {
                    code: DiagnosticCode::WrongScope,
                    severity: semantic_rule_severity(
                        matching.iter().copied(),
                        DiagnosticCode::WrongScope,
                    ),
                    range: property.key_range,
                    message: format!(
                        "`{}` is not available in game scope `{}` ({})",
                        property.key,
                        scope.current,
                        semantic_rule_provenance(matching[0])
                    ),
                });
            }
            let applicable = if scoped_matching.is_empty() {
                &matching
            } else {
                &scoped_matching
            };
            let valid = applicable
                .iter()
                .any(|rule| semantic_property_matches(snapshot, rule, property, scope));
            if !valid {
                diagnostics.push(Diagnostic {
                    code: DiagnosticCode::InvalidValue,
                    severity: semantic_rule_severity(
                        applicable.iter().copied(),
                        DiagnosticCode::InvalidValue,
                    ),
                    range: property
                        .scalar
                        .as_ref()
                        .map_or(property.key_range, |(_, range)| *range),
                    message: format!(
                        "value of `{}` does not match the semantic rule ({})",
                        property.key,
                        semantic_rule_provenance(applicable[0])
                    ),
                });
            }
            let max_occurs = applicable
                .iter()
                .filter(|rule| {
                    !semantic_rule_is_alias_definition(rule)
                        && semantic_rule_is_selected(rule, selected_alternative.as_deref())
                })
                .filter_map(|rule| rule.max_occurs)
                .max();
            if let Some(max_occurs) = max_occurs
                && *count > max_occurs
            {
                diagnostics.push(Diagnostic {
                    code: DiagnosticCode::Cardinality,
                    severity: 2,
                    range: property.key_range,
                    message: format!(
                        "`{}` occurs {} times, but rule cardinality allows at most {} ({})",
                        property.key,
                        count,
                        max_occurs,
                        semantic_rule_provenance(applicable[0])
                    ),
                });
            }
        }
        let cached_child_fact = cached_scope_fact_for_property(
            snapshot,
            hir,
            context,
            parent_path,
            property,
            &matching,
            selected_alternative.as_deref(),
            scope,
            transparent_wrapper,
        );
        let destination = if let Some(fact) = cached_child_fact {
            Some((
                fact.context.clone(),
                fact.parent_path.clone(),
                scope_context_from_hir(snapshot.game_profile_handle(), &fact.state),
            ))
        } else {
            semantic_selected_transition(
                snapshot,
                &matching,
                selected_alternative.as_deref(),
                context,
                parent_path,
                property,
                scope,
                transparent_wrapper,
            )
            .map(|rule| {
                let (next_context, child_path) = semantic_transition_destination(
                    rule,
                    context,
                    parent_path,
                    &property.key,
                    transparent_wrapper,
                );
                let next_scope = semantic_child_scope(snapshot, scope, rule);
                (next_context, child_path, next_scope)
            })
        };
        let mut structural_path = parent_path.to_vec();
        if !transparent_wrapper {
            structural_path.push(property.key.clone());
        }
        let Some((next_context, child_path, next_scope)) = destination else {
            let structural_rules =
                semantic_rules_for_container(snapshot, context, &structural_path, scope);
            if !structural_rules.is_empty() {
                validate_semantic_container(
                    snapshot,
                    context,
                    &structural_path,
                    &property.block,
                    &property.bare_values,
                    scope,
                    hir,
                    diagnostics,
                    cancellation,
                )?;
            }
            continue;
        };
        let destination_is_structural = next_context.eq_ignore_ascii_case(context)
            && child_path.len() == structural_path.len()
            && child_path
                .iter()
                .zip(&structural_path)
                .all(|(left, right)| left.eq_ignore_ascii_case(right));
        if !destination_is_structural {
            let structural_rules =
                semantic_rules_for_container(snapshot, context, &structural_path, scope);
            if !structural_rules.is_empty() {
                // Clauses such as `limit` are evaluated after the enclosing scope link has
                // moved to its target, so structural and transitioned children share next_scope.
                let (structural_properties, transition_properties): (Vec<_>, Vec<_>) =
                    property.block.iter().cloned().partition(|child| {
                        structural_rules.iter().any(|rule| {
                            !matches!(rule.shape, RuleShape::LeafValue)
                                && semantic_rule_key_matches(
                                    snapshot,
                                    rule,
                                    &structural_path,
                                    &child.key,
                                )
                        })
                    });
                let (structural_values, transition_values): (Vec<_>, Vec<_>) = property
                    .bare_values
                    .iter()
                    .cloned()
                    .partition(|(value, _)| {
                        structural_rules.iter().any(|rule| {
                            matches!(rule.shape, RuleShape::LeafValue)
                                && semantic_leaf_value_matches(snapshot, rule, value, &next_scope)
                        })
                    });
                validate_semantic_container(
                    snapshot,
                    context,
                    &structural_path,
                    &structural_properties,
                    &structural_values,
                    &next_scope,
                    hir,
                    diagnostics,
                    cancellation,
                )?;
                validate_semantic_container(
                    snapshot,
                    &next_context,
                    &child_path,
                    &transition_properties,
                    &transition_values,
                    &next_scope,
                    hir,
                    diagnostics,
                    cancellation,
                )?;
                continue;
            }
        }
        validate_semantic_container(
            snapshot,
            &next_context,
            &child_path,
            &property.block,
            &property.bare_values,
            &next_scope,
            hir,
            diagnostics,
            cancellation,
        )?;
    }
    for (value, value_range) in bare_values {
        cancellation.checkpoint()?;
        let matching = rules
            .iter()
            .filter(|rule| {
                matches!(rule.shape, RuleShape::LeafValue)
                    && semantic_leaf_value_matches(snapshot, rule, value, scope)
            })
            .copied()
            .collect::<Vec<_>>();
        if matching.is_empty() {
            diagnostics.push(Diagnostic {
                code: DiagnosticCode::InvalidValue,
                severity: DiagnosticCode::InvalidValue.severity(),
                range: *value_range,
                message: format!(
                    "bare value `{value}` does not match the semantic rule value clause"
                ),
            });
        }
    }
    let empty_range = properties
        .first()
        .map_or_else(|| TextRange::empty(0), |property| property.key_range);
    for rule in rules
        .iter()
        .filter(|rule| semantic_scope_allows(rule, scope))
    {
        cancellation.checkpoint()?;
        if !semantic_rule_is_selected(rule, selected_alternative.as_deref()) {
            continue;
        }
        if semantic_rule_is_alias_definition(rule) {
            continue;
        }
        if matches!(rule.shape, RuleShape::LeafValue) {
            let count = bare_values
                .iter()
                .filter(|(value, _)| semantic_leaf_value_matches(snapshot, rule, value, scope))
                .count();
            let count = u32::try_from(count).unwrap_or(u32::MAX);
            if let Some(min_occurs) = semantic_min_occurs(rule)
                && count < min_occurs
            {
                diagnostics.push(Diagnostic {
                    code: DiagnosticCode::Cardinality,
                    severity: semantic_min_cardinality_severity(rule),
                    range: empty_range,
                    message: format!(
                        "semantic rule value clause requires at least {min_occurs} value(s), but `{}` occurs {count} times ({})",
                        semantic_value_matcher_label(&rule.value),
                        semantic_rule_provenance(rule)
                    ),
                });
            }
            if let Some(max_occurs) = rule.max_occurs
                && count > max_occurs
            {
                diagnostics.push(Diagnostic {
                    code: DiagnosticCode::Cardinality,
                    severity: 2,
                    range: bare_values.first().map_or(empty_range, |(_, range)| *range),
                    message: format!(
                        "semantic rule value clause allows at most {max_occurs} value(s), but found {count} ({})",
                        semantic_rule_provenance(rule)
                    ),
                });
            }
            continue;
        }
        let Some(min_occurs) = semantic_min_occurs(rule) else {
            continue;
        };
        let count = properties
            .iter()
            .filter(|property| {
                semantic_rule_key_matches(snapshot, rule, parent_path, &property.key)
                    && !matches!(rule.shape, RuleShape::LeafValue)
            })
            .count();
        let count = u32::try_from(count).unwrap_or(u32::MAX);
        if count < min_occurs {
            diagnostics.push(Diagnostic {
                code: DiagnosticCode::Cardinality,
                severity: semantic_min_cardinality_severity(rule),
                range: empty_range,
                message: format!(
                    "semantic rule requires at least {min_occurs} occurrence(s), but `{}` occurs {count} times ({})",
                    semantic_matcher_label(&rule.key),
                    semantic_rule_provenance(rule)
                ),
            });
        }
    }
    Ok(())
}

fn semantic_rule_is_selected(rule: &pdx_rules::SemanticRule, selected: Option<&str>) -> bool {
    rule.alternative_id
        .as_deref()
        .is_none_or(|alternative| selected == Some(alternative))
}

/// `required` is the declarative shorthand for one minimum occurrence.  Explicit cardinality
/// remains authoritative when both fields are present, which keeps generated rules backwards
/// compatible while making a standalone required field executable.
fn semantic_min_occurs(rule: &pdx_rules::SemanticRule) -> Option<u32> {
    rule.min_occurs.or(rule.required.then_some(1))
}

/// Alias-definition cardinality describes the fields inside one invocation. It must not be
/// applied to sibling invocations in the surrounding effect/trigger container.
fn semantic_rule_is_alias_definition(rule: &pdx_rules::SemanticRule) -> bool {
    rule.alternative_id.as_deref() == Some(rule.id.as_str())
}

#[allow(clippy::too_many_arguments)]
fn semantic_selected_transition<'rule>(
    snapshot: &AnalysisSnapshot,
    matching: &[&'rule pdx_rules::SemanticRule],
    selected_alternative: Option<&str>,
    context: &str,
    parent_path: &[String],
    property: &ScriptProperty,
    scope: &ScopeContext,
    transparent_wrapper: bool,
) -> Option<&'rule pdx_rules::SemanticRule> {
    let applicable = matching
        .iter()
        .copied()
        .filter(|rule| {
            selected_alternative.is_none() || semantic_rule_is_selected(rule, selected_alternative)
        })
        .filter(|rule| semantic_scope_allows(rule, scope))
        .collect::<Vec<_>>();
    if semantic_transitions_equivalent(&applicable) {
        return applicable.first().copied();
    }
    if property.block.is_empty() && property.bare_values.is_empty() {
        return None;
    }

    let mut structural_path = parent_path.to_vec();
    if !transparent_wrapper {
        structural_path.push(property.key.clone());
    }
    let structural_rules = semantic_rules_for_container(snapshot, context, &structural_path, scope);
    let possible = applicable
        .into_iter()
        .filter(|candidate| {
            let (child_context, child_path) = semantic_transition_destination(
                candidate,
                context,
                parent_path,
                &property.key,
                transparent_wrapper,
            );
            let child_scope = semantic_child_scope(snapshot, scope, candidate);
            let child_rules =
                semantic_rules_for_container(snapshot, &child_context, &child_path, &child_scope);
            property.block.iter().all(|child| {
                structural_rules.iter().any(|rule| {
                    !matches!(rule.shape, RuleShape::LeafValue)
                        && semantic_rule_key_matches(snapshot, rule, &structural_path, &child.key)
                }) || child_rules.iter().any(|rule| {
                    !matches!(rule.shape, RuleShape::LeafValue)
                        && semantic_rule_key_matches(snapshot, rule, &child_path, &child.key)
                })
            }) && property.bare_values.iter().all(|(value, _)| {
                structural_rules.iter().any(|rule| {
                    matches!(rule.shape, RuleShape::LeafValue)
                        && semantic_leaf_value_matches(snapshot, rule, value, scope)
                }) || child_rules.iter().any(|rule| {
                    matches!(rule.shape, RuleShape::LeafValue)
                        && semantic_leaf_value_matches(snapshot, rule, value, &child_scope)
                })
            })
        })
        .collect::<Vec<_>>();
    semantic_transitions_equivalent(&possible).then(|| possible[0])
}

fn semantic_transition_destination(
    rule: &pdx_rules::SemanticRule,
    context: &str,
    parent_path: &[String],
    property_key: &str,
    transparent_wrapper: bool,
) -> (String, Vec<String>) {
    rule.child_context.as_deref().map_or_else(
        || {
            let mut child_path = parent_path.to_vec();
            if !transparent_wrapper {
                child_path.push(property_key.to_owned());
            }
            (context.to_owned(), child_path)
        },
        |child_context| (child_context.to_owned(), Vec::new()),
    )
}

fn semantic_transitions_equivalent(rules: &[&pdx_rules::SemanticRule]) -> bool {
    let Some(first) = rules.first() else {
        return false;
    };
    rules.iter().all(|candidate| {
        semantic_optional_text_eq(
            first.child_context.as_deref(),
            candidate.child_context.as_deref(),
        ) && semantic_optional_text_eq(first.push_scope.as_deref(), candidate.push_scope.as_deref())
            && first.replace_scope.len() == candidate.replace_scope.len()
            && first
                .replace_scope
                .iter()
                .all(|(left_register, left_scope)| {
                    candidate
                        .replace_scope
                        .iter()
                        .any(|(right_register, right_scope)| {
                            left_register.eq_ignore_ascii_case(right_register)
                                && left_scope.eq_ignore_ascii_case(right_scope)
                        })
                })
            && candidate
                .replace_scope
                .iter()
                .all(|(right_register, right_scope)| {
                    first
                        .replace_scope
                        .iter()
                        .any(|(left_register, left_scope)| {
                            left_register.eq_ignore_ascii_case(right_register)
                                && left_scope.eq_ignore_ascii_case(right_scope)
                        })
                })
    })
}

fn semantic_optional_text_eq(left: Option<&str>, right: Option<&str>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => left.eq_ignore_ascii_case(right),
        (None, None) => true,
        _ => false,
    }
}

fn semantic_selected_alternative(
    snapshot: &AnalysisSnapshot,
    rules: &[&pdx_rules::SemanticRule],
    parent_path: &[String],
    properties: &[ScriptProperty],
    bare_values: &[(String, TextRange)],
    scope: &ScopeContext,
) -> Option<String> {
    let mut alternatives = Vec::<String>::new();
    for rule in rules {
        if let Some(alternative) = rule.alternative_id.as_ref()
            && !alternatives.iter().any(|known| known == alternative)
        {
            alternatives.push(alternative.clone());
        }
    }
    let mut best: Option<((usize, usize), String)> = None;
    let mut tied = false;
    for alternative in alternatives {
        let group = rules
            .iter()
            .filter(|rule| rule.alternative_id.as_deref() == Some(alternative.as_str()))
            .copied()
            .collect::<Vec<_>>();
        let mut present = 0_usize;
        let mut valid = 0_usize;
        for property in properties {
            let matching = group.iter().filter(|rule| {
                !matches!(rule.shape, RuleShape::LeafValue)
                    && semantic_rule_key_matches(snapshot, rule, parent_path, &property.key)
            });
            if matching.clone().next().is_some() {
                present += 1;
            }
            if matching
                .filter(|rule| semantic_scope_allows(rule, scope))
                .any(|rule| semantic_property_matches(snapshot, rule, property, scope))
            {
                valid += 1;
            }
        }
        valid += bare_values
            .iter()
            .filter(|(value, _)| {
                group.iter().any(|rule| {
                    matches!(rule.shape, RuleShape::LeafValue)
                        && semantic_leaf_value_matches(snapshot, rule, value, scope)
                })
            })
            .count();
        let score = (valid, present);
        match best.as_ref() {
            None => {
                best = Some((score, alternative));
                tied = false;
            }
            Some((current, _)) if score > *current => {
                best = Some((score, alternative));
                tied = false;
            }
            Some((current, _)) if score == *current => tied = true,
            Some(_) => {}
        }
    }
    if tied {
        None
    } else {
        best.map(|(_, alternative)| alternative)
    }
}

fn semantic_leaf_value_matches(
    snapshot: &AnalysisSnapshot,
    rule: &pdx_rules::SemanticRule,
    value: &str,
    scope: &ScopeContext,
) -> bool {
    match &rule.value {
        ValueMatcher::Dynamic(kind) => semantic_dynamic_value_matches(snapshot, kind, value, scope),
        ValueMatcher::DynamicSet(_) => !value.is_empty(),
        matcher => matcher.matches(
            value,
            |type_name, member| workspace_member(snapshot, type_name, member),
            |enum_name, member| enum_member(snapshot, enum_name, member),
            |scope_name, member| scope_member(scope_name, member, scope),
        ),
    }
}

fn semantic_value_matcher_label(matcher: &ValueMatcher) -> String {
    match matcher {
        ValueMatcher::AnyScalar => "scalar".to_owned(),
        ValueMatcher::Exact(value) => value.clone(),
        ValueMatcher::Bool => "bool".to_owned(),
        ValueMatcher::Int { .. } => "int".to_owned(),
        ValueMatcher::Float { .. } => "float".to_owned(),
        ValueMatcher::Type(value) => format!("<{value}>"),
        ValueMatcher::Enum(value) => format!("enum[{value}]"),
        ValueMatcher::Scope(value) => value
            .as_deref()
            .map_or_else(|| "scope".to_owned(), |value| format!("scope[{value}]")),
        ValueMatcher::Localisation => "localisation".to_owned(),
        ValueMatcher::Filepath => "filepath".to_owned(),
        ValueMatcher::Dynamic(value) => format!("value[{value}]"),
        ValueMatcher::DynamicSet(value) => format!("value_set[{value}]"),
        ValueMatcher::Opaque(value) => value.clone(),
    }
}

fn semantic_rule_provenance(rule: &pdx_rules::SemanticRule) -> String {
    format!("rule {}:{}", rule.source_file, rule.line)
}

fn semantic_matcher_label(matcher: &KeyMatcher) -> String {
    match matcher {
        KeyMatcher::Exact(value) => value.clone(),
        KeyMatcher::Type(value) => format!("<{value}>"),
        KeyMatcher::Enum(value) => format!("enum[{value}]"),
        KeyMatcher::AnyScalar => "scalar".to_owned(),
        KeyMatcher::Dynamic(value) => format!("value_set[{value}]"),
    }
}

fn semantic_parent_path_matches(
    snapshot: &AnalysisSnapshot,
    expected: &[String],
    actual: &[String],
) -> bool {
    expected.len() == actual.len()
        && expected.iter().zip(actual).all(|(expected, actual)| {
            if let Some(type_name) = expected
                .strip_prefix('<')
                .and_then(|name| name.strip_suffix('>'))
            {
                workspace_member(snapshot, type_name, actual)
            } else if let Some(enum_name) = expected
                .strip_prefix("enum[")
                .and_then(|name| name.strip_suffix(']'))
            {
                enum_member(snapshot, enum_name, actual)
            } else {
                expected.eq_ignore_ascii_case(actual)
            }
        })
}

fn semantic_rule_severity<'a>(
    rules: impl IntoIterator<Item = &'a pdx_rules::SemanticRule>,
    fallback: DiagnosticCode,
) -> u8 {
    rules
        .into_iter()
        .filter_map(|rule| rule.severity)
        .min()
        .unwrap_or_else(|| fallback.severity())
}

fn semantic_min_cardinality_severity(rule: &pdx_rules::SemanticRule) -> u8 {
    if !rule.strict_min {
        2
    } else {
        rule.severity
            .unwrap_or(DiagnosticCode::Cardinality.severity())
    }
}

fn semantic_scope_allows(rule: &pdx_rules::SemanticRule, scope: &ScopeContext) -> bool {
    rule.allowed_scopes.is_empty()
        || rule
            .allowed_scopes
            .iter()
            .any(|expected| scope.profile.scopes_compatible(&scope.current, expected))
}

fn semantic_child_scope(
    snapshot: &AnalysisSnapshot,
    parent: &ScopeContext,
    rule: &pdx_rules::SemanticRule,
) -> ScopeContext {
    let mut child = parent.clone();
    if let Some(push_scope) = &rule.push_scope
        && !push_scope.eq_ignore_ascii_case("any")
    {
        child.previous.insert(0, child.current.clone());
        child.current.clone_from(push_scope);
    }
    for (register, value) in &rule.replace_scope {
        let value = resolve_scope_expression_context(snapshot, &child, value);
        let register = register.to_ascii_lowercase().replace('_', "");
        match register.as_str() {
            "root" => child.root = value,
            "this" => child.current = value,
            _ => {
                if let Some(depth) = repeated_scope_register_depth(&register, "from") {
                    set_scope_register(&mut child.from, depth, &value);
                } else if let Some(depth) = repeated_scope_register_depth(&register, "previous")
                    .or_else(|| repeated_scope_register_depth(&register, "prev"))
                {
                    set_scope_register(&mut child.previous, depth, &value);
                }
            }
        }
    }
    child
}

fn resolve_scope_expression_context(
    snapshot: &AnalysisSnapshot,
    context: &ScopeContext,
    expression: &str,
) -> String {
    if expression.contains('.') {
        let mut segments = expression.split('.');
        let Some(first) = segments.next() else {
            return "any".to_owned();
        };
        let mut value = resolve_scope_expression_context(snapshot, context, first);
        for segment in segments {
            value = resolve_scope_link_context(snapshot, context, &value, segment)
                .unwrap_or_else(|| "any".to_owned());
            if value.eq_ignore_ascii_case("any") {
                break;
            }
        }
        return value;
    }

    let lowered = expression.to_ascii_lowercase().replace('_', "");
    if lowered == "root" {
        return context.root.clone();
    }
    if lowered == "this" {
        return context.current.clone();
    }
    if let Some(depth) = repeated_scope_register_depth(&lowered, "from") {
        return context
            .from
            .get(depth)
            .cloned()
            .unwrap_or_else(|| "any".to_owned());
    }
    if let Some(depth) = repeated_scope_register_depth(&lowered, "previous")
        .or_else(|| repeated_scope_register_depth(&lowered, "prev"))
    {
        return context
            .previous
            .get(depth)
            .cloned()
            .unwrap_or_else(|| "any".to_owned());
    }

    let link_expression = snapshot
        .rules()
        .exact_semantic_rules(expression)
        .any(|rule| {
            matches!(
                rule.context.to_ascii_lowercase().as_str(),
                "effect" | "trigger"
            ) && rule.push_scope.is_some()
        });
    if let Some(target) =
        resolve_scope_link_context(snapshot, context, &context.current, expression)
    {
        return target;
    }
    if expression.eq_ignore_ascii_case("any") || link_expression {
        "any".to_owned()
    } else if context.profile.is_scope(expression) {
        expression.to_owned()
    } else {
        "any".to_owned()
    }
}

fn resolve_scope_link_context(
    snapshot: &AnalysisSnapshot,
    context: &ScopeContext,
    current: &str,
    expression: &str,
) -> Option<String> {
    let mut targets = snapshot
        .rules()
        .exact_semantic_rules(expression)
        .filter_map(|rule| {
            if !matches!(
                rule.context.to_ascii_lowercase().as_str(),
                "effect" | "trigger"
            ) || !rule.allowed_scopes.is_empty()
                && !rule
                    .allowed_scopes
                    .iter()
                    .any(|expected| context.profile.scopes_compatible(current, expected))
            {
                return None;
            }
            rule.push_scope
                .as_deref()
                .filter(|target| !target.eq_ignore_ascii_case("any"))
                .map(str::to_owned)
        })
        .collect::<Vec<_>>();
    targets.sort_by_key(|target| target.to_ascii_lowercase());
    targets.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
    if targets.len() == 1 {
        Some(targets.remove(0))
    } else {
        None
    }
}

fn set_scope_register(registers: &mut Vec<String>, depth: usize, value: &str) {
    if registers.len() <= depth {
        registers.resize(depth + 1, "any".to_owned());
    }
    registers[depth] = value.to_owned();
}

fn repeated_scope_register_depth(value: &str, token: &str) -> Option<usize> {
    let count = value.len().checked_div(token.len())?;
    if count > 0 && token.repeat(count) == value {
        Some(count - 1)
    } else {
        None
    }
}

fn semantic_rule_key_matches(
    snapshot: &AnalysisSnapshot,
    rule: &pdx_rules::SemanticRule,
    parent_path: &[String],
    key: &str,
) -> bool {
    qualified_parameter_names(snapshot, rule, parent_path).map_or_else(
        || semantic_key_matches(snapshot, &rule.key, key),
        |names| names.iter().any(|name| name.eq_ignore_ascii_case(key)),
    )
}

fn qualified_parameter_names(
    snapshot: &AnalysisSnapshot,
    rule: &pdx_rules::SemanticRule,
    parent_path: &[String],
) -> Option<Vec<String>> {
    let KeyMatcher::Enum(enum_name) = &rule.key else {
        return None;
    };
    if !enum_name.eq_ignore_ascii_case("scripted_effect_params") {
        return None;
    }
    let owner_kind = rule
        .parent_path
        .last()
        .and_then(|segment| segment.strip_prefix('<'))
        .and_then(|segment| segment.strip_suffix('>'))?;
    if !matches!(
        owner_kind.to_ascii_lowercase().as_str(),
        "scripted_effect" | "scripted_trigger"
    ) {
        return None;
    }
    let owner_name = parent_path.last()?;
    parameter_names_for_owner(snapshot, owner_kind, owner_name)
}

fn parameter_names_for_owner(
    snapshot: &AnalysisSnapshot,
    owner_kind: &str,
    owner_name: &str,
) -> Option<Vec<String>> {
    let mut overlay_candidates = Vec::new();
    for document in snapshot
        .documents()
        .values()
        .filter(|document| document.source() == DocumentSource::Overlay)
    {
        let Some(hir) = document.hir_handle() else {
            continue;
        };
        for definition in hir.definitions().iter().filter(|definition| {
            definition.kind.eq_ignore_ascii_case(owner_kind)
                && definition.name.eq_ignore_ascii_case(owner_name)
        }) {
            overlay_candidates.push((Arc::clone(&hir), definition.range));
        }
    }
    if overlay_candidates.len() > 1 {
        return None;
    }
    if let Some((hir, owner_range)) = overlay_candidates.pop() {
        return Some(parameter_names_in_hir(&hir, owner_range));
    }

    let definition = snapshot.index().active_definition(owner_kind, owner_name)?;
    let source_file = snapshot.source_files().get(&definition.file_id)?;
    let hidden_by_overlay = snapshot.documents().values().any(|document| {
        document.source() == DocumentSource::Overlay
            && document
                .path()
                .is_some_and(|path| path == source_file.physical_path)
    });
    if hidden_by_overlay {
        return None;
    }
    let hir = snapshot.file_state(definition.file_id)?.hir()?;
    Some(parameter_names_in_hir(hir, definition.range))
}

fn parameter_names_in_hir(hir: &HirFile, owner_range: TextRange) -> Vec<String> {
    let mut names = hir
        .parameter_definitions_for_owner(owner_range)
        .map(|definition| definition.name.clone())
        .collect::<Vec<_>>();
    names.sort_by_key(|name| name.to_ascii_lowercase());
    names.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
    names
}

fn semantic_key_matches(snapshot: &AnalysisSnapshot, matcher: &KeyMatcher, key: &str) -> bool {
    matcher.matches(
        key,
        |type_name, member| workspace_member(snapshot, type_name, member),
        |enum_name, member| enum_member(snapshot, enum_name, member),
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WorkspaceTypeMember {
    /// The workspace has a definition for this type member.
    Present,
    /// The workspace has definitions for the type, but not this member.
    Absent,
    /// No definition for the type is indexed yet; keep the conservative open-world fallback.
    Unknown,
}

fn workspace_type_member(
    snapshot: &AnalysisSnapshot,
    type_name: &str,
    member: &str,
) -> WorkspaceTypeMember {
    let base = type_name
        .split_once('.')
        .map_or(type_name, |(kind, _)| kind);
    let candidates = [
        type_name,
        base,
        snapshot
            .game_profile()
            .member_kind_alias(base)
            .unwrap_or(base),
    ];
    let mut has_members = false;
    for candidate in candidates {
        if snapshot
            .index()
            .definitions_for_kind(candidate)
            .next()
            .is_some()
        {
            has_members = true;
            if !snapshot.index().definitions(candidate, member).is_empty() {
                return WorkspaceTypeMember::Present;
            }
        }
    }
    if has_members {
        WorkspaceTypeMember::Absent
    } else {
        WorkspaceTypeMember::Unknown
    }
}

fn type_member_provably_valid(snapshot: &AnalysisSnapshot, type_name: &str, key: &str) -> bool {
    snapshot
        .rules()
        .model()
        .semantic
        .type_descriptors
        .get(type_name)
        .and_then(|descriptor| descriptor.type_key_filter.as_ref())
        .is_some_and(|(values, negate)| {
            values.iter().any(|value| value.eq_ignore_ascii_case(key)) != *negate
        })
}

fn semantic_property_matches(
    snapshot: &AnalysisSnapshot,
    rule: &pdx_rules::SemanticRule,
    property: &ScriptProperty,
    scope_context: &ScopeContext,
) -> bool {
    let shape_matches = match rule.shape {
        RuleShape::Node => property.block_range.is_some(),
        RuleShape::ValueClause => {
            property.block_range.is_some() && !property.bare_values.is_empty()
        }
        RuleShape::Leaf | RuleShape::LeafValue => property.scalar.is_some(),
    };
    if !shape_matches {
        return false;
    }
    if rule
        .operator
        .as_deref()
        .is_some_and(|operator| property.operator.as_deref() != Some(operator))
    {
        return false;
    }
    let Some((value, _)) = property.scalar.as_ref() else {
        return matches!(
            rule.value,
            ValueMatcher::AnyScalar | ValueMatcher::Opaque(_)
        );
    };
    if let ValueMatcher::Dynamic(kind) = &rule.value {
        return semantic_dynamic_value_matches(snapshot, kind, value, scope_context);
    }
    if let ValueMatcher::DynamicSet(_) = &rule.value {
        return !value.is_empty();
    }
    rule.value.matches(
        value,
        |type_name, member| workspace_member(snapshot, type_name, member),
        |enum_name, member| enum_member(snapshot, enum_name, member),
        |scope, member| scope_member(scope, member, scope_context),
    )
}

fn semantic_dynamic_value_matches(
    snapshot: &AnalysisSnapshot,
    kind: &str,
    value: &str,
    scope_context: &ScopeContext,
) -> bool {
    let kind = kind.to_ascii_lowercase();
    if kind == "scope_field" {
        return scope_member(None, value, scope_context)
            || workspace_member(snapshot, "variable_name", value);
    }
    if kind == "variable" {
        return value.parse::<f64>().is_ok()
            || value.starts_with('$')
            || workspace_member(snapshot, "variable", value)
            || workspace_member(snapshot, "variable_name", value);
    }
    if kind == "value" {
        return value.parse::<f64>().is_ok()
            || value.starts_with('$')
            || workspace_member(snapshot, "variable", value)
            || workspace_member(snapshot, "variable_name", value);
    }
    if value.starts_with('$') && value.ends_with('$') {
        return true;
    }
    enum_member(snapshot, &kind, value) || workspace_member(snapshot, &kind, value)
}

fn workspace_member(snapshot: &AnalysisSnapshot, type_name: &str, member: &str) -> bool {
    let base = type_name
        .split_once('.')
        .map_or(type_name, |(kind, _)| kind);
    let candidates = [
        type_name,
        base,
        snapshot
            .game_profile()
            .member_kind_alias(base)
            .unwrap_or(base),
    ];
    candidates
        .iter()
        .any(|candidate| !snapshot.index().definitions(candidate, member).is_empty())
}

fn enum_member(snapshot: &AnalysisSnapshot, enum_name: &str, member: &str) -> bool {
    let static_member = snapshot
        .rules()
        .model()
        .semantic
        .enum_values
        .get(enum_name)
        .is_some_and(|values| {
            values
                .iter()
                .any(|value| value.eq_ignore_ascii_case(member))
        });
    static_member
        || snapshot.game_profile().enum_extra_member(enum_name, member)
        || snapshot
            .game_profile()
            .member_kind_alias(enum_name)
            .is_some_and(|kind| workspace_member(snapshot, kind, member))
        || workspace_member(snapshot, enum_name, member)
}

fn scope_member(scope: Option<&str>, member: &str, context: &ScopeContext) -> bool {
    let lowered = member.to_ascii_lowercase().replace('_', "");
    let resolved = if lowered == "root" {
        Some(context.root.as_str())
    } else if lowered == "this" {
        Some(context.current.as_str())
    } else if let Some(depth) = repeated_scope_register_depth(&lowered, "from") {
        context.from.get(depth).map(String::as_str)
    } else if let Some(depth) = repeated_scope_register_depth(&lowered, "previous")
        .or_else(|| repeated_scope_register_depth(&lowered, "prev"))
    {
        context.previous.get(depth).map(String::as_str)
    } else {
        Some(member)
    };
    let Some(resolved) = resolved else {
        return false;
    };
    context.profile.is_scope(resolved)
        && scope.is_none_or(|expected| context.profile.scopes_compatible(resolved, expected))
}

fn syntax_diagnostics(input: &ParsedInput) -> Vec<Diagnostic> {
    match &input.parsed {
        ParsedContent::Text(parsed) => parsed.errors().iter().map(diagnostic_from_syntax).collect(),
    }
}

fn diagnostic_from_syntax(error: &SyntaxError) -> Diagnostic {
    Diagnostic {
        code: DiagnosticCode::Syntax,
        severity: DiagnosticCode::Syntax.severity(),
        range: error.range,
        message: error.message.clone(),
    }
}

fn semantic_data(snapshot: &AnalysisSnapshot, input: &ParsedInput) -> SemanticFile {
    let mut data = SemanticFile {
        definitions: Vec::new(),
        references: Vec::new(),
    };
    let Some(hir) = input.hir.as_deref() else {
        return data;
    };
    // The inactive-range set is only consulted for Semantic-origin references; skip building it
    // entirely when this file has none, keeping semantic_data O(references + definitions).
    let has_semantic_references = hir
        .references()
        .iter()
        .any(|reference| reference.origin == HirReferenceOrigin::Semantic);
    let inactive_semantic_references = if has_semantic_references {
        inactive_semantic_reference_ranges(snapshot, hir)
    } else {
        BTreeSet::new()
    };
    for definition in hir.definitions() {
        data.definitions.push(make_definition(
            input,
            &definition.kind,
            definition.name.clone(),
            definition.range,
            definition.selection_range,
        ));
    }
    for reference in hir
        .references()
        .iter()
        .filter(|reference| {
            matches!(
                reference.origin,
                HirReferenceOrigin::Profile
                    | HirReferenceOrigin::Semantic
                    | HirReferenceOrigin::DerivedLocalisation
            )
        })
        .filter(|reference| semantic_reference_is_active(&inactive_semantic_references, reference))
    {
        data.references.push(ReferenceInternal {
            kind: reference.kind.clone(),
            name: reference.name.clone(),
            range: reference.range,
            document: input.document.clone(),
            file: input.file,
            path: input.path.clone(),
        });
    }
    data
}

fn inactive_semantic_reference_ranges(
    snapshot: &AnalysisSnapshot,
    hir: &HirFile,
) -> BTreeSet<TextRange> {
    let mut inactive = BTreeSet::new();
    let mut invalid_ancestors = Vec::<(Vec<String>, TextRange)>::new();
    // Container rule sets are identical for every property in one (context, parent_path). Cache
    // them per context and path so dynamic members (e.g. one container per mission) do not
    // rebuild and re-filter the container rules for every property.
    let mut cached_containers =
        HashMap::<String, HashMap<Vec<String>, ContainerRuleCache<'_>>>::new();
    for property in hir.properties() {
        while invalid_ancestors.last().is_some_and(|(path, range)| {
            !property.path.starts_with(path) || !text_range_within(property.range, *range)
        }) {
            invalid_ancestors.pop();
        }
        let own_invalid =
            semantic_type_property_is_invalid(snapshot, hir, property, &mut cached_containers);
        if (!invalid_ancestors.is_empty() || own_invalid)
            && let Some(scalar) = property.scalar.as_ref()
        {
            inactive.insert(scalar.range);
        }
        if own_invalid {
            invalid_ancestors.push((property.path.clone(), property.range));
        }
    }
    inactive
}

/// One (context, parent_path) container's rule set, with derived fast-path indexes so the
/// per-property validity check does not rescan the container rules for every property.
struct ContainerRuleCache<'a> {
    rules: Vec<&'a pdx_rules::SemanticRule>,
    /// Lowercased keys of non-leaf exact rules; a property key in this set is valid by a concrete
    /// match without scanning `rules`.
    concrete_keys: HashSet<String>,
    /// Whether any concrete non-leaf rule uses an `AnyScalar` matcher (matches every key).
    any_scalar_concrete: bool,
    /// Whether the container carries concrete non-leaf rules at all (enum/qualified matchers that
    /// the key set cannot express still need the scan).
    has_concrete: bool,
    /// Whether the container carries `Type` matchers, the only rules the workspace check applies
    /// to.
    has_type: bool,
}

fn semantic_type_property_is_invalid<'a>(
    snapshot: &'a AnalysisSnapshot,
    hir: &HirFile,
    property: &pdx_engine::hir::HirProperty,
    cached_containers: &mut HashMap<String, HashMap<Vec<String>, ContainerRuleCache<'a>>>,
) -> bool {
    if property.path.len() <= 1 {
        return false;
    }
    let Some(fact) = hir.scope_fact_at(property.key_range) else {
        return false;
    };
    let by_path = match cached_containers.get_mut(fact.context.as_str()) {
        Some(by_path) => by_path,
        None => cached_containers.entry(fact.context.clone()).or_default(),
    };
    if !by_path.contains_key(fact.parent_path.as_slice()) {
        // `semantic_rules_for_container` ignores its scope argument; build it once per container
        // only so the caller does not allocate a scope context for every property.
        let scope = scope_context_from_hir(snapshot.game_profile_handle(), &fact.state);
        let rules =
            semantic_rules_for_container(snapshot, &fact.context, &fact.parent_path, &scope);
        let mut concrete_keys = HashSet::new();
        let mut any_scalar_concrete = false;
        let mut has_concrete = false;
        let mut has_type = false;
        for rule in &rules {
            match &rule.key {
                KeyMatcher::Type(_) => has_type = true,
                KeyMatcher::Dynamic(_) => {}
                KeyMatcher::Exact(key) if !matches!(rule.shape, RuleShape::LeafValue) => {
                    has_concrete = true;
                    concrete_keys.insert(key.to_ascii_lowercase());
                }
                KeyMatcher::AnyScalar if !matches!(rule.shape, RuleShape::LeafValue) => {
                    has_concrete = true;
                    any_scalar_concrete = true;
                }
                KeyMatcher::Exact(_) | KeyMatcher::AnyScalar | KeyMatcher::Enum(_) => {}
            }
        }
        by_path.insert(
            fact.parent_path.clone(),
            ContainerRuleCache {
                rules,
                concrete_keys,
                any_scalar_concrete,
                has_concrete,
                has_type,
            },
        );
    }
    let entry = by_path
        .get(fact.parent_path.as_slice())
        .expect("filled above");
    if entry.any_scalar_concrete
        || entry
            .concrete_keys
            .contains(&property.key.to_ascii_lowercase())
    {
        return false;
    }
    if entry.has_concrete
        && entry.rules.iter().any(|rule| {
            !matches!(rule.key, KeyMatcher::Type(_) | KeyMatcher::Dynamic(_))
                && !matches!(rule.shape, RuleShape::LeafValue)
                && semantic_rule_key_matches(snapshot, rule, &fact.parent_path, &property.key)
        })
    {
        return false;
    }
    // The workspace check below only fires for containers that actually carry Type matchers.
    if !entry.has_type {
        return false;
    }
    entry.rules.iter().any(|rule| {
        let KeyMatcher::Type(type_name) = &rule.key else {
            return false;
        };
        match workspace_type_member(snapshot, type_name, &property.key) {
            WorkspaceTypeMember::Present => false,
            WorkspaceTypeMember::Absent => true,
            WorkspaceTypeMember::Unknown => {
                !type_member_provably_valid(snapshot, type_name, &property.key)
            }
        }
    })
}

fn semantic_reference_is_active(
    inactive_semantic_references: &BTreeSet<TextRange>,
    reference: &HirReference,
) -> bool {
    if reference.origin != HirReferenceOrigin::Semantic {
        return true;
    }
    !inactive_semantic_references.contains(&reference.range)
}

fn text_range_within(inner: TextRange, outer: TextRange) -> bool {
    outer.start() <= inner.start() && inner.end() <= outer.end()
}

fn make_definition(
    input: &ParsedInput,
    kind: &str,
    name: String,
    range: TextRange,
    selection_range: TextRange,
) -> DefinitionInfo {
    let location = Location {
        document: input.document.clone(),
        file: input.file,
        path: input.path.clone(),
        range,
    };
    DefinitionInfo {
        kind: kind.to_owned(),
        name: name.clone(),
        symbol: Symbol {
            name,
            kind: kind.to_owned(),
            range,
            selection_range,
            location,
        },
        document: input.document.clone(),
        file: input.file,
    }
}

fn property_key(input: &ParsedInput, node: &CstNode) -> Option<(String, TextRange)> {
    let key = node
        .children()
        .iter()
        .find(|child| child.kind() == CstKind::Key)?;
    let text = text(input, key.range())?.trim().to_owned();
    Some((text, key.range()))
}

fn property_scalar(input: &ParsedInput, node: &CstNode) -> Option<(String, TextRange)> {
    let value = node
        .children()
        .iter()
        .find(|child| child.kind() == CstKind::Value)?;
    let scalar = value
        .children()
        .iter()
        .find(|child| matches!(child.kind(), CstKind::BareValue | CstKind::QuotedString))?;
    let raw = text(input, scalar.range())?.trim();
    let value = raw
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .unwrap_or(raw)
        .to_owned();
    Some((value, scalar.range()))
}

fn text(input: &ParsedInput, range: TextRange) -> Option<&str> {
    input.source_text(range)
}

fn properties(input: &ParsedInput) -> Vec<PropertyInfo> {
    input.hir.as_deref().map_or_else(Vec::new, |hir| {
        hir.properties()
            .iter()
            .map(|property| PropertyInfo {
                key: property.key.clone(),
                value: property
                    .scalar
                    .as_ref()
                    .map(|scalar| (scalar.value.clone(), scalar.range)),
            })
            .collect()
    })
}

fn all_semantics(
    snapshot: &AnalysisSnapshot,
    cancellation: &CancellationToken,
) -> Result<SemanticWorkspace, Cancelled> {
    #[cfg(test)]
    ALL_SEMANTICS_CALLS.with(|calls| calls.set(calls.get().saturating_add(1)));
    let mut all = SemanticWorkspace::default();
    for definition in snapshot.index().definitions_iter() {
        cancellation.checkpoint()?;
        all.definitions
            .push(index_definition_info(snapshot, definition));
    }
    for reference in snapshot.index().references_iter() {
        cancellation.checkpoint()?;
        let path = snapshot
            .source_files()
            .get(&reference.file_id)
            .map(|file| file.logical_path.clone());
        all.references.push(ReferenceInternal {
            kind: reference.kind.clone(),
            name: reference.name.clone(),
            range: reference.range,
            document: None,
            file: Some(reference.file_id),
            path,
        });
    }
    for document in snapshot.documents().values() {
        cancellation.checkpoint()?;
        if document.source() != DocumentSource::Overlay {
            continue;
        }
        if let Some(input) = input_for_document(snapshot, document.id()) {
            let semantic = semantic_data(snapshot, &input);
            all.definitions.extend(semantic.definitions);
            all.references.extend(semantic.references);
        }
    }
    Ok(all)
}

#[cfg(test)]
thread_local! {
    static ALL_SEMANTICS_CALLS: Cell<usize> = const { Cell::new(0) };
}

fn completion_definitions(
    snapshot: &AnalysisSnapshot,
    prefix: &str,
    cancellation: &CancellationToken,
) -> Result<Vec<(String, String)>, Cancelled> {
    let mut definitions = Vec::new();
    for definition in snapshot.index().definitions_iter() {
        cancellation.checkpoint()?;
        if starts_with_ignore_ascii_case(&definition.name, prefix) {
            definitions.push((definition.kind.clone(), definition.name.clone()));
        }
    }
    for document in snapshot.documents().values() {
        cancellation.checkpoint()?;
        if document.source() != DocumentSource::Overlay {
            continue;
        }
        if let Some(input) = input_for_document(snapshot, document.id()) {
            definitions.extend(
                semantic_data(snapshot, &input)
                    .definitions
                    .into_iter()
                    .filter(|definition| starts_with_ignore_ascii_case(&definition.name, prefix))
                    .map(|definition| (definition.kind, definition.name)),
            );
        }
    }
    definitions.sort_by(|left, right| {
        (left.0.to_ascii_lowercase(), left.1.to_ascii_lowercase())
            .cmp(&(right.0.to_ascii_lowercase(), right.1.to_ascii_lowercase()))
    });
    definitions.dedup_by(|left, right| {
        left.0.eq_ignore_ascii_case(&right.0) && left.1.eq_ignore_ascii_case(&right.1)
    });
    Ok(definitions)
}

fn completion_definitions_for_kinds(
    snapshot: &AnalysisSnapshot,
    prefix: &str,
    kinds: &[&str],
    cancellation: &CancellationToken,
) -> Result<Vec<(String, String)>, Cancelled> {
    let mut definitions = Vec::new();
    for kind in kinds {
        for definition in snapshot.index().definitions_for_kind(kind) {
            cancellation.checkpoint()?;
            if starts_with_ignore_ascii_case(&definition.name, prefix) {
                definitions.push((definition.kind.clone(), definition.name.clone()));
            }
        }
    }
    for document in snapshot.documents().values() {
        cancellation.checkpoint()?;
        if document.source() != DocumentSource::Overlay {
            continue;
        }
        if let Some(input) = input_for_document(snapshot, document.id()) {
            definitions.extend(
                semantic_data(snapshot, &input)
                    .definitions
                    .into_iter()
                    .filter(|definition| {
                        kinds
                            .iter()
                            .any(|kind| definition.kind.eq_ignore_ascii_case(kind))
                            && starts_with_ignore_ascii_case(&definition.name, prefix)
                    })
                    .map(|definition| (definition.kind, definition.name)),
            );
        }
    }
    definitions.sort_by(|left, right| {
        (left.0.to_ascii_lowercase(), left.1.to_ascii_lowercase())
            .cmp(&(right.0.to_ascii_lowercase(), right.1.to_ascii_lowercase()))
    });
    definitions.dedup_by(|left, right| {
        left.0.eq_ignore_ascii_case(&right.0) && left.1.eq_ignore_ascii_case(&right.1)
    });
    Ok(definitions)
}

fn resolve_symbol(
    snapshot: &AnalysisSnapshot,
    all: &SemanticWorkspace,
    kind: &str,
    name: &str,
) -> Resolution {
    let mut candidates = symbol_candidates(snapshot, all, kind, name);
    if candidates.is_empty() {
        return Resolution::Missing;
    }
    let policy = symbol_resolution_policy(snapshot, kind);
    if matches!(
        policy,
        SymbolResolutionPolicy::Merge | SymbolResolutionPolicy::Unique
    ) {
        return if candidates.len() == 1 {
            Resolution::Unique(candidates.remove(0))
        } else {
            Resolution::Ambiguous
        };
    }
    let highest = candidates
        .iter()
        .map(|candidate| candidate.priority)
        .max()
        .unwrap_or(0);
    candidates.retain(|candidate| candidate.priority == highest);
    if candidates.len() == 1 {
        Resolution::Unique(candidates.remove(0))
    } else {
        Resolution::Ambiguous
    }
}

fn symbol_candidates(
    snapshot: &AnalysisSnapshot,
    all: &SemanticWorkspace,
    kind: &str,
    name: &str,
) -> Vec<ResolutionDefinition> {
    let overlay_files = snapshot
        .documents()
        .values()
        .filter(|document| document.source() == DocumentSource::Overlay)
        .filter_map(|document| document.path())
        .filter_map(|path| {
            snapshot
                .source_files()
                .values()
                .find(|file| file.physical_path == path)
                .map(|file| file.id)
        })
        .collect::<BTreeSet<_>>();
    let mut candidates = all
        .definitions
        .iter()
        .filter(|definition| definition.kind == kind && same_name(&definition.name, name))
        .filter(|definition| {
            definition
                .file
                .is_none_or(|file| !overlay_files.contains(&file) || definition.document.is_some())
        })
        .map(|definition| ResolutionDefinition {
            location: definition.symbol.location.clone(),
            selection_range: definition.symbol.selection_range,
            priority: definition_priority(snapshot, definition),
        })
        .collect::<Vec<_>>();
    // If a manually injected workspace shard has a definition with no source text, retain it as
    // a navigation candidate.  Normal scanned files already appear above with exact ranges.
    if candidates.is_empty() {
        for definition in snapshot.index().definitions(kind, name) {
            candidates.push(index_definition(snapshot, definition));
        }
    }
    candidates.sort_by(|left, right| {
        right.priority.cmp(&left.priority).then_with(|| {
            symbol_location_sort_key(&left.location).cmp(&symbol_location_sort_key(&right.location))
        })
    });
    candidates.dedup_by(|left, right| {
        left.location == right.location && left.selection_range == right.selection_range
    });
    candidates
}

fn symbol_candidates_for_hover(
    snapshot: &AnalysisSnapshot,
    kind: &str,
    name: &str,
    cancellation: &CancellationToken,
) -> Result<Vec<ResolutionDefinition>, Cancelled> {
    let overlay_files = overlay_file_ids(snapshot);
    let mut candidates = Vec::new();
    for document in snapshot
        .documents()
        .values()
        .filter(|document| document.source() == DocumentSource::Overlay)
    {
        cancellation.checkpoint()?;
        let Some(input) = input_for_document(snapshot, document.id()) else {
            continue;
        };
        for definition in semantic_data(snapshot, &input).definitions {
            cancellation.checkpoint()?;
            if definition.kind != kind || !same_name(&definition.name, name) {
                continue;
            }
            let priority = definition_priority(snapshot, &definition);
            candidates.push(ResolutionDefinition {
                location: definition.symbol.location,
                selection_range: definition.symbol.selection_range,
                priority,
            });
        }
    }
    for definition in snapshot.index().definitions(kind, name) {
        cancellation.checkpoint()?;
        if overlay_files.contains(&definition.file_id) {
            continue;
        }
        candidates.push(index_definition(snapshot, definition));
    }
    candidates.sort_by(|left, right| {
        right.priority.cmp(&left.priority).then_with(|| {
            symbol_location_sort_key(&left.location).cmp(&symbol_location_sort_key(&right.location))
        })
    });
    candidates.dedup_by(|left, right| {
        left.location == right.location && left.selection_range == right.selection_range
    });
    Ok(candidates)
}

fn symbol_resolution_policy(snapshot: &AnalysisSnapshot, kind: &str) -> SymbolResolutionPolicy {
    snapshot
        .rules()
        .model()
        .symbol_descriptors
        .iter()
        .find(|descriptor| descriptor.kind_id.eq_ignore_ascii_case(kind))
        .map_or(SymbolResolutionPolicy::ReplaceBySymbol, |descriptor| {
            descriptor.resolution
        })
}

fn symbol_location_sort_key(location: &Location) -> (String, u32, u32) {
    (
        location
            .path
            .as_ref()
            .map_or_else(String::new, |path| path.as_str().to_owned()),
        location.range.start(),
        location.range.end(),
    )
}

struct DirectResolutionContext<'snapshot> {
    snapshot: &'snapshot AnalysisSnapshot,
    overlay_files: BTreeSet<SourceFileId>,
    overlay_definitions: BTreeMap<(String, String), Vec<ResolutionDefinition>>,
}

impl<'snapshot> DirectResolutionContext<'snapshot> {
    fn new(snapshot: &'snapshot AnalysisSnapshot) -> Self {
        let mut context = Self {
            snapshot,
            overlay_files: BTreeSet::new(),
            overlay_definitions: BTreeMap::new(),
        };
        for document in snapshot
            .documents()
            .values()
            .filter(|document| document.source() == DocumentSource::Overlay)
        {
            if let Some(path) = document.path()
                && let Some(file) = snapshot
                    .source_files()
                    .values()
                    .find(|file| file.physical_path == path)
            {
                context.overlay_files.insert(file.id);
            }
            let Some(input) = input_for_document(snapshot, document.id()) else {
                continue;
            };
            for definition in semantic_data(snapshot, &input).definitions {
                let priority = definition_priority(snapshot, &definition);
                context
                    .overlay_definitions
                    .entry((
                        definition.kind.to_ascii_lowercase(),
                        definition.name.to_ascii_lowercase(),
                    ))
                    .or_default()
                    .push(ResolutionDefinition {
                        location: definition.symbol.location,
                        selection_range: definition.symbol.selection_range,
                        priority,
                    });
            }
        }
        context
    }

    fn resolve(&self, kind: &str, name: &str) -> Resolution {
        let mut candidates = self
            .overlay_definitions
            .get(&(kind.to_ascii_lowercase(), name.to_ascii_lowercase()))
            .cloned()
            .unwrap_or_default();
        candidates.extend(
            self.snapshot
                .index()
                .definitions(kind, name)
                .into_iter()
                .filter(|definition| !self.overlay_files.contains(&definition.file_id))
                .map(|definition| index_definition(self.snapshot, definition)),
        );
        if candidates.is_empty() {
            return Resolution::Missing;
        }
        let policy = self
            .snapshot
            .rules()
            .model()
            .symbol_descriptors
            .iter()
            .find(|descriptor| descriptor.kind_id.eq_ignore_ascii_case(kind))
            .map_or(SymbolResolutionPolicy::ReplaceBySymbol, |descriptor| {
                descriptor.resolution
            });
        if matches!(
            policy,
            SymbolResolutionPolicy::Merge | SymbolResolutionPolicy::Unique
        ) {
            return if candidates.len() == 1 {
                Resolution::Unique(candidates.remove(0))
            } else {
                Resolution::Ambiguous
            };
        }
        let highest = candidates
            .iter()
            .map(|candidate| candidate.priority)
            .max()
            .unwrap_or(0);
        candidates.retain(|candidate| candidate.priority == highest);
        if candidates.len() == 1 {
            Resolution::Unique(candidates.remove(0))
        } else {
            Resolution::Ambiguous
        }
    }
}

fn definition_priority(snapshot: &AnalysisSnapshot, definition: &DefinitionInfo) -> u64 {
    if definition.document.is_some() {
        return 20_000;
    }
    let Some(file) = definition
        .file
        .and_then(|id| snapshot.source_files().get(&id))
    else {
        return 0;
    };
    let Some(root) = snapshot
        .source_roots()
        .iter()
        .find(|root| root.id == file.root_id)
    else {
        return 0;
    };
    match root.kind {
        pdx_engine::SourceRootKind::Vanilla => 0,
        pdx_engine::SourceRootKind::Dependency => 1_000 + u64::from(root.order),
        pdx_engine::SourceRootKind::CurrentMod => 10_000 + u64::from(root.order),
    }
}

fn index_definition(snapshot: &AnalysisSnapshot, definition: &Definition) -> ResolutionDefinition {
    let (path, document) = snapshot
        .source_files()
        .get(&definition.file_id)
        .map(|file| (Some(file.logical_path.clone()), None))
        .unwrap_or((None, None));
    ResolutionDefinition {
        location: Location {
            document,
            file: Some(definition.file_id),
            path,
            range: definition.range,
        },
        selection_range: indexed_definition_selection_range(snapshot, definition),
        priority: definition_priority_for_file(snapshot, definition.file_id),
    }
}

fn index_definition_info(snapshot: &AnalysisSnapshot, definition: &Definition) -> DefinitionInfo {
    let selection_range = indexed_definition_selection_range(snapshot, definition);
    let path = snapshot
        .source_files()
        .get(&definition.file_id)
        .map(|file| file.logical_path.clone());
    let location = Location {
        document: None,
        file: Some(definition.file_id),
        path,
        range: definition.range,
    };
    DefinitionInfo {
        kind: definition.kind.clone(),
        name: definition.name.clone(),
        symbol: Symbol {
            name: definition.name.clone(),
            kind: definition.kind.clone(),
            range: definition.range,
            selection_range,
            location,
        },
        document: None,
        file: Some(definition.file_id),
    }
}

fn indexed_definition_selection_range(
    snapshot: &AnalysisSnapshot,
    definition: &Definition,
) -> TextRange {
    snapshot
        .file_state(definition.file_id)
        .and_then(|state| state.hir())
        .and_then(|hir| {
            hir.definitions()
                .iter()
                .find(|candidate| {
                    candidate.kind.eq_ignore_ascii_case(&definition.kind)
                        && candidate.name.eq_ignore_ascii_case(&definition.name)
                        && candidate.range == definition.range
                })
                .map(|candidate| candidate.selection_range)
        })
        .unwrap_or(definition.range)
}

fn definition_selection_location(definition: &ResolutionDefinition) -> Location {
    let mut location = definition.location.clone();
    location.range = definition.selection_range;
    location
}

fn definition_priority_for_file(snapshot: &AnalysisSnapshot, id: SourceFileId) -> u64 {
    let Some(file) = snapshot.source_files().get(&id) else {
        return 0;
    };
    let Some(root) = snapshot
        .source_roots()
        .iter()
        .find(|root| root.id == file.root_id)
    else {
        return 0;
    };
    match root.kind {
        pdx_engine::SourceRootKind::Vanilla => 0,
        pdx_engine::SourceRootKind::Dependency => 1_000 + u64::from(root.order),
        pdx_engine::SourceRootKind::CurrentMod => 10_000 + u64::from(root.order),
    }
}

fn symbol_at(
    all: &SemanticWorkspace,
    document: &DocumentId,
    position: TextSize,
) -> Option<(String, String)> {
    if let Some(reference) = all.references.iter().find(|reference| {
        reference.document.as_ref() == Some(document) && contains(reference.range, position)
    }) {
        return Some((reference.kind.clone(), reference.name.clone()));
    }
    all.definitions
        .iter()
        .find(|definition| {
            definition.document.as_ref() == Some(document)
                && contains(definition.symbol.selection_range, position)
        })
        .map(|definition| (definition.kind.clone(), definition.name.clone()))
}

fn local_parameter_target(
    input: &ParsedInput,
    position: TextSize,
) -> Option<(
    &pdx_engine::hir::HirParameterDefinition,
    &pdx_engine::hir::HirParameterReference,
)> {
    let hir = input.hir.as_deref()?;
    let reference = hir.parameter_reference_at(position)?;
    let definition = hir
        .parameter_definitions_for_owner(reference.owner_range)
        .find(|definition| definition.name.eq_ignore_ascii_case(&reference.name))?;
    Some((definition, reference))
}

fn local_location(input: &ParsedInput, range: TextRange) -> Location {
    Location {
        document: input.document.clone(),
        file: input.file,
        path: input.path.clone(),
        range,
    }
}

fn hover_for_symbol(
    snapshot: &AnalysisSnapshot,
    kind: &str,
    name: &str,
    range: TextRange,
    cancellation: &CancellationToken,
) -> Result<Hover, Cancelled> {
    let candidates = symbol_candidates_for_hover(snapshot, kind, name, cancellation)?;
    let policy = symbol_resolution_policy(snapshot, kind);
    let mut sections = vec![format!("### {} `{}`", kind, name)];
    if candidates.is_empty() {
        sections.push(format!("#### unresolved {kind} symbol"));
    } else {
        let highest = candidates
            .iter()
            .map(|candidate| candidate.priority)
            .max()
            .unwrap_or(0);
        let active = match policy {
            SymbolResolutionPolicy::ReplaceBySymbol => candidates
                .iter()
                .filter(|candidate| candidate.priority == highest)
                .collect::<Vec<_>>(),
            SymbolResolutionPolicy::Merge | SymbolResolutionPolicy::Unique => {
                if candidates.len() == 1 {
                    vec![&candidates[0]]
                } else {
                    Vec::new()
                }
            }
        };
        if active.len() == 1 {
            let definition = active[0];
            sections.push(format!(
                "#### Resolved definition\n\n- Source root: {}\n- Defined in: `{}`",
                symbol_source_root(snapshot, &definition.location),
                symbol_location_path(&definition.location),
            ));
            let shadowed = candidates
                .iter()
                .filter(|candidate| {
                    !same_location(&candidate.location, &definition.location)
                        && candidate.priority < definition.priority
                })
                .collect::<Vec<_>>();
            if !shadowed.is_empty() {
                sections.push(format!(
                    "#### Shadowed definitions:\n\n{}",
                    shadowed
                        .into_iter()
                        .map(|candidate| format!(
                            "- {}: `{}`",
                            symbol_source_root(snapshot, &candidate.location),
                            symbol_location_path(&candidate.location),
                        ))
                        .collect::<Vec<_>>()
                        .join("\n")
                ));
            }
            if kind.eq_ignore_ascii_case("localisation")
                && let Some((language, value)) = localisation_preview(snapshot, definition)
            {
                sections.push(format!(
                    "#### Localisation preview\n\n- Localisation{}: \"{}\"",
                    language
                        .as_deref()
                        .map_or_else(String::new, |language| format!(" ({language})")),
                    value
                ));
            }
        } else {
            sections.push(format!("#### ambiguous {kind} symbol"));
            sections.push(format!(
                "#### Candidates:\n\n{}",
                candidates
                    .iter()
                    .map(|candidate| format!(
                        "- {}: `{}`",
                        symbol_source_root(snapshot, &candidate.location),
                        symbol_location_path(&candidate.location),
                    ))
                    .collect::<Vec<_>>()
                    .join("\n")
            ));
        }
    }
    Ok(Hover {
        contents: sections.join("\n\n"),
        range: Some(range),
    })
}

fn localisation_preview(
    snapshot: &AnalysisSnapshot,
    definition: &ResolutionDefinition,
) -> Option<(Option<String>, String)> {
    if let Some(file) = definition.location.file
        && let Some(preview) =
            snapshot.vanilla_localisation_preview(file, definition.location.range)
    {
        return Some((preview.language.clone(), preview.value.clone()));
    }
    let input = definition
        .location
        .document
        .as_ref()
        .and_then(|document| input_for_document(snapshot, document))
        .or_else(|| {
            definition
                .location
                .file
                .and_then(|file| input_for_source_file(snapshot, file))
        })?;
    let ParsedContent::Text(parsed) = &input.parsed;
    let entry = find_cst_node(
        parsed.root(),
        CstKind::LocalisationEntry,
        definition.location.range,
    )?;
    let value_node = entry.children().iter().find(|child| {
        matches!(
            child.kind(),
            CstKind::LocalisationString | CstKind::UnquotedValue
        )
    })?;
    let raw = parsed.text(value_node.range())?.trim();
    let value = raw
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .unwrap_or(raw);
    let value = truncate_hover_text(value);
    if value.is_empty() {
        return None;
    }
    let mut language = None;
    for node in parsed.root().children() {
        if node.range().start() > entry.range().start() {
            break;
        }
        if node.kind() == CstKind::LanguageHeader
            && let Some(value) = node
                .children()
                .iter()
                .find(|child| child.kind() == CstKind::LocalisationKey)
                .and_then(|child| parsed.text(child.range()))
        {
            language = Some(value.trim().to_owned());
        }
    }
    Some((language, value))
}

fn find_cst_node(node: &CstNode, kind: CstKind, range: TextRange) -> Option<&CstNode> {
    if node.kind() == kind && node.range() == range {
        return Some(node);
    }
    node.children()
        .iter()
        .find_map(|child| find_cst_node(child, kind, range))
}

fn truncate_hover_text(value: &str) -> String {
    const MAX_CHARS: usize = 240;
    let mut truncated = value.chars().take(MAX_CHARS).collect::<String>();
    if value.chars().count() > MAX_CHARS {
        truncated.push('…');
    }
    truncated
}

fn symbol_location_path(location: &Location) -> String {
    location.path.as_ref().map_or_else(
        || "<open document>".to_owned(),
        |path| path.as_str().to_owned(),
    )
}

fn symbol_source_root(snapshot: &AnalysisSnapshot, location: &Location) -> String {
    let root = location
        .file
        .and_then(|file_id| snapshot.source_files().get(&file_id))
        .and_then(|file| {
            snapshot
                .source_roots()
                .iter()
                .find(|root| root.id == file.root_id)
        })
        .or_else(|| {
            location
                .document
                .as_ref()
                .and_then(|document_id| snapshot.document(document_id))
                .and_then(|document| document.path())
                .and_then(|path| root_for_path(snapshot, path))
        });
    match root.map(|root| root.kind) {
        Some(pdx_engine::SourceRootKind::Vanilla) => "Vanilla".to_owned(),
        Some(pdx_engine::SourceRootKind::Dependency) => "Dependency".to_owned(),
        Some(pdx_engine::SourceRootKind::CurrentMod) => "Current Mod".to_owned(),
        None if location.document.is_some() => "Open overlay".to_owned(),
        None => "Unknown source root".to_owned(),
    }
}

fn known_keys(snapshot: &AnalysisSnapshot) -> BTreeSet<String> {
    let mut keys = snapshot
        .game_profile()
        .fallback_keys
        .iter()
        .map(|key| key.to_ascii_lowercase())
        .collect::<BTreeSet<_>>();
    for record in &snapshot.rules().model().records {
        keys.extend(record.fields.keys().map(|key| key.to_ascii_lowercase()));
    }
    // The imported descriptor catalog is the authoritative extension point for semantic keys.
    // Keep profile fallbacks useful in degraded mode, then admit every descriptor name supplied
    // by a validated rules artifact.
    keys.extend(
        snapshot
            .rules()
            .model()
            .symbol_descriptors
            .iter()
            .map(|descriptor| descriptor.kind_id.to_ascii_lowercase()),
    );
    keys
}

fn completion_value_context(input: &ParsedInput, position: TextSize) -> bool {
    if input.format == FileFormat::Script
        && let Some(hir) = input.hir.as_deref()
    {
        if hir.properties().iter().any(|property| {
            position >= property.key_range.start() && position <= property.key_range.end()
        }) {
            return false;
        }
        if hir.properties().iter().any(|property| {
            property.scalar.as_ref().is_some_and(|scalar| {
                position >= scalar.range.start() && position <= scalar.range.end()
            })
        }) {
            return true;
        }
    }
    let offset = usize::try_from(position)
        .unwrap_or(input.source.len())
        .min(input.source.len());
    let line_start = input.source[..offset]
        .rfind('\n')
        .map_or(0, |index| index + 1);
    let line = &input.source[line_start..offset];
    if input.format == FileFormat::Localisation {
        return line.contains(':') && !line.trim_start().starts_with('#');
    }
    let equals = line.rfind('=');
    let open = line.rfind('{');
    equals.is_some_and(|equals| open.is_none_or(|open| equals > open))
}

fn add_scalar_items(items: &mut Vec<CompletionItem>, range: TextRange, prefix: &str) {
    for (label, score) in [
        ("yes", 0),
        ("no", 0),
        ("true", 5),
        ("false", 5),
        ("ROOT", 10),
        ("FROM", 10),
        ("PREV", 10),
    ] {
        push_completion(
            items,
            CompletionItem {
                label: label.to_owned(),
                kind: CompletionKind::Value,
                detail: "PDX scalar".to_owned(),
                documentation: None,
                replacement_range: range,
                insert_text: label.to_owned(),
                sort_score: score,
                deprecated: false,
            },
            prefix,
        );
    }
}

fn push_completion(items: &mut Vec<CompletionItem>, item: CompletionItem, prefix: &str) {
    if starts_with_ignore_ascii_case(&item.label, prefix) {
        items.push(item);
    }
}

fn starts_with_ignore_ascii_case(value: &str, prefix: &str) -> bool {
    value.len() >= prefix.len()
        && value.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
}

fn word_range(source: &str, position: TextSize) -> TextRange {
    let mut offset = usize::try_from(position)
        .unwrap_or(source.len())
        .min(source.len());
    while offset > 0 && !source.is_char_boundary(offset) {
        offset -= 1;
    }
    let mut start = offset;
    while start > 0 && is_word_byte(source.as_bytes()[start - 1]) {
        start -= 1;
    }
    let mut end = offset;
    while end < source.len() && is_word_byte(source.as_bytes()[end]) {
        end += 1;
    }
    TextRange::new(
        u32::try_from(start).unwrap_or(u32::MAX),
        u32::try_from(end).unwrap_or(u32::MAX),
    )
    .unwrap_or_else(|| TextRange::empty(u32::try_from(start).unwrap_or(u32::MAX)))
}

fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b'@' | b'$')
}

fn contains(range: TextRange, position: TextSize) -> bool {
    if range.is_empty() {
        position == range.start()
    } else {
        position >= range.start() && position < range.end()
    }
}

fn same_name(left: &str, right: &str) -> bool {
    left.eq_ignore_ascii_case(right)
}

fn same_location(left: &Location, right: &Location) -> bool {
    left.document == right.document
        && left.file == right.file
        && left.path == right.path
        && left.range == right.range
}

fn rename_target(
    snapshot: &AnalysisSnapshot,
    document: &DocumentId,
    position: TextSize,
    cancellation: &CancellationToken,
) -> Result<RenameTarget, RenameFailure> {
    cancellation
        .checkpoint()
        .map_err(|Cancelled| RenameFailure::Cancelled)?;
    let input = input_for_document(snapshot, document).ok_or(RenameError::NoSymbol)?;
    let all =
        all_semantics(snapshot, cancellation).map_err(|Cancelled| RenameFailure::Cancelled)?;
    let Some((kind, name)) = symbol_at(&all, document, position) else {
        return Err(RenameError::NoSymbol.into());
    };
    let definition = match resolve_symbol(snapshot, &all, &kind, &name) {
        Resolution::Unique(definition) => definition,
        Resolution::Ambiguous => return Err(RenameError::Ambiguous.into()),
        Resolution::Missing => return Err(RenameError::Unresolved.into()),
    };
    if !writable_location(snapshot, &definition.location) {
        return Err(RenameError::ReadOnly.into());
    }
    Ok(RenameTarget {
        kind,
        name,
        cursor_range: word_range(&input.source, position),
        definition,
    })
}

fn check_rename_conflict(
    snapshot: &AnalysisSnapshot,
    all: &SemanticWorkspace,
    target: &RenameTarget,
    new_name: &str,
    cancellation: &CancellationToken,
) -> Result<(), RenameFailure> {
    let policy = snapshot
        .rules()
        .model()
        .symbol_descriptors
        .iter()
        .find(|descriptor| descriptor.kind_id.eq_ignore_ascii_case(&target.kind))
        .map_or(SymbolResolutionPolicy::ReplaceBySymbol, |descriptor| {
            descriptor.resolution
        });
    for definition in &all.definitions {
        cancellation
            .checkpoint()
            .map_err(|Cancelled| RenameFailure::Cancelled)?;
        if definition.kind != target.kind || !same_name(&definition.name, new_name) {
            continue;
        }
        if same_location(&definition.symbol.location, &target.definition.location) {
            continue;
        }
        let priority = definition_priority(snapshot, definition);
        let conflict = match policy {
            SymbolResolutionPolicy::Merge | SymbolResolutionPolicy::Unique => true,
            SymbolResolutionPolicy::ReplaceBySymbol => priority >= target.definition.priority,
        };
        if conflict {
            return Err(RenameError::Conflict.into());
        }
    }
    Ok(())
}

fn valid_rename_name(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(is_word_byte)
}

fn valid_parameter_name(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|byte| byte != b'$' && is_word_byte(byte))
}

fn writable_location(snapshot: &AnalysisSnapshot, location: &Location) -> bool {
    if let Some(file) = location.file
        && let Some(source_file) = snapshot.source_files().get(&file)
    {
        return snapshot
            .source_roots()
            .iter()
            .find(|root| root.id == source_file.root_id)
            .is_some_and(|root| matches!(root.kind, pdx_engine::SourceRootKind::CurrentMod));
    }
    if let Some(document_id) = location.document.as_ref()
        && let Some(document) = snapshot.document(document_id)
    {
        if document.source() != DocumentSource::Overlay {
            return false;
        }
        return document.path().is_none_or(|path| {
            root_for_path(snapshot, path)
                .is_some_and(|root| matches!(root.kind, pdx_engine::SourceRootKind::CurrentMod))
        });
    }
    false
}

fn root_for_path<'a>(
    snapshot: &'a AnalysisSnapshot,
    path: &Path,
) -> Option<&'a pdx_engine::SourceRoot> {
    snapshot
        .source_roots()
        .iter()
        .filter(|root| path.strip_prefix(&root.path).is_ok())
        .max_by_key(|root| root.path.as_os_str().len())
}

fn overlay_file_ids(snapshot: &AnalysisSnapshot) -> BTreeSet<SourceFileId> {
    snapshot
        .documents()
        .values()
        .filter(|document| document.source() == DocumentSource::Overlay)
        .filter_map(|document| document.path())
        .flat_map(|path| {
            snapshot
                .source_files()
                .values()
                .filter(move |file| file.physical_path == path)
                .map(|file| file.id)
        })
        .collect()
}

fn edit_target_key(location: &Location) -> (u8, String) {
    if let Some(document) = location.document.as_ref() {
        return (0, document.as_str().to_owned());
    }
    if let Some(file) = location.file {
        return (1, file.get().to_string());
    }
    (
        2,
        location
            .path
            .as_ref()
            .map_or_else(String::new, |path| path.as_str().to_owned()),
    )
}

fn fuzzy_match(value: &str, query: &str) -> bool {
    let mut chars = value.chars();
    query
        .chars()
        .all(|wanted| chars.by_ref().any(|actual| actual == wanted))
}

#[cfg(test)]
mod tests {
    use super::{
        CancellationToken, Cancelled, CompletionKind, DiagnosticCode, RenameError, RenameFailure,
        complete, complete_with_cancellation, definition, diagnostics,
        diagnostics_with_cancellation, document_symbols, hover, input_for_document, prepare_rename,
        references, rename, rename_with_cancellation, semantic_completion_context,
        semantic_root_context, workspace_symbols, workspace_symbols_with_cancellation,
    };
    use pdx_engine::{
        AnalysisHost, DocumentId, SourceRoot, SourceRootId, SourceRootKind, VanillaIndexCache,
        WorkspaceChange,
    };
    use pdx_rules::{
        KeyMatcher, ProfileDefinitionRule, ProfileMatchMode, ProfileTextMatcher, RuleSet,
        RuleShape, SemanticRule, ValueMatcher,
    };
    use pdx_text::{LogicalPath, TextRange};

    fn eu4_host(rules: RuleSet) -> AnalysisHost {
        AnalysisHost::with_profile(rules, pdx_game::eu4::profile())
    }

    fn snapshot(text: &str) -> (AnalysisHost, DocumentId) {
        let mut host = eu4_host(pdx_game::eu4::bootstrap_rules());
        let id = DocumentId::new("file:///tmp/common/events/test.txt");
        host.open_document(id.clone(), 1, text.to_owned(), None)
            .expect("open");
        (host, id)
    }

    fn semantic_snapshot(text: &str) -> (AnalysisHost, DocumentId) {
        semantic_snapshot_with_constraints(text, None, None, Some(1))
    }

    fn semantic_snapshot_with_severity(
        text: &str,
        severity: Option<u8>,
    ) -> (AnalysisHost, DocumentId) {
        semantic_snapshot_with_constraints(text, severity, None, Some(1))
    }

    fn semantic_snapshot_with_constraints(
        text: &str,
        severity: Option<u8>,
        min_occurs: Option<u32>,
        max_occurs: Option<u32>,
    ) -> (AnalysisHost, DocumentId) {
        let mut model = pdx_game::eu4::bootstrap_model();
        model.semantic.rules.push(SemanticRule {
            id: "fixture:trigger:foo".to_owned(),
            context: "trigger".to_owned(),
            parent_path: Vec::new(),
            key: KeyMatcher::Exact("foo".to_owned()),
            operator: None,
            value: ValueMatcher::Bool,
            shape: RuleShape::Leaf,
            child_context: None,
            alternative_id: None,
            severity,
            required: min_occurs.is_none() && max_occurs.is_none(),
            documentation: Vec::new(),
            allowed_scopes: Vec::new(),
            push_scope: None,
            replace_scope: Vec::new(),
            min_occurs,
            strict_min: true,
            max_occurs,
            source_file: "fixture.semantic".to_owned(),
            line: 1,
        });
        let mut host = eu4_host(RuleSet::from_model(model));
        let id = DocumentId::new("file:///tmp/common/events/test.txt");
        host.open_document(id.clone(), 1, text.to_owned(), None)
            .expect("open");
        (host, id)
    }

    #[test]
    fn required_rule_without_explicit_minimum_reports_missing_property() {
        let (host, id) = semantic_snapshot_with_constraints("trigger = { }\n", None, None, None);
        let results = diagnostics(&host.snapshot(), &id);
        assert!(
            results
                .iter()
                .any(|item| item.code == DiagnosticCode::Cardinality),
            "required must imply one minimum occurrence"
        );
    }

    #[test]
    fn cancellable_queries_stop_at_internal_checkpoints() {
        let (host, id) = snapshot(
            "country_event = { id = cancel.1 immediate = { country_event = { id = cancel.1 } } }\n",
        );
        let snapshot = host.snapshot();

        let completion_cancellation = CancellationToken::cancel_after(1);
        assert_eq!(
            complete_with_cancellation(&snapshot, &id, 25, &completion_cancellation),
            Err(Cancelled)
        );
        assert!(completion_cancellation.is_cancelled());

        let diagnostics_cancellation = CancellationToken::cancel_after(3);
        assert_eq!(
            diagnostics_with_cancellation(&snapshot, &id, &diagnostics_cancellation),
            Err(Cancelled)
        );

        let workspace_cancellation = CancellationToken::new();
        workspace_cancellation.cancel();
        assert_eq!(
            workspace_symbols_with_cancellation(&snapshot, "cancel", &workspace_cancellation),
            Err(Cancelled)
        );
        assert_eq!(
            rename_with_cancellation(&snapshot, &id, 25, "renamed.1", &workspace_cancellation,),
            Err(RenameFailure::Cancelled)
        );
    }

    #[test]
    fn incomplete_input_has_syntax_diagnostics_and_completion() {
        let (host, id) = snapshot("country_event = { id = test.1\n  un");
        let snapshot = host.snapshot();
        assert!(
            diagnostics(&snapshot, &id)
                .iter()
                .any(|item| item.code == DiagnosticCode::Syntax)
        );
        let result = complete(&snapshot, &id, 35);
        assert!(!result.items.is_empty());
    }

    #[test]
    fn semantic_completion_does_not_materialize_the_full_workspace() {
        let text = concat!(
            "country_event = { mean_time_to_happen = { ",
            "modifier = { factor = 0.5 always = maybe }",
            " } }\n",
        );
        let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
        let id = DocumentId::new("file:///tmp/events/completion-fast-path.txt");
        host.open_document(id.clone(), 1, text.to_owned(), None)
            .expect("open");
        let snapshot = host.snapshot();
        let position =
            u32::try_from(text.find("always").expect("completion key")).expect("position");

        super::ALL_SEMANTICS_CALLS.with(|calls| calls.set(0));
        let completion = complete(&snapshot, &id, position);

        assert!(completion.items.iter().any(|item| item.label == "always"));
        super::ALL_SEMANTICS_CALLS.with(|calls| {
            assert_eq!(
                calls.get(),
                0,
                "contextual completion must not clone all workspace definitions and references"
            );
        });
    }

    #[test]
    fn symbol_hover_does_not_materialize_the_full_workspace() {
        let (host, id) = snapshot("country_event = { id = hover.1 }\nevent = hover.1\n");
        let snapshot = host.snapshot();
        let position = u32::try_from("country_event = { id = hover.1 }\nevent = ".len() + 1)
            .expect("reference offset");

        super::ALL_SEMANTICS_CALLS.with(|calls| calls.set(0));
        let hover = hover(&snapshot, &id, position).expect("symbol hover");
        assert!(hover.contents.contains("Resolved definition"));
        super::ALL_SEMANTICS_CALLS.with(|calls| {
            assert_eq!(
                calls.get(),
                0,
                "symbol hover must query the current file and symbol bucket directly"
            );
        });
    }

    #[test]
    fn semantic_diagnostics_do_not_materialize_the_full_workspace() {
        let (host, id) = snapshot(
            "country_event = { id = direct.1 title = missing_title immediate = { always = yes } }\n",
        );
        let snapshot = host.snapshot();

        super::ALL_SEMANTICS_CALLS.with(|calls| calls.set(0));
        let results = diagnostics(&snapshot, &id);

        assert!(
            results
                .iter()
                .any(|item| item.code == DiagnosticCode::UnknownSymbol)
        );
        super::ALL_SEMANTICS_CALLS.with(|calls| {
            assert_eq!(
                calls.get(),
                0,
                "document diagnostics must query symbol buckets instead of cloning the workspace"
            );
        });
    }

    #[test]
    fn completion_traversal_uses_hir_to_disambiguate_nested_rule_contexts() {
        let text = concat!(
            "country_event = { mean_time_to_happen = { ",
            "modifier = { factor = 0.5 always = maybe }",
            " } }\n",
        );
        let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
        let id = DocumentId::new("file:///tmp/events/completion_scope.txt");
        host.open_document(id.clone(), 1, text.to_owned(), None)
            .expect("open");
        let snapshot = host.snapshot();
        let input = input_for_document(&snapshot, &id).expect("analysis input");
        let position =
            u32::try_from(text.find("always").expect("trigger child")).expect("position");
        let context = semantic_completion_context(&snapshot, &input, position)
            .expect("semantic completion context");
        assert_eq!(context.context, "trigger");
        assert!(context.parent_path.is_empty());
        assert_eq!(
            context.structural_containers,
            [("modifier_rule".to_owned(), vec!["modifier".to_owned()])]
        );
        assert_eq!(context.scope.current, "country");
        let mut all_key_items = Vec::new();
        let mut member_cache = super::CompletionMemberCache::default();
        super::add_semantic_key_items(
            &snapshot,
            &context,
            &mut member_cache,
            &mut all_key_items,
            TextRange::empty(position),
            "",
        );
        assert!(all_key_items.iter().any(|item| item.label == "always"));
        assert!(all_key_items.iter().any(|item| item.label == "factor"));
        let completion = complete(&snapshot, &id, position);
        assert!(
            completion.items.iter().any(|item| item.label == "always"),
            "trigger keys must be offered inside the disambiguated modifier block: {:?}",
            completion.items
        );
        let results = diagnostics(&snapshot, &id);
        assert!(
            results
                .iter()
                .all(|item| item.code != DiagnosticCode::UnknownKey),
            "structural modifier fields and nested trigger keys must both be recognized: {results:?}"
        );
        assert!(
            results.iter().any(|item| {
                item.code == DiagnosticCode::InvalidValue && item.message.contains("always")
            }),
            "the nested trigger context must validate `always`: {results:?}"
        );
    }

    #[test]
    fn empty_ambiguous_block_completion_unions_possible_rule_destinations() {
        let text = concat!(
            "country_event = {\n",
            "  mean_time_to_happen = {\n",
            "    \n",
            "  }\n",
            "}\n",
        );
        let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
        let id = DocumentId::new("file:///tmp/events/empty-scope-completion.txt");
        host.open_document(id.clone(), 1, text.to_owned(), None)
            .expect("open");
        let snapshot = host.snapshot();
        let input = input_for_document(&snapshot, &id).expect("analysis input");
        let position = u32::try_from(text.find("    \n").expect("blank completion line"))
            .expect("position")
            .saturating_add(4);
        let context = semantic_completion_context(&snapshot, &input, position)
            .expect("semantic completion context");
        assert!(
            context
                .alternative_containers
                .iter()
                .any(|container| container.context == "modifier_rule"),
            "the conflicting modifier destination must remain available"
        );
        let completion = complete(&snapshot, &id, position);
        assert!(completion.items.iter().any(|item| item.label == "days"));
        assert!(completion.items.iter().any(|item| item.label == "modifier"));
    }

    #[test]
    fn query_input_reuses_the_document_hir_handle() {
        let (host, id) = snapshot("country_event = { id = shared.1 }\n");
        let snapshot = host.snapshot();
        let document_hir = snapshot
            .document(&id)
            .expect("document")
            .hir_handle()
            .expect("HIR");
        let input = input_for_document(&snapshot, &id).expect("analysis input");
        let input_hir = input.hir.as_ref().expect("shared analysis HIR");

        assert!(std::sync::Arc::ptr_eq(&document_hir, input_hir));
    }

    #[test]
    fn identity_only_host_does_not_guess_eu4_semantics_from_game_id() {
        let mut host = AnalysisHost::new(pdx_game::eu4::bootstrap_rules());
        let id = DocumentId::new("file:///tmp/common/events/generic.txt");
        host.open_document(
            id.clone(),
            1,
            "country_event = { id = generic.1 scope = country }\n".to_owned(),
            None,
        )
        .expect("open");

        let snapshot = host.snapshot();
        assert!(document_symbols(&snapshot, &id).is_empty());
        assert!(
            diagnostics(&snapshot, &id)
                .iter()
                .any(|item| item.code == DiagnosticCode::UnknownScope)
        );
    }

    #[test]
    fn eu4_profile_supplies_known_scope_spellings() {
        let (host, id) = snapshot("country_event = { scope = country }\n");

        assert!(
            diagnostics(&host.snapshot(), &id)
                .iter()
                .all(|item| item.code != DiagnosticCode::UnknownScope)
        );
    }

    #[test]
    fn multiple_hir_scope_candidates_remain_conservative_in_analysis() {
        let state = pdx_engine::hir::ScopeState {
            root: pdx_engine::hir::ScopeValue::Known(vec![
                "country".to_owned(),
                "province".to_owned(),
            ]),
            current: vec![pdx_engine::hir::ScopeValue::Known(vec![
                "country".to_owned(),
                "province".to_owned(),
            ])],
            from: vec![pdx_engine::hir::ScopeValue::Known(vec![
                "country".to_owned(),
            ])],
            previous: Vec::new(),
        };
        let context =
            super::scope_context_from_hir(std::sync::Arc::new(pdx_game::eu4::profile()), &state);
        assert_eq!(context.root, "any");
        assert_eq!(context.current, "any");
        assert_eq!(context.from, ["country"]);
    }

    #[test]
    fn unresolved_symbol_is_diagnosed_without_a_definition() {
        let (host, id) = snapshot("event = missing.1\n");
        let diagnostics = diagnostics(&host.snapshot(), &id);
        assert!(
            diagnostics
                .iter()
                .any(|item| item.code == DiagnosticCode::UnknownSymbol)
        );
        assert!(definition(&host.snapshot(), &id, 8).is_empty());
    }

    #[test]
    fn ambiguous_symbol_is_diagnosed_and_never_picks_a_definition() {
        let (host, id) = snapshot(
            "country_event = { id = duplicate.1 }\ncountry_event = { id = duplicate.1 }\nevent = duplicate.1\n",
        );
        let snapshot = host.snapshot();
        assert!(
            diagnostics(&snapshot, &id)
                .iter()
                .any(|item| item.code == DiagnosticCode::AmbiguousSymbol)
        );
        assert!(definition(&snapshot, &id, 80).is_empty());
        let reference = u32::try_from(
            "country_event = { id = duplicate.1 }\ncountry_event = { id = duplicate.1 }\nevent = "
                .len()
                + 1,
        )
        .expect("reference offset");
        let hover = hover(&snapshot, &id, reference).expect("ambiguous hover");
        assert!(hover.contents.contains("ambiguous event symbol"));
        assert!(hover.contents.contains("#### Candidates:\n\n- "));
        assert!(hover.contents.contains("Candidates:"));
    }

    #[test]
    fn symbol_hover_explains_active_and_shadowed_source_roots() {
        use pdx_engine::{SourceRoot, SourceRootId, SourceRootKind, WorkspaceChange};
        use std::fs;

        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("pdx-analysis-hover-sources-{nonce}"));
        let vanilla = root.join("vanilla");
        let current = root.join("current");
        fs::create_dir_all(vanilla.join("common/events")).expect("Vanilla directory");
        fs::create_dir_all(current.join("common/events")).expect("current directory");
        for source_root in [&vanilla, &current] {
            fs::write(
                source_root.join("common/events/definitions.txt"),
                "country_event = { id = shared.1 }\n",
            )
            .expect("event definition");
        }

        let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
        host.apply_change(WorkspaceChange::SetSourceRoots(vec![
            SourceRoot {
                id: SourceRootId::new(1),
                kind: SourceRootKind::Vanilla,
                path: vanilla,
                order: 0,
                writable: false,
            },
            SourceRoot {
                id: SourceRootId::new(2),
                kind: SourceRootKind::CurrentMod,
                path: current,
                order: 0,
                writable: true,
            },
        ]));
        host.refresh_source_roots().expect("scan source roots");
        let id = DocumentId::new("file:///tmp/events/reference.txt");
        let text = "event = shared.1\n";
        host.open_document(id.clone(), 1, text.to_owned(), None)
            .expect("open reference");
        let position =
            u32::try_from(text.find("shared.1").expect("reference") + 1).expect("position");
        let hover = hover(&host.snapshot(), &id, position).expect("source hover");
        assert!(
            hover
                .contents
                .contains("#### Resolved definition\n\n- Source root:")
        );
        assert!(hover.contents.contains("Source root: Current Mod"));
        assert!(hover.contents.contains("#### Shadowed definitions:"));
        assert!(hover.contents.contains("Shadowed definitions:"));
        assert!(hover.contents.contains("Vanilla"));
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn semantic_hover_explains_scope_transition() {
        let (host, id) = {
            let mut host =
                eu4_host(pdx_game::eu4::first_party_rules().expect("load first-party rules"));
            let id = DocumentId::new("file:///tmp/events/scope-hover.txt");
            host.open_document(
                id.clone(),
                1,
                "country_event = { immediate = { capital_scope = { } } }\n".to_owned(),
                None,
            )
            .expect("open scope fixture");
            (host, id)
        };
        let text = "country_event = { immediate = { capital_scope = { } } }\n";
        let position =
            u32::try_from(text.find("capital_scope").expect("scope link") + 1).expect("position");
        let hover = hover(&host.snapshot(), &id, position).expect("scope hover");
        assert!(hover.contents.contains("scope transition:"));
        assert!(hover.contents.contains("scope registers: ROOT=`"));
        assert!(hover.contents.contains("scope registers after:"));
        assert!(hover.contents.contains("child context:") || hover.contents.contains("context:"));
    }

    #[test]
    fn unknown_key_and_unknown_scope_are_independent_diagnostics() {
        let (host, id) = semantic_snapshot("trigger = { unknown_key = yes scope = nowhere }\n");
        let diagnostics = diagnostics(&host.snapshot(), &id);
        assert!(
            diagnostics
                .iter()
                .any(|item| item.code == DiagnosticCode::UnknownKey)
        );
        assert!(
            diagnostics
                .iter()
                .any(|item| item.code == DiagnosticCode::UnknownScope)
        );
        assert_eq!(
            diagnostics
                .iter()
                .filter(|item| item.code == DiagnosticCode::UnknownScope)
                .count(),
            1
        );
    }

    #[test]
    fn uncovered_semantic_context_is_syntax_only() {
        let (host, id) = semantic_snapshot("uncovered_root = { perfectly_valid_key = yes }\n");
        let diagnostics = diagnostics(&host.snapshot(), &id);
        assert!(
            diagnostics
                .iter()
                .all(|item| item.code != DiagnosticCode::UnknownKey),
            "an uncovered semantic context must not fabricate unknown-key diagnostics"
        );
    }

    #[test]
    fn semantic_matcher_rejects_invalid_values_and_unknown_keys() {
        let (host, id) = semantic_snapshot("trigger = { foo = maybe unknown = yes }\n");
        let diagnostics = diagnostics(&host.snapshot(), &id);
        assert!(
            diagnostics
                .iter()
                .any(|item| item.code == DiagnosticCode::InvalidValue)
        );
        assert!(
            diagnostics
                .iter()
                .any(|item| item.code == DiagnosticCode::UnknownKey)
        );
    }

    #[test]
    fn semantic_rule_severity_reaches_editor_diagnostic() {
        let (host, id) = semantic_snapshot_with_severity("trigger = { foo = maybe }\n", Some(2));
        let diagnostics = diagnostics(&host.snapshot(), &id);
        let invalid_value = diagnostics
            .iter()
            .find(|item| item.code == DiagnosticCode::InvalidValue)
            .expect("invalid semantic rules value diagnostic");
        assert_eq!(invalid_value.severity, 2);
        assert!(invalid_value.message.contains("rule fixture.semantic:1"));
    }

    #[test]
    fn semantic_matcher_enforces_max_cardinality() {
        let (host, id) = semantic_snapshot("trigger = { foo = yes foo = no }\n");
        let results = diagnostics(&host.snapshot(), &id);
        assert!(
            results
                .iter()
                .any(|item| item.code == DiagnosticCode::Cardinality)
        );
    }

    #[test]
    fn logical_scope_wrappers_keep_the_trigger_context() {
        let (host, id) = semantic_snapshot("trigger = { OR = { foo = yes } NOT = { foo = no } }\n");
        let results = diagnostics(&host.snapshot(), &id);
        assert!(
            !results
                .iter()
                .any(|item| item.code == DiagnosticCode::UnknownKey)
        );
    }

    #[test]
    fn alias_definition_cardinality_does_not_limit_repeated_effect_commands() {
        let rules = pdx_game::eu4::first_party_rules().expect("load first-party rules");
        let mut host = eu4_host(rules);
        let id = DocumentId::new("file:///tmp/events/repeated-tooltip.txt");
        host.open_document(
            id.clone(),
            1,
            "effect = { custom_tooltip = first custom_tooltip = second }\n".to_owned(),
            None,
        )
        .expect("open");
        let results = diagnostics(&host.snapshot(), &id);
        assert!(
            !results
                .iter()
                .any(|item| item.code == DiagnosticCode::Cardinality)
        );
    }

    #[test]
    fn semantic_matcher_enforces_min_cardinality() {
        let (host, id) =
            semantic_snapshot_with_constraints("trigger = { }\n", None, Some(1), Some(1));
        assert!(
            diagnostics(&host.snapshot(), &id)
                .iter()
                .any(|item| item.code == DiagnosticCode::Cardinality)
        );
    }

    #[test]
    fn semantic_value_clause_validates_bare_values_and_cardinality() {
        let mut model = pdx_game::eu4::bootstrap_model();
        model.semantic.rules.push(SemanticRule {
            id: "fixture:terrain:color".to_owned(),
            context: "terrain".to_owned(),
            parent_path: Vec::new(),
            key: KeyMatcher::Exact("color".to_owned()),
            operator: Some("=".to_owned()),
            value: ValueMatcher::AnyScalar,
            shape: RuleShape::ValueClause,
            child_context: None,
            alternative_id: None,
            severity: None,
            required: false,
            documentation: vec!["RGB color clause".to_owned()],
            allowed_scopes: Vec::new(),
            push_scope: None,
            replace_scope: Vec::new(),
            min_occurs: None,
            strict_min: true,
            max_occurs: Some(1),
            source_file: "fixture.semantic".to_owned(),
            line: 1,
        });
        model.semantic.rules.push(SemanticRule {
            id: "fixture:terrain:color:int".to_owned(),
            context: "terrain".to_owned(),
            parent_path: vec!["color".to_owned()],
            key: KeyMatcher::AnyScalar,
            operator: None,
            value: ValueMatcher::Int {
                min: Some(0),
                max: Some(255),
            },
            shape: RuleShape::LeafValue,
            child_context: None,
            alternative_id: None,
            severity: None,
            required: false,
            documentation: Vec::new(),
            allowed_scopes: Vec::new(),
            push_scope: None,
            replace_scope: Vec::new(),
            min_occurs: Some(3),
            strict_min: true,
            max_occurs: Some(3),
            source_file: "fixture.semantic".to_owned(),
            line: 2,
        });
        let mut host = eu4_host(RuleSet::from_model(model));
        let id = DocumentId::new("file:///tmp/common/terrain/test.txt");
        host.open_document(
            id.clone(),
            1,
            "terrain = { color = { 1 2 300 } }\n".to_owned(),
            None,
        )
        .expect("open");
        let diagnostics = diagnostics(&host.snapshot(), &id);
        assert!(
            diagnostics
                .iter()
                .any(|item| item.code == DiagnosticCode::InvalidValue)
        );
        assert!(
            diagnostics
                .iter()
                .any(|item| item.code == DiagnosticCode::Cardinality)
        );
    }

    #[test]
    fn semantic_rules_drive_value_completion_and_hover() {
        let (host, id) = semantic_snapshot("trigger = { foo = yes }\n");
        let snapshot = host.snapshot();
        let value = u32::try_from("trigger = { foo = ".len()).expect("offset");
        let result = complete(&snapshot, &id, value);
        assert!(result.items.iter().any(|item| item.label == "yes"));
        let property = u32::try_from("trigger = { ".len() + 1).expect("offset");
        let property_hover = hover(&snapshot, &id, property).expect("semantic hover");
        assert!(property_hover.contents.contains("PDX property `foo`"));
        assert!(
            property_hover
                .contents
                .starts_with("### PDX property `foo`")
        );
        assert!(
            property_hover
                .contents
                .contains("#### Rule\n\n- context: `trigger`")
        );
        assert!(property_hover.contents.contains("context: `trigger`"));
        assert!(property_hover.contents.contains("shape: `scalar`"));
        assert!(
            property_hover
                .contents
                .contains("value: `bool (`yes` / `no`)`")
        );
        assert!(
            property_hover
                .contents
                .contains("rule: `fixture.semantic:1`")
        );

        let value_position = u32::try_from("trigger = { foo = yes".find("yes").expect("value") + 1)
            .expect("value offset");
        let value_hover = hover(&snapshot, &id, value_position).expect("value hover");
        assert!(value_hover.contents.contains("PDX value `yes`"));
        assert!(value_hover.contents.starts_with("### PDX value `yes`"));
        assert!(value_hover.contents.contains("- validation: `accepted`"));
        assert!(value_hover.contents.contains("validation: `accepted`"));

        let (invalid_host, invalid_id) = semantic_snapshot("trigger = { foo = maybe }\n");
        let invalid_text = "trigger = { foo = maybe }\n";
        let invalid_position =
            u32::try_from(invalid_text.find("maybe").expect("invalid value") + 1)
                .expect("invalid offset");
        let invalid_hover = hover(&invalid_host.snapshot(), &invalid_id, invalid_position)
            .expect("invalid value hover");
        assert!(
            invalid_hover
                .contents
                .contains("validation: `does not match`")
        );
    }

    #[test]
    fn hover_ignores_unknown_property_and_plain_text() {
        let (host, id) = semantic_snapshot("trigger = { unknown_property = yes }\n");
        let analysis_snapshot = host.snapshot();
        let property = u32::try_from("trigger = { ".len() + 2).expect("offset");
        assert!(hover(&analysis_snapshot, &id, property).is_none());

        let (host, id) = snapshot("# ordinary comment text\n");
        assert!(
            hover(
                &host.snapshot(),
                &id,
                u32::try_from("# ordinary ".len()).expect("offset")
            )
            .is_none()
        );
    }

    #[test]
    fn semantic_hover_keeps_multiple_matching_rule_meanings() {
        let mut model = pdx_game::eu4::bootstrap_model();
        for (id, value) in [
            ("fixture:trigger:choice-bool", ValueMatcher::Bool),
            (
                "fixture:trigger:choice-int",
                ValueMatcher::Int {
                    min: Some(1),
                    max: Some(3),
                },
            ),
        ] {
            model.semantic.rules.push(SemanticRule {
                id: id.to_owned(),
                context: "trigger".to_owned(),
                parent_path: Vec::new(),
                key: KeyMatcher::Exact("choice".to_owned()),
                operator: Some("=".to_owned()),
                value,
                shape: RuleShape::Leaf,
                child_context: None,
                alternative_id: None,
                severity: None,
                required: false,
                documentation: Vec::new(),
                allowed_scopes: Vec::new(),
                push_scope: None,
                replace_scope: Vec::new(),
                min_occurs: None,
                strict_min: true,
                max_occurs: Some(1),
                source_file: "fixture.semantic".to_owned(),
                line: 1,
            });
        }
        let mut host = eu4_host(RuleSet::from_model(model));
        let id = DocumentId::new("file:///tmp/choice.txt");
        let text = "trigger = { choice = yes }\n";
        host.open_document(id.clone(), 1, text.to_owned(), None)
            .expect("open ambiguous rule fixture");
        let position = u32::try_from(text.find("choice").expect("choice") + 1).expect("position");
        let hover = hover(&host.snapshot(), &id, position).expect("ambiguous rule hover");
        assert!(hover.contents.contains("2 possible semantic meanings"));
        assert!(hover.contents.contains("#### 2 possible semantic meanings"));
        assert!(hover.contents.contains("##### Candidate 1"));
        assert!(hover.contents.contains("value: `bool (`yes` / `no`)`"));
        assert!(hover.contents.contains("value: `integer in [1, 3]`"));
    }

    #[test]
    fn semantic_hover_preserves_rule_detail_line_breaks() {
        let mut model = pdx_game::eu4::bootstrap_model();
        model.semantic.rules.push(SemanticRule {
            id: "fixture:trigger:documented".to_owned(),
            context: "trigger".to_owned(),
            parent_path: Vec::new(),
            key: KeyMatcher::Exact("documented".to_owned()),
            operator: None,
            value: ValueMatcher::Bool,
            shape: RuleShape::Leaf,
            child_context: None,
            alternative_id: None,
            severity: None,
            required: false,
            documentation: vec!["first line".to_owned(), "second line".to_owned()],
            allowed_scopes: Vec::new(),
            push_scope: None,
            replace_scope: Vec::new(),
            min_occurs: None,
            strict_min: true,
            max_occurs: Some(1),
            source_file: "fixture.semantic".to_owned(),
            line: 1,
        });
        let mut host = eu4_host(RuleSet::from_model(model));
        let id = DocumentId::new("file:///tmp/documented.txt");
        let text = "trigger = { documented = yes }\n";
        host.open_document(id.clone(), 1, text.to_owned(), None)
            .expect("open documented fixture");
        let position = u32::try_from(text.find("documented").expect("documented key") + 2)
            .expect("hover position");
        let hover = hover(&host.snapshot(), &id, position).expect("documented hover");
        assert!(
            hover
                .contents
                .contains("#### Documentation\n\nfirst line  \nsecond line")
        );
        assert!(hover.contents.contains("first line  \nsecond line"));
    }

    #[test]
    fn embedded_first_party_rules_drive_runtime_value_diagnostics() {
        let rules = pdx_game::eu4::first_party_rules().expect("load first-party rules");
        assert!(!rules.model().semantic.rules.is_empty());
        assert!(
            rules
                .model()
                .semantic
                .rules
                .iter()
                .any(|rule| rule.severity == Some(2))
        );
        assert!(
            rules
                .model()
                .semantic
                .rules
                .iter()
                .any(|rule| rule.min_occurs == Some(1))
        );
        let mut host = eu4_host(rules);
        let id = DocumentId::new("file:///tmp/common/events/test.txt");
        host.open_document(
            id.clone(),
            1,
            "trigger = { ai = maybe definitely_not_a_trigger = yes }\n".to_owned(),
            None,
        )
        .expect("open");
        let diagnostics = diagnostics(&host.snapshot(), &id);
        assert!(
            diagnostics
                .iter()
                .any(|item| item.code == DiagnosticCode::InvalidValue)
        );
        assert!(
            diagnostics
                .iter()
                .any(|item| item.code == DiagnosticCode::UnknownKey)
        );
    }

    #[test]
    fn semantic_type_selector_applies_event_rules_to_country_event() {
        let rules = pdx_game::eu4::first_party_rules().expect("load first-party rules");
        let mut host = eu4_host(rules);
        let id = DocumentId::new("file:///tmp/events/test.txt");
        host.open_document(
            id.clone(),
            1,
            "country_event = { id = test.1 definitely_not_an_event_key = yes }\n".to_owned(),
            None,
        )
        .expect("open");
        assert!(
            diagnostics(&host.snapshot(), &id)
                .iter()
                .any(|item| item.code == DiagnosticCode::UnknownKey)
        );
    }

    #[test]
    fn area_scope_transition_keeps_province_trigger_valid() {
        use pdx_engine::{SourceRoot, SourceRootId, SourceRootKind, WorkspaceChange};
        use std::fs;

        let rules = pdx_game::eu4::first_party_rules().expect("load first-party rules");
        let mut profile = pdx_game::eu4::profile();
        profile.definitions.push(ProfileDefinitionRule {
            path: ProfileTextMatcher::insensitive(ProfileMatchMode::Exact, "map/area.txt"),
            key: ProfileTextMatcher::any(),
            kind: "area".to_owned(),
            name_field: None,
            requires_value: false,
        });
        let mut host = AnalysisHost::with_profile(rules, profile);
        let root = std::env::temp_dir().join(format!(
            "pdx-analysis-area-scope-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let area_path = root.join("map/area.txt");
        let event_path = root.join("events/EDG_KTPEvents.txt");
        fs::create_dir_all(area_path.parent().expect("area parent")).expect("area directory");
        fs::create_dir_all(event_path.parent().expect("event parent")).expect("event directory");
        fs::write(&area_path, "tripolitania_area = { 1 2 }\n").expect("area source");
        host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
            SourceRootId::new(1),
            SourceRootKind::CurrentMod,
            root.clone(),
        )]));
        host.refresh_source_roots().expect("index area definitions");
        let id = DocumentId::new("file:///tmp/events/EDG_KTPEvents.txt");
        let text = concat!(
            "country_event = {\n",
            "  immediate = {\n",
            "    tripolitania_area = {\n",
            "      limit = { country_or_non_sovereign_subject_holds = ROOT }\n",
            "    }\n",
            "  }\n",
            "}\n",
        );
        host.open_document(id.clone(), 1, text.to_owned(), Some(event_path))
            .expect("open");

        let results = diagnostics(&host.snapshot(), &id);
        assert!(
            !results.iter().any(|item| {
                item.code == DiagnosticCode::UnknownKey
                    && item.message.contains("`tripolitania_area`")
            }),
            "area name was not accepted as a dynamic area key: {results:?}"
        );
        assert!(
            !results.iter().any(|item| {
                item.code == DiagnosticCode::WrongScope
                    && item
                        .message
                        .contains("`country_or_non_sovereign_subject_holds`")
            }),
            "province trigger was diagnosed in the parent country scope: {results:?}"
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn eu4_normal_type_selector_applies_mission_rules_to_custom_root_names() {
        use pdx_engine::{SourceRoot, SourceRootId, SourceRootKind, WorkspaceChange};
        use std::fs;

        let root = std::env::temp_dir().join(format!(
            "pdx-analysis-cwt-missions-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        fs::create_dir_all(root.join("missions")).expect("missions directory");
        let rules = pdx_game::eu4::first_party_rules().expect("load first-party rules");
        let mut host = eu4_host(rules);
        host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
            SourceRootId::new(1),
            SourceRootKind::CurrentMod,
            root.clone(),
        )]));
        let path = root.join("missions/EDG_Bavarian_Missions.txt");
        let source = "EDG_Bavarian_Missions = { slot = 1 generic = no ai = yes has_country_shield = yes potential = { } EDG_bav_claim = { required_missions = { potential } } }\n";
        fs::write(&path, source).expect("write mission document");
        host.refresh_source_roots().expect("index mission document");
        let id = DocumentId::new("file:///tmp/EDG_Bavarian_Missions.txt");
        host.open_document(id.clone(), 1, source.to_owned(), Some(path))
            .expect("open mission document");
        let results = diagnostics(&host.snapshot(), &id);
        assert!(
            !results.iter().any(|item| {
                item.code == DiagnosticCode::UnknownKey
                    && [
                        "slot",
                        "generic",
                        "ai",
                        "has_country_shield",
                        "EDG_bav_claim",
                    ]
                    .iter()
                    .any(|key| item.message.contains(&format!("`{key}`")))
            }),
            "mission fields were not selected by the path-based type: {results:?}"
        );
        assert!(
            results.iter().any(|item| {
                item.code == DiagnosticCode::InvalidValue && item.message.contains("potential")
            }),
            "negative type_key_filter was not applied to <mission>: {results:?}"
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn eu4_starts_with_type_selector_applies_on_action_rules() {
        let rules = pdx_game::eu4::first_party_rules().expect("load first-party rules");
        let mut host = eu4_host(rules);
        let id = DocumentId::new("file:///tmp/common/on_actions/test.txt");
        host.open_document(
            id.clone(),
            1,
            "on_harmonized_religiongroup = { definitely_not_an_on_action_key = yes }\n".to_owned(),
            None,
        )
        .expect("open");
        assert!(
            diagnostics(&host.snapshot(), &id)
                .iter()
                .any(|item| item.code == DiagnosticCode::UnknownKey)
        );
    }

    #[test]
    fn eu4_starts_with_type_selector_still_requires_a_matching_path() {
        let rules = pdx_game::eu4::first_party_rules().expect("load first-party rules");
        let host = eu4_host(rules);
        let snapshot = host.snapshot();
        let valid_path =
            LogicalPath::parse("common/on_actions/test.txt").expect("valid on-action path");
        let unrelated_path = LogicalPath::parse("events/test.txt").expect("valid unrelated path");

        assert_eq!(
            semantic_root_context(&snapshot, "on_harmonized_religiongroup", Some(&valid_path))
                .as_deref(),
            Some("type:on_action")
        );
        assert_ne!(
            semantic_root_context(
                &snapshot,
                "on_harmonized_religiongroup",
                Some(&unrelated_path)
            )
            .as_deref(),
            Some("type:on_action")
        );
    }

    #[test]
    fn eu4_scope_links_switch_effect_context_and_scope() {
        let rules = pdx_game::eu4::first_party_rules().expect("load first-party rules");
        let mut host = eu4_host(rules);
        let id = DocumentId::new("file:///tmp/events/scope.txt");
        host.open_document(
            id.clone(),
            1,
            "country_event = { immediate = { capital_scope = { add_base_tax = nope } } }\n"
                .to_owned(),
            None,
        )
        .expect("open");
        assert!(
            diagnostics(&host.snapshot(), &id)
                .iter()
                .any(|item| item.code == DiagnosticCode::InvalidValue)
        );
    }

    #[test]
    fn eu4_scope_link_chains_are_resolved_segment_by_segment() {
        let rules = pdx_game::eu4::first_party_rules().expect("load first-party rules");
        let host = eu4_host(rules);
        let snapshot = host.snapshot();
        let mut context = super::ScopeContext::new(std::sync::Arc::new(pdx_game::eu4::profile()));
        context.root = "province".to_owned();
        context.current = "province".to_owned();

        assert_eq!(
            super::resolve_scope_expression_context(&snapshot, &context, "owner.capital_scope"),
            "province"
        );
        assert_eq!(
            super::resolve_scope_expression_context(&snapshot, &context, "owner.missing_link"),
            "any"
        );

        let mut invalid_register_rule = snapshot.rules().model().semantic.rules[0].clone();
        invalid_register_rule.push_scope = None;
        invalid_register_rule.replace_scope = vec![
            ("from_owner".to_owned(), "country".to_owned()),
            ("previous_owner".to_owned(), "country".to_owned()),
        ];
        let unchanged = super::semantic_child_scope(&snapshot, &context, &invalid_register_rule);
        assert!(unchanged.from.is_empty());
        assert!(unchanged.previous.is_empty());
    }

    #[test]
    fn eu4_alias_alternatives_do_not_cross_report_cardinality() {
        let rules = pdx_game::eu4::first_party_rules().expect("load first-party rules");
        let mut host = eu4_host(rules);
        let id = DocumentId::new("file:///tmp/events/alternatives.txt");
        host.open_document(
            id.clone(),
            1,
            "effect = { multiply_variable = { which = $foo$ value = 1 } }\n".to_owned(),
            None,
        )
        .expect("open");
        let results = diagnostics(&host.snapshot(), &id);
        assert!(
            !results
                .iter()
                .any(|item| item.code == DiagnosticCode::Cardinality),
            "unexpected diagnostics: {results:?}"
        );
    }

    #[test]
    fn semantic_alternative_selection_refuses_equal_scores() {
        let host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
        let snapshot = host.snapshot();
        let mut left = snapshot.rules().model().semantic.rules[0].clone();
        left.id = "fixture:left".to_owned();
        left.context = "fixture".to_owned();
        left.parent_path.clear();
        left.key = KeyMatcher::Exact("left".to_owned());
        left.shape = RuleShape::Leaf;
        left.value = ValueMatcher::Bool;
        left.alternative_id = Some("left-alternative".to_owned());
        left.allowed_scopes.clear();
        let mut right = left.clone();
        right.id = "fixture:right".to_owned();
        right.key = KeyMatcher::Exact("right".to_owned());
        right.alternative_id = Some("right-alternative".to_owned());
        let rules = [&left, &right];
        let scope = super::ScopeContext::new(std::sync::Arc::new(pdx_game::eu4::profile()));
        assert_eq!(
            super::semantic_selected_alternative(&snapshot, &rules, &[], &[], &[], &scope),
            None
        );

        let property = super::ScriptProperty {
            key: "left".to_owned(),
            key_range: TextRange::empty(0),
            range: TextRange::empty(0),
            operator: None,
            scalar: Some(("yes".to_owned(), TextRange::empty(0))),
            block_range: None,
            block: Vec::new(),
            bare_values: Vec::new(),
        };
        assert_eq!(
            super::semantic_selected_alternative(&snapshot, &rules, &[], &[property], &[], &scope,)
                .as_deref(),
            Some("left-alternative")
        );
    }

    #[test]
    fn workspace_type_child_key_selects_only_one_transition() {
        use pdx_engine::{SourceRoot, SourceRootId, SourceRootKind, WorkspaceChange};
        use std::fs;

        let root = std::env::temp_dir().join(format!(
            "pdx-analysis-dynamic-transition-{}",
            std::process::id()
        ));
        fs::create_dir_all(root.join("common/country_tags")).expect("country tag directory");
        fs::write(
            root.join("common/country_tags/00_test.txt"),
            "FRA = \"countries/France.txt\"\n",
        )
        .expect("country tag definition");

        let mut model = pdx_game::eu4::first_party_rules()
            .expect("load first-party rules")
            .model()
            .clone();
        let mut country_transition = model.semantic.rules[0].clone();
        country_transition.id = "fixture:country-transition".to_owned();
        country_transition.context = "fixture".to_owned();
        country_transition.parent_path.clear();
        country_transition.key = KeyMatcher::Exact("choose".to_owned());
        country_transition.shape = RuleShape::Node;
        country_transition.child_context = Some("country-destination".to_owned());
        country_transition.alternative_id = None;
        country_transition.allowed_scopes.clear();
        country_transition.push_scope = None;
        country_transition.replace_scope.clear();
        let mut other_transition = country_transition.clone();
        other_transition.id = "fixture:other-transition".to_owned();
        other_transition.child_context = Some("other-destination".to_owned());

        let mut country_child = country_transition.clone();
        country_child.id = "fixture:country-child".to_owned();
        country_child.context = "country-destination".to_owned();
        country_child.key = KeyMatcher::Type("country_tag".to_owned());
        country_child.shape = RuleShape::Leaf;
        country_child.child_context = None;
        country_child.value = ValueMatcher::Bool;
        let mut other_child = country_child.clone();
        other_child.id = "fixture:other-child".to_owned();
        other_child.context = "other-destination".to_owned();
        other_child.key = KeyMatcher::Exact("other".to_owned());
        model.semantic.rules.extend([
            country_transition.clone(),
            other_transition.clone(),
            country_child,
            other_child,
        ]);

        let mut host = eu4_host(RuleSet::from_model(model));
        host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot {
            id: SourceRootId::new(1),
            kind: SourceRootKind::CurrentMod,
            path: root.clone(),
            order: 0,
            writable: true,
        }]));
        host.refresh_source_roots()
            .expect("scan country tag definition");
        let snapshot = host.snapshot();
        let scope = super::ScopeContext::new(std::sync::Arc::new(pdx_game::eu4::profile()));
        let mut property = super::ScriptProperty {
            key: "choose".to_owned(),
            key_range: TextRange::empty(0),
            range: TextRange::empty(0),
            operator: Some("=".to_owned()),
            scalar: None,
            block_range: Some(TextRange::empty(0)),
            block: vec![super::ScriptProperty {
                key: "FRA".to_owned(),
                key_range: TextRange::empty(0),
                range: TextRange::empty(0),
                operator: Some("=".to_owned()),
                scalar: Some(("yes".to_owned(), TextRange::empty(0))),
                block_range: None,
                block: Vec::new(),
                bare_values: Vec::new(),
            }],
            bare_values: Vec::new(),
        };
        let selected = super::semantic_selected_transition(
            &snapshot,
            &[&country_transition, &other_transition],
            None,
            "fixture",
            &[],
            &property,
            &scope,
            false,
        )
        .expect("workspace-backed child key selects a transition");
        assert_eq!(
            selected.child_context.as_deref(),
            Some("country-destination")
        );

        property.block[0].key = "MISSING".to_owned();
        assert!(
            super::semantic_selected_transition(
                &snapshot,
                &[&country_transition, &other_transition],
                None,
                "fixture",
                &[],
                &property,
                &scope,
                false,
            )
            .is_none(),
            "an unresolved child key must not fall back to rule order"
        );

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn unresolved_game_age_ability_does_not_descend_cached_scope_fact() {
        use std::fs;

        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("pdx-analysis-game-age-{nonce}"));
        let ages = root.join("common/ages");
        fs::create_dir_all(&ages).expect("ages directory");
        fs::write(
            ages.join("00_abilities.txt"),
            "abilities = { known_ability = { effect = { custom_tooltip = missing_loc } } }\n",
        )
        .expect("ability source");

        let rules = pdx_game::eu4::first_party_rules().expect("first-party rules");
        let mut host = eu4_host(rules);
        host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
            SourceRootId::new(1),
            SourceRootKind::CurrentMod,
            root.clone(),
        )]));
        host.refresh_source_roots().expect("index ability source");

        let id = DocumentId::new("file:///tmp/common/ages/target.txt");
        let source = concat!(
            "age_of_discovery = { abilities = { ",
            "known_ability = { effect = { custom_tooltip = missing_loc } } ",
            "MISSING = { effect = { custom_tooltip = missing_loc } } ",
            "} }\n",
        );
        host.open_document(
            id.clone(),
            1,
            source.to_owned(),
            Some(ages.join("target.txt")),
        )
        .expect("open target");

        let diagnostics = diagnostics(&host.snapshot(), &id);
        let missing_loc_ranges = source
            .match_indices("missing_loc")
            .map(|(start, _)| {
                TextRange::new(start as u32, (start + "missing_loc".len()) as u32).expect("range")
            })
            .collect::<Vec<_>>();
        let missing_key = diagnostics.iter().filter(|item| {
            item.code == DiagnosticCode::UnknownKey && item.message.contains("`MISSING`")
        });
        assert_eq!(
            missing_key.count(),
            1,
            "unresolved ability should report one key"
        );
        let missing_symbols = diagnostics.iter().filter(|item| {
            item.code == DiagnosticCode::UnknownSymbol && item.message.contains("`missing_loc`")
        });
        assert_eq!(
            missing_symbols.count(),
            1,
            "known ability should still validate its value"
        );
        assert!(
            diagnostics.iter().all(|item| {
                item.code != DiagnosticCode::UnknownSymbol || item.range != missing_loc_ranges[1]
            }),
            "the unresolved ability must not cascade to its missing_loc value"
        );

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn unresolved_game_age_ability_without_index_does_not_descend_cached_scope_fact() {
        use std::fs;

        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("pdx-analysis-empty-game-age-{nonce}"));
        let ages = root.join("common/ages");
        fs::create_dir_all(&ages).expect("ages directory");

        let rules = pdx_game::eu4::first_party_rules().expect("first-party rules");
        let mut host = eu4_host(rules);
        host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
            SourceRootId::new(1),
            SourceRootKind::CurrentMod,
            root.clone(),
        )]));
        let id = DocumentId::new("file:///tmp/common/ages/empty-target.txt");
        let source = "age_of_discovery = { abilities = { MISSING = { effect = { custom_tooltip = missing_loc } } } }\n";
        host.open_document(
            id.clone(),
            1,
            source.to_owned(),
            Some(ages.join("empty-target.txt")),
        )
        .expect("open target");

        let diagnostics = diagnostics(&host.snapshot(), &id);
        let missing_loc_start = source.find("missing_loc").expect("missing localisation") as u32;
        assert!(
            diagnostics.iter().any(|item| {
                item.code == DiagnosticCode::UnknownKey && item.message.contains("`MISSING`")
            }),
            "the unresolved ability should retain its parent key diagnostic: {diagnostics:?}"
        );
        assert!(
            diagnostics.iter().all(|item| {
                item.code != DiagnosticCode::UnknownSymbol
                    || item.range.start() != missing_loc_start
            }),
            "an empty type index must not cascade into missing_loc: {diagnostics:?}"
        );

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn eu4_common_links_allow_owner_to_push_province_scope_to_country() {
        let rules = pdx_game::eu4::first_party_rules().expect("load first-party rules");
        let mut host = eu4_host(rules);
        let id = DocumentId::new("file:///tmp/events/owner.txt");
        host.open_document(
            id.clone(),
            1,
            "province_event = { immediate = { owner = { add_treasury = nope } } }\n".to_owned(),
            None,
        )
        .expect("open");
        let results = diagnostics(&host.snapshot(), &id);
        assert!(
            results
                .iter()
                .any(|item| item.code == DiagnosticCode::InvalidValue)
        );
        assert!(
            !results
                .iter()
                .any(|item| item.code == DiagnosticCode::UnknownKey)
        );
    }

    #[test]
    fn eu4_replace_scope_links_populate_from_intrinsics() {
        use pdx_engine::{SourceRoot, SourceRootId, SourceRootKind, WorkspaceChange};
        use std::fs;

        assert_eq!(
            super::repeated_scope_register_depth("prevprev", "prev"),
            Some(1)
        );
        assert_eq!(
            super::repeated_scope_register_depth("previous_owner", "previous"),
            None
        );

        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("pdx-analysis-scope-intrinsics-{nonce}"));
        let directory = root.join("common/buildings");
        fs::create_dir_all(&directory).expect("building directory");
        let rules = pdx_game::eu4::first_party_rules().expect("load first-party rules");
        let mut host = eu4_host(rules);
        host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
            SourceRootId::new(1),
            SourceRootKind::CurrentMod,
            root.clone(),
        )]));

        let valid_id = DocumentId::new("file:///tmp/from-building.txt");
        host.open_document(
            valid_id.clone(),
            1,
            "test_building = { on_built = { cossack_infantry = FROM } }\n".to_owned(),
            Some(directory.join("from.txt")),
        )
        .expect("open FROM fixture");
        assert!(
            diagnostics(&host.snapshot(), &valid_id)
                .iter()
                .all(|item| item.code != DiagnosticCode::InvalidValue)
        );

        let invalid_id = DocumentId::new("file:///tmp/this-building.txt");
        host.open_document(
            invalid_id.clone(),
            1,
            "other_building = { on_built = { cossack_infantry = THIS } }\n".to_owned(),
            Some(directory.join("this.txt")),
        )
        .expect("open THIS fixture");
        assert!(
            diagnostics(&host.snapshot(), &invalid_id)
                .iter()
                .any(|item| item.code == DiagnosticCode::InvalidValue)
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn eu4_dynamic_culture_definition_is_used_by_semantic_type_matcher() {
        use pdx_engine::{SourceRoot, SourceRootId, SourceRootKind, WorkspaceChange};
        use std::fs;

        let root =
            std::env::temp_dir().join(format!("pdx-analysis-cwt-dynamic-{}", std::process::id()));
        fs::create_dir_all(root.join("common/cultures")).expect("culture directory");
        fs::write(root.join("common/cultures/00_test.txt"), "french = { }\n")
            .expect("culture definition");
        let rules = pdx_game::eu4::first_party_rules().expect("load first-party rules");
        let mut host = eu4_host(rules);
        host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot {
            id: SourceRootId::new(1),
            kind: SourceRootKind::CurrentMod,
            path: root.clone(),
            order: 0,
            writable: true,
        }]));
        host.refresh_source_roots()
            .expect("scan culture definition");
        let id = DocumentId::new("file:///tmp/events/culture.txt");
        host.open_document(
            id.clone(),
            1,
            "country_event = { trigger = { culture = french } }\n".to_owned(),
            None,
        )
        .expect("open");
        let results = diagnostics(&host.snapshot(), &id);
        assert!(
            results
                .iter()
                .all(|item| item.code != DiagnosticCode::InvalidValue)
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn eu4_country_tag_definition_feeds_dynamic_enum_matcher() {
        use pdx_engine::{SourceRoot, SourceRootId, SourceRootKind, WorkspaceChange};
        use std::fs;

        let root =
            std::env::temp_dir().join(format!("pdx-analysis-cwt-tags-{}", std::process::id()));
        fs::create_dir_all(root.join("common/country_tags")).expect("country tag directory");
        fs::write(
            root.join("common/country_tags/00_test.txt"),
            "FRA = \"countries/France.txt\"\n",
        )
        .expect("country tag definition");
        let rules = pdx_game::eu4::first_party_rules().expect("load first-party rules");
        let mut host = eu4_host(rules);
        host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot {
            id: SourceRootId::new(1),
            kind: SourceRootKind::CurrentMod,
            path: root.clone(),
            order: 0,
            writable: true,
        }]));
        host.refresh_source_roots()
            .expect("scan country tag definition");
        let id = DocumentId::new("file:///tmp/events/tag.txt");
        host.open_document(
            id.clone(),
            1,
            "country_event = { immediate = { change_tag = FRA } }\n".to_owned(),
            None,
        )
        .expect("open");
        assert!(
            diagnostics(&host.snapshot(), &id)
                .iter()
                .all(|item| item.code != DiagnosticCode::InvalidValue)
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn eu4_flag_definition_feeds_dynamic_value_matcher() {
        use pdx_engine::{SourceRoot, SourceRootId, SourceRootKind, WorkspaceChange};
        use std::fs;

        let root =
            std::env::temp_dir().join(format!("pdx-analysis-cwt-flags-{}", std::process::id()));
        fs::create_dir_all(root.join("events")).expect("event directory");
        fs::write(
            root.join("events/00_flags.txt"),
            "country_event = { immediate = { set_country_flag = known_flag } }\n",
        )
        .expect("flag definition");
        let rules = pdx_game::eu4::first_party_rules().expect("load first-party rules");
        let mut host = eu4_host(rules);
        host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot {
            id: SourceRootId::new(1),
            kind: SourceRootKind::CurrentMod,
            path: root.clone(),
            order: 0,
            writable: true,
        }]));
        host.refresh_source_roots().expect("scan flag definition");
        let id = DocumentId::new("file:///tmp/events/flag-use.txt");
        host.open_document(
            id.clone(),
            1,
            "country_event = { immediate = { clr_country_flag = known_flag } }\n".to_owned(),
            None,
        )
        .expect("open");
        assert!(
            diagnostics(&host.snapshot(), &id)
                .iter()
                .all(|item| item.code != DiagnosticCode::InvalidValue)
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn eu4_scripted_effect_params_are_owner_qualified() {
        use pdx_engine::{SourceRoot, SourceRootId, SourceRootKind, WorkspaceChange};
        use std::fs;

        let root =
            std::env::temp_dir().join(format!("pdx-analysis-cwt-params-{}", std::process::id()));
        fs::create_dir_all(root.join("common/scripted_effects"))
            .expect("scripted effect directory");
        let definition_path = root.join("common/scripted_effects/00_test.txt");
        fs::write(
            &definition_path,
            concat!(
                "apply = { value = $amount$ [[optional] enabled = yes ] }\n",
                "other_effect = { value = $other$ }\n",
            ),
        )
        .expect("scripted effect definition");
        let rules = pdx_game::eu4::first_party_rules().expect("load first-party rules");
        let mut host = eu4_host(rules);
        host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot {
            id: SourceRootId::new(1),
            kind: SourceRootKind::CurrentMod,
            path: root.clone(),
            order: 0,
            writable: true,
        }]));
        host.refresh_source_roots()
            .expect("scan scripted effect definition");
        let id = DocumentId::new("file:///tmp/events/params.txt");
        let invocation =
            "country_event = { immediate = { apply = { amount = 1 optional = yes } } }\n";
        host.open_document(id.clone(), 1, invocation.to_owned(), None)
            .expect("open");
        let snapshot = host.snapshot();
        assert_eq!(
            snapshot
                .index()
                .definitions("scripted_effect", "apply")
                .len(),
            1
        );
        assert_eq!(
            super::parameter_names_for_owner(&snapshot, "scripted_effect", "apply")
                .expect("resolved owner parameters"),
            ["amount", "optional"]
        );
        assert!(diagnostics(&snapshot, &id).iter().all(|item| !matches!(
            item.code,
            DiagnosticCode::InvalidValue | DiagnosticCode::UnknownKey
        )));
        let completion_position = u32::try_from(
            invocation.find("apply = { ").expect("invocation") + "apply = { ".len() - 1,
        )
        .expect("position");
        let labels = complete(&snapshot, &id, completion_position)
            .items
            .into_iter()
            .map(|item| item.label)
            .collect::<Vec<_>>();
        assert!(
            labels
                .iter()
                .any(|label| label.eq_ignore_ascii_case("amount")),
            "{labels:?}"
        );
        assert!(
            labels
                .iter()
                .any(|label| label.eq_ignore_ascii_case("optional")),
            "{labels:?}"
        );
        assert!(
            !labels
                .iter()
                .any(|label| label.eq_ignore_ascii_case("other"))
        );

        let wrong_id = DocumentId::new("file:///tmp/events/wrong-params.txt");
        host.open_document(
            wrong_id.clone(),
            1,
            "country_event = { immediate = { apply = { other = 1 } } }\n".to_owned(),
            None,
        )
        .expect("open wrong invocation");
        assert!(diagnostics(&host.snapshot(), &wrong_id).iter().any(|item| {
            item.code == DiagnosticCode::UnknownKey && item.message.contains("`other`")
        }));

        let overlay_id = DocumentId::new("file:///tmp/common/scripted_effects/00_test.txt");
        host.open_document(
            overlay_id,
            1,
            "apply = { value = $overlay_only$ }\n".to_owned(),
            Some(definition_path),
        )
        .expect("open scripted effect overlay");
        let overlay_call = DocumentId::new("file:///tmp/events/overlay-params.txt");
        host.open_document(
            overlay_call.clone(),
            1,
            "country_event = { immediate = { apply = { overlay_only = 1 amount = 2 } } }\n"
                .to_owned(),
            None,
        )
        .expect("open overlay invocation");
        let overlay_results = diagnostics(&host.snapshot(), &overlay_call);
        assert!(!overlay_results.iter().any(|item| {
            item.code == DiagnosticCode::UnknownKey && item.message.contains("`overlay_only`")
        }));
        assert!(overlay_results.iter().any(|item| {
            item.code == DiagnosticCode::UnknownKey && item.message.contains("`amount`")
        }));
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn local_parameter_navigation_stays_within_its_scripted_definition() {
        use pdx_engine::{SourceRoot, SourceRootId, SourceRootKind, WorkspaceChange};
        use std::fs;

        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("pdx-analysis-local-params-{nonce}"));
        let directory = root.join("common/scripted_effects");
        fs::create_dir_all(&directory).expect("scripted effect directory");
        let path = directory.join("parameters.txt");
        let text = concat!(
            "first = { value = $Amount$ again = $amount$ ",
            "[[optional] enabled = yes ] }\n",
            "second = { value = $amount$ }\n",
        );
        let mut host = eu4_host(pdx_game::eu4::bootstrap_rules());
        host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
            SourceRootId::new(1),
            SourceRootKind::CurrentMod,
            root.clone(),
        )]));
        let id = DocumentId::new("file:///tmp/parameters.txt");
        host.open_document(id.clone(), 1, text.to_owned(), Some(path.clone()))
            .expect("open parameter document");
        let snapshot = host.snapshot();

        let second_use =
            u32::try_from(text.find("$amount$").expect("second use") + 1).expect("position");
        let first_name = TextRange::new(
            u32::try_from(text.find("Amount").expect("first definition")).expect("start"),
            u32::try_from(text.find("Amount").expect("first definition") + "Amount".len())
                .expect("end"),
        )
        .expect("first name range");
        assert_eq!(definition(&snapshot, &id, second_use)[0].range, first_name);

        let local_references = references(&snapshot, &id, second_use, true);
        assert_eq!(local_references.len(), 2);
        assert!(local_references.iter().all(|location| {
            location.range.end()
                < u32::try_from(text.find("second").expect("second definition")).expect("offset")
        }));

        let optional =
            u32::try_from(text.find("optional").expect("conditional parameter")).expect("offset");
        let hover = hover(&snapshot, &id, optional).expect("parameter hover");
        assert!(hover.contents.starts_with("### parameter `optional`"));
        assert!(hover.contents.contains("parameter `optional`"));
        assert!(hover.contents.contains("Arity: `optional`"));

        let preparation = prepare_rename(&snapshot, &id, second_use).expect("prepare local rename");
        assert_eq!(preparation.placeholder, "amount");
        let rename_plan = rename(&snapshot, &id, second_use, "total").expect("local rename");
        assert_eq!(rename_plan.edits.len(), 2);
        assert!(rename_plan.edits.iter().all(|edit| {
            edit.new_text == "total"
                && edit.location.range.end()
                    < u32::try_from(text.find("second").expect("second definition"))
                        .expect("offset")
        }));
        assert_eq!(
            rename(&snapshot, &id, optional, "feature")
                .expect("conditional rename")
                .edits
                .len(),
            1
        );
        assert_eq!(
            rename(&snapshot, &id, second_use, "optional"),
            Err(RenameError::Conflict)
        );
        assert_eq!(
            rename(&snapshot, &id, second_use, "$invalid$"),
            Err(RenameError::InvalidName)
        );

        let parameter_symbols = document_symbols(&snapshot, &id)
            .into_iter()
            .filter(|symbol| symbol.kind == "parameter")
            .collect::<Vec<_>>();
        assert_eq!(
            parameter_symbols
                .iter()
                .map(|symbol| symbol.name.as_str())
                .collect::<Vec<_>>(),
            vec!["Amount", "optional", "amount"]
        );
        assert_eq!(parameter_symbols[0].selection_range, first_name);
        assert!(
            workspace_symbols(&snapshot, "amount")
                .iter()
                .all(|symbol| symbol.kind != "parameter")
        );

        let second_owner_use =
            u32::try_from(text.rfind("$amount$").expect("second owner use") + 1).expect("position");
        let second_target = definition(&snapshot, &id, second_owner_use);
        assert_eq!(second_target.len(), 1);
        assert!(second_target[0].range.start() > first_name.end());

        let mut read_only = eu4_host(pdx_game::eu4::bootstrap_rules());
        read_only.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
            SourceRootId::new(2),
            SourceRootKind::Dependency,
            root.clone(),
        )]));
        let read_only_id = DocumentId::new("file:///tmp/read-only-parameters.txt");
        read_only
            .open_document(read_only_id.clone(), 1, text.to_owned(), Some(path))
            .expect("open dependency parameter document");
        let read_only_snapshot = read_only.snapshot();
        assert_eq!(
            prepare_rename(&read_only_snapshot, &read_only_id, second_use),
            Err(RenameError::ReadOnly)
        );
        assert_eq!(
            rename(&read_only_snapshot, &read_only_id, second_use, "total"),
            Err(RenameError::ReadOnly)
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn eu4_legacy_governments_use_eu4_reform_semantics() {
        use pdx_engine::{SourceRoot, SourceRootId, SourceRootKind, WorkspaceChange};
        use std::fs;

        let root =
            std::env::temp_dir().join(format!("pdx-analysis-cwt-legacy-{}", std::process::id()));
        fs::create_dir_all(root.join("common/government_reforms")).expect("reform directory");
        fs::write(
            root.join("common/government_reforms/00_test.txt"),
            "reform_a = { legacy_government = yes }\n",
        )
        .expect("legacy reform definition");
        let rules = pdx_game::eu4::first_party_rules().expect("load first-party rules");
        let mut host = eu4_host(rules);
        host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot {
            id: SourceRootId::new(1),
            kind: SourceRootKind::CurrentMod,
            path: root.clone(),
            order: 0,
            writable: true,
        }]));
        host.refresh_source_roots()
            .expect("scan legacy reform definition");
        let id = DocumentId::new("file:///tmp/events/legacy.txt");
        host.open_document(
            id.clone(),
            1,
            "country_event = { immediate = { set_legacy_government = reform_a } }\n".to_owned(),
            None,
        )
        .expect("open");
        assert!(
            diagnostics(&host.snapshot(), &id)
                .iter()
                .all(|item| item.code != DiagnosticCode::InvalidValue)
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn localisation_values_offer_indexed_localisation_symbols() {
        let mut host = eu4_host(pdx_game::eu4::bootstrap_rules());
        let id = DocumentId::new("file:///tmp/localisation/test.yml");
        host.open_document(
            id.clone(),
            1,
            "l_english:\nfoo_name:0 \"Foo\"\nbar:0 \"\"\n".to_owned(),
            None,
        )
        .expect("open");
        let snapshot = host.snapshot();
        let result = complete(&snapshot, &id, 36);
        assert!(
            result
                .items
                .iter()
                .any(|item| item.label == "foo_name" && item.kind == CompletionKind::Localisation)
        );
    }

    #[test]
    fn localisation_hover_shows_the_resolved_short_text() {
        let mut host = eu4_host(pdx_game::eu4::bootstrap_rules());
        let id = DocumentId::new("file:///tmp/localisation/test.yml");
        let text = "l_english:\nfoo_name:0 \"Foo\"\n";
        host.open_document(id.clone(), 1, text.to_owned(), None)
            .expect("open localisation");
        let position =
            u32::try_from(text.find("foo_name").expect("localisation key") + 2).expect("position");
        let hover = hover(&host.snapshot(), &id, position).expect("localisation hover");
        assert!(
            hover
                .contents
                .contains("#### Localisation preview\n\n- Localisation")
        );
        assert!(hover.contents.contains("Localisation (l_english): \"Foo\""));
    }

    #[test]
    fn required_type_localisation_keys_report_missing_derived_keys() {
        let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
        host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
            SourceRootId::new(1),
            SourceRootKind::CurrentMod,
            std::path::PathBuf::from("/tmp"),
        )]));
        let id = DocumentId::new("file:///tmp/missions/test.txt");
        host.open_document(
            id.clone(),
            1,
            "series = { mission_one = { potential = { always = yes } } }\n".to_owned(),
            Some(std::path::PathBuf::from("/tmp/missions/test.txt")),
        )
        .expect("open mission");

        let messages = diagnostics(&host.snapshot(), &id)
            .into_iter()
            .filter(|diagnostic| diagnostic.code == DiagnosticCode::UnknownSymbol)
            .map(|diagnostic| diagnostic.message)
            .collect::<Vec<_>>();
        assert!(
            messages
                .iter()
                .any(|message| message.contains("mission_one_title"))
        );
        assert!(
            messages
                .iter()
                .any(|message| message.contains("mission_one_desc"))
        );
    }

    #[test]
    fn mission_metadata_fields_do_not_derive_localisation_keys() {
        let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
        host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
            SourceRootId::new(1),
            SourceRootKind::CurrentMod,
            std::path::PathBuf::from("/tmp"),
        )]));
        let id = DocumentId::new("file:///tmp/missions/metadata.txt");
        host.open_document(
            id.clone(),
            1,
            "series = { slot = 1 generic = no ai = yes has_country_shield = yes mission_one = { potential = { always = yes } } }\n"
                .to_owned(),
            Some(std::path::PathBuf::from("/tmp/missions/metadata.txt")),
        )
        .expect("open mission");

        let messages = diagnostics(&host.snapshot(), &id)
            .into_iter()
            .filter(|diagnostic| diagnostic.code == DiagnosticCode::UnknownSymbol)
            .map(|diagnostic| diagnostic.message)
            .collect::<Vec<_>>();
        for key in [
            "slot_title",
            "slot_desc",
            "generic_title",
            "generic_desc",
            "ai_title",
            "ai_desc",
            "has_country_shield_title",
            "has_country_shield_desc",
        ] {
            assert!(
                !messages
                    .iter()
                    .any(|message| message.contains(&format!("`{key}`"))),
                "metadata field {key} must not derive a localisation key: {messages:?}"
            );
        }
        assert!(
            messages
                .iter()
                .any(|message| message.contains("mission_one_title")),
            "the nested mission still derives its title key: {messages:?}"
        );
        assert!(
            messages
                .iter()
                .any(|message| message.contains("mission_one_desc")),
            "the nested mission still derives its desc key: {messages:?}"
        );
    }

    #[test]
    fn hover_prefers_nonempty_localisation_preview_over_empty_sibling() {
        let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
        host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
            SourceRootId::new(1),
            SourceRootKind::CurrentMod,
            std::path::PathBuf::from("/tmp"),
        )]));
        let localisation = DocumentId::new("file:///tmp/localisation/test.yml");
        host.open_document(
            localisation.clone(),
            1,
            "l_english:\nmission_one_title:0 \"Mission One Title\"\nmission_one_desc:0 \"\"\n"
                .to_owned(),
            Some(std::path::PathBuf::from("/tmp/localisation/test.yml")),
        )
        .expect("open localisation");
        let mission = DocumentId::new("file:///tmp/missions/test.txt");
        let source = "series = { mission_one = { potential = { always = yes } } }\n";
        host.open_document(
            mission.clone(),
            1,
            source.to_owned(),
            Some(std::path::PathBuf::from("/tmp/missions/test.txt")),
        )
        .expect("open mission");

        let position =
            u32::try_from(source.find("mission_one").expect("mission name") + 4).expect("position");
        let hover = hover(&host.snapshot(), &mission, position).expect("mission hover");
        assert!(
            hover
                .contents
                .contains("Localisation (l_english): \"Mission One Title\""),
            "hover should prefer the non-empty title preview: {}",
            hover.contents
        );
        assert!(!hover.contents.contains("mission_one_desc"));
    }

    #[test]
    fn custom_tooltip_hover_shows_localisation_preview_inside_mission_effects() {
        let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
        host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
            SourceRootId::new(1),
            SourceRootKind::CurrentMod,
            std::path::PathBuf::from("/tmp"),
        )]));
        let localisation = DocumentId::new("file:///tmp/localisation/test.yml");
        host.open_document(
            localisation.clone(),
            1,
            "l_english:\nEDG_TEST_TT:0 \"My tooltip text\"\n".to_owned(),
            Some(std::path::PathBuf::from("/tmp/localisation/test.yml")),
        )
        .expect("open localisation");
        let mission = DocumentId::new("file:///tmp/missions/test.txt");
        let source = "series = { mission_one = { effect = { custom_tooltip = EDG_TEST_TT } } }\n";
        host.open_document(
            mission.clone(),
            1,
            source.to_owned(),
            Some(std::path::PathBuf::from("/tmp/missions/test.txt")),
        )
        .expect("open mission");

        let position =
            u32::try_from(source.find("EDG_TEST_TT").expect("tooltip key") + 4).expect("position");
        let hover = hover(&host.snapshot(), &mission, position).expect("tooltip hover");
        assert!(
            hover
                .contents
                .contains("Localisation (l_english): \"My tooltip text\""),
            "custom_tooltip inside a mission effect should resolve to the localisation preview: {}",
            hover.contents
        );
    }

    #[test]
    fn vanilla_cache_localisation_hover_shows_derived_text_without_source_state() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("pdx-analysis-vanilla-hover-{nonce}"));
        let vanilla = root.join("vanilla");
        let current = root.join("current");
        std::fs::create_dir_all(vanilla.join("localisation/nested")).expect("Vanilla directory");
        std::fs::create_dir_all(current.join("events")).expect("current directory");
        std::fs::write(
            vanilla.join("localisation/nested/test_l_english.yml"),
            "l_english:\ncached_name:0 \"Cached Vanilla text\"\n",
        )
        .expect("Vanilla localisation");

        let mut vanilla_host = eu4_host(pdx_game::eu4::bootstrap_rules());
        vanilla_host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
            SourceRootId::new(0),
            SourceRootKind::Vanilla,
            vanilla.clone(),
        )]));
        vanilla_host
            .refresh_source_roots()
            .expect("scan Vanilla for cache");
        let cache = VanillaIndexCache::from_snapshot(&vanilla_host.snapshot())
            .expect("build Vanilla cache");
        let localisation_file = cache
            .index()
            .active_definition("localisation", "cached_name")
            .expect("cached localisation definition")
            .file_id;

        let mut host = eu4_host(pdx_game::eu4::bootstrap_rules());
        host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
            SourceRootId::new(1),
            SourceRootKind::CurrentMod,
            current.clone(),
        )]));
        host.install_vanilla_cache(cache)
            .expect("install Vanilla cache");
        let document = DocumentId::new("file:///current/events/hover.txt");
        let text = "country_event = { title = cached_name }\n";
        host.open_document(
            document.clone(),
            1,
            text.to_owned(),
            Some(current.join("events/hover.txt")),
        )
        .expect("open current script");
        let position = u32::try_from(text.find("cached_name").expect("localisation reference"))
            .expect("position");
        let hover =
            hover(&host.snapshot(), &document, position).expect("cached localisation hover");
        assert!(
            hover
                .contents
                .contains("Localisation (l_english): \"Cached Vanilla text\"")
        );
        assert!(host.snapshot().file_state(localisation_file).is_none());
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn navigation_and_hover_use_local_event_definition() {
        let text = "country_event = { id = test.1 }\nevent = test.1\n";
        let (host, id) = snapshot(text);
        let snapshot = host.snapshot();
        let symbols = document_symbols(&snapshot, &id);
        assert_eq!(symbols.len(), 1);
        let definition_location = definition(&snapshot, &id, 40);
        assert_eq!(definition_location.len(), 1);
        let definition_name_start =
            u32::try_from(text.find("test.1").expect("definition name")).expect("offset");
        assert_eq!(
            definition_location[0].range,
            TextRange::new(definition_name_start, definition_name_start + 6)
                .expect("definition name range")
        );
        assert!(hover(&snapshot, &id, 40).is_some());
        let references = references(&snapshot, &id, 40, true);
        assert_eq!(references.len(), 2);
        assert!(references.iter().any(|location| {
            location.range
                == TextRange::new(definition_name_start, definition_name_start + 6)
                    .expect("definition name range")
        }));
        assert!(!workspace_symbols(&snapshot, "test").is_empty());
        assert!(TextRange::new(0, 1).is_some());
    }

    #[test]
    fn navigation_targets_the_name_in_an_indexed_definition() {
        use pdx_engine::{SourceRoot, SourceRootId, SourceRootKind, WorkspaceChange};
        use std::fs;

        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("pdx-analysis-navigation-{nonce}"));
        let definitions = root.join("common/events");
        fs::create_dir_all(&definitions).expect("event directory");
        let definition_path = definitions.join("definitions.txt");
        let definition_text = "country_event = { id = indexed.1 }\n";
        fs::write(&definition_path, definition_text).expect("event definition");

        let mut host = eu4_host(pdx_game::eu4::bootstrap_rules());
        host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
            SourceRootId::new(1),
            SourceRootKind::CurrentMod,
            root.clone(),
        )]));
        host.refresh_source_roots().expect("scan event definition");

        let id = DocumentId::new("file:///tmp/events/use.txt");
        let use_text = "event = indexed.1\n";
        let position =
            u32::try_from(use_text.find("indexed.1").expect("event reference")).expect("offset");
        host.open_document(id.clone(), 1, use_text.to_owned(), None)
            .expect("open event reference");

        let location = definition(&host.snapshot(), &id, position)
            .into_iter()
            .next()
            .expect("indexed definition location");
        let name_start = u32::try_from(definition_text.find("indexed.1").expect("definition name"))
            .expect("offset");
        assert_eq!(
            location.range,
            TextRange::new(name_start, name_start + 9).expect("definition name range")
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn rename_updates_definition_and_resolved_references() {
        let text = "country_event = { id = test.1 }\nevent = test.1\n";
        let (mut host, id) = snapshot(text);
        let position = u32::try_from(text.rfind("test.1").expect("reference")).expect("offset");
        let prepared = prepare_rename(&host.snapshot(), &id, position).expect("prepare rename");
        assert_eq!(prepared.placeholder, "test.1");
        assert_eq!(prepared.range.len(), 6);

        let plan = rename(&host.snapshot(), &id, position, "renamed.1").expect("rename");
        assert_eq!(plan.edits.len(), 2);
        assert!(plan.edits[0].location.range.start() > plan.edits[1].location.range.start());
        let mut changed = text.to_owned();
        for edit in &plan.edits {
            let start = usize::try_from(edit.location.range.start()).expect("start");
            let end = usize::try_from(edit.location.range.end()).expect("end");
            changed.replace_range(start..end, &edit.new_text);
        }
        host.apply_document_changes(&id, 2, &[pdx_engine::TextChange::full(changed)])
            .expect("apply rename");
        assert!(diagnostics(&host.snapshot(), &id).iter().all(|item| {
            item.code != DiagnosticCode::UnknownSymbol
                && item.code != DiagnosticCode::AmbiguousSymbol
        }));
    }

    #[test]
    fn rename_rejects_invalid_names_ambiguous_symbols_and_conflicts() {
        let (host, id) = snapshot(
            "country_event = { id = old.1 }\ncountry_event = { id = other.1 }\nevent = old.1\n",
        );
        let current_snapshot = host.snapshot();
        let old_position = u32::try_from("country_event = { id = ".len()).expect("offset");
        assert_eq!(
            rename(&current_snapshot, &id, old_position, "not a name").expect_err("invalid name"),
            RenameError::InvalidName
        );
        assert_eq!(
            rename(&current_snapshot, &id, old_position, "other.1").expect_err("conflict"),
            RenameError::Conflict
        );

        let ambiguous_text = "country_event = { id = duplicate.1 }\ncountry_event = { id = duplicate.1 }\nevent = duplicate.1\n";
        let (ambiguous_host, ambiguous_id) = snapshot(ambiguous_text);
        let reference =
            u32::try_from(ambiguous_text.rfind("duplicate.1").expect("reference")).expect("offset");
        assert_eq!(
            prepare_rename(&ambiguous_host.snapshot(), &ambiguous_id, reference)
                .expect_err("ambiguous symbol"),
            RenameError::Ambiguous
        );
    }

    #[test]
    fn rename_rejects_dependency_and_vanilla_definitions() {
        use pdx_engine::{SourceRoot, SourceRootId, SourceRootKind, WorkspaceChange};
        use std::fs;

        let nonce = std::process::id();
        let root = std::env::temp_dir().join(format!("pdx-analysis-rename-{nonce}"));
        let dependency = root.join("dependency/common/events");
        fs::create_dir_all(&dependency).expect("dependency directory");
        let path = dependency.join("events.txt");
        fs::write(&path, "country_event = { id = read_only.1 }\n").expect("dependency event");

        let mut host = eu4_host(pdx_game::eu4::bootstrap_rules());
        host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot {
            id: SourceRootId::new(1),
            kind: SourceRootKind::Dependency,
            path: root.join("dependency"),
            order: 0,
            writable: false,
        }]));
        host.refresh_source_roots().expect("scan dependency");
        let id = DocumentId::new("file:///dependency/events.txt");
        let text = "country_event = { id = read_only.1 }\n";
        host.open_document(id.clone(), 1, text.to_owned(), Some(path.clone()))
            .expect("open dependency overlay");
        let position =
            u32::try_from(text.find("read_only.1").expect("definition")).expect("offset");
        assert_eq!(
            prepare_rename(&host.snapshot(), &id, position).expect_err("read-only definition"),
            RenameError::ReadOnly
        );
        fs::remove_dir_all(root).expect("cleanup");
    }
}
