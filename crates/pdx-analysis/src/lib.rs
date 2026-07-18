//! Editor-neutral diagnostics and language-feature queries.
//!
//! The analysis crate owns all semantic decisions.  `pdx-lsp` only converts the DTOs in this
//! module to protocol values, which keeps the same behaviour available to the CLI and tests.

use std::collections::BTreeSet;
use std::path::Path;

use pdx_eu4::{
    CsvDialect as RuleCsvDialect, CwtKeyMatcher, CwtRuleShape, CwtValueMatcher, ParserKind,
    SymbolResolutionPolicy,
};
use pdx_hir::Scope;
use pdx_syntax::{
    CstKind, CstNode, CsvParsedFile, Eu4FileFormat, ParsedFile, SyntaxError, csv::CsvDialect,
    parse_eu4, parse_eu4_csv_file,
};
use pdx_text::{LogicalPath, TextRange, TextSize};
use pdx_workspace::{AnalysisSnapshot, Definition, DocumentId, DocumentSource, SourceFileId};

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
    /// A scalar or block does not satisfy the selected CWT matcher.
    InvalidValue,
    /// A CWT cardinality constraint was violated.
    Cardinality,
    /// A key or value is known to CWT but is used from the wrong EU4 scope.
    WrongScope,
}

impl DiagnosticCode {
    /// Returns the stable wire-facing code.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Syntax => "pdx-syntax",
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
    /// The requested replacement is not a single EU4 identifier token.
    InvalidName,
    /// The replacement would create a same-priority or otherwise disallowed definition conflict.
    Conflict,
}

impl std::fmt::Display for RenameError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::NoSymbol => "cursor is not on a renameable symbol",
            Self::Unresolved => "symbol has no unique definition",
            Self::Ambiguous => "symbol has multiple definitions",
            Self::ReadOnly => "symbol is defined in a read-only source",
            Self::InvalidName => "new name is not a valid EU4 identifier",
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
    pub format: Option<Eu4FileFormat>,
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
        (diagnostic.range.start(), diagnostic.range.end(), diagnostic.code)
    });
    AnalysisResult { revision: snapshot.revision(), scope: Scope::Unknown, diagnostics }
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
    analyze_document(snapshot, document).map_or_else(Vec::new, |analysis| analysis.diagnostics)
}

/// Computes key, value, localisation, and symbol completion.
#[must_use]
pub fn complete(
    snapshot: &AnalysisSnapshot,
    document: &DocumentId,
    position: TextSize,
) -> CompletionResult {
    let Some(input) = input_for_document(snapshot, document) else {
        return CompletionResult { revision: snapshot.revision(), items: Vec::new() };
    };
    let replacement_range = word_range(&input.source, position);
    let prefix = input.source_text(replacement_range).unwrap_or_default().to_owned();
    let value_context = completion_value_context(&input, position);
    let all = all_semantics(snapshot);
    let mut items = Vec::new();
    let cwt_context = cwt_completion_context(snapshot, &input, position);
    if let Some(context) = cwt_context.as_ref() {
        if value_context {
            if let Some(property) = context.property.as_ref() {
                add_cwt_value_items(
                    snapshot,
                    context,
                    property,
                    &mut items,
                    replacement_range,
                    &prefix,
                );
            }
        } else {
            add_cwt_key_items(snapshot, context, &mut items, replacement_range, &prefix);
        }
    }
    if items.is_empty() && value_context {
        add_scalar_items(&mut items, replacement_range, &prefix);
        for definition in &all.definitions {
            let kind = if definition.kind == "localisation" {
                CompletionKind::Localisation
            } else {
                CompletionKind::Symbol
            };
            let detail = format!("{} symbol", definition.kind);
            push_completion(
                &mut items,
                CompletionItem {
                    label: definition.name.clone(),
                    kind,
                    detail,
                    documentation: None,
                    replacement_range,
                    insert_text: definition.name.clone(),
                    sort_score: if definition.kind == "localisation" { 20 } else { 30 },
                    deprecated: false,
                },
                &prefix,
            );
        }
    } else if items.is_empty() {
        for key in known_keys(snapshot) {
            push_completion(
                &mut items,
                CompletionItem {
                    label: key.clone(),
                    kind: CompletionKind::Key,
                    detail: "EU4 property".to_owned(),
                    documentation: None,
                    replacement_range,
                    insert_text: key,
                    sort_score: 10,
                    deprecated: false,
                },
                &prefix,
            );
        }
        for definition in &all.definitions {
            if matches!(definition.kind.as_str(), "scripted_effect" | "scripted_trigger") {
                push_completion(
                    &mut items,
                    CompletionItem {
                        label: definition.name.clone(),
                        kind: CompletionKind::Symbol,
                        detail: format!("{} command", definition.kind),
                        documentation: None,
                        replacement_range,
                        insert_text: format!("{} = {{\n    $0\n}}", definition.name),
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
                items.push(CompletionItem {
                    label: key.clone(),
                    kind: CompletionKind::Key,
                    detail: "EU4 property".to_owned(),
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
    CompletionResult { revision: snapshot.revision(), items }
}

#[derive(Clone, Debug)]
struct CwtCompletionContext {
    context: String,
    parent_path: Vec<String>,
    scope: Eu4ScopeContext,
    property: Option<ScriptProperty>,
}

fn cwt_completion_context(
    snapshot: &AnalysisSnapshot,
    input: &ParsedInput,
    position: TextSize,
) -> Option<CwtCompletionContext> {
    let ParsedContent::Text(parsed) = &input.parsed else { return None };
    for root in script_properties(input, parsed.root()) {
        let Some(context) = cwt_root_context(snapshot, &root.key, input.path.as_ref()) else {
            continue;
        };
        let Some(block_range) = root.block_range else { continue };
        if !contains(block_range, position) {
            continue;
        }
        let scope = cwt_initial_scope(snapshot, &context, &root.key);
        return Some(cwt_completion_container(
            snapshot,
            context,
            Vec::new(),
            root.block,
            root.bare_values,
            scope,
            position,
        ));
    }
    None
}

fn cwt_completion_container(
    snapshot: &AnalysisSnapshot,
    context: String,
    parent_path: Vec<String>,
    properties: Vec<ScriptProperty>,
    _bare_values: Vec<(String, TextRange)>,
    scope: Eu4ScopeContext,
    position: TextSize,
) -> CwtCompletionContext {
    for property in &properties {
        let Some(block_range) = property.block_range else { continue };
        if !contains(block_range, position) {
            continue;
        }
        let next_rule = cwt_rules_for_container(snapshot, &context, &parent_path, &scope)
            .into_iter()
            .find(|rule| {
                !matches!(rule.shape, CwtRuleShape::LeafValue)
                    && cwt_key_matches(snapshot, &rule.key, &property.key)
                    && cwt_scope_allows(rule, &scope)
            });
        let (next_context, child_path) =
            next_rule.and_then(|rule| rule.child_context.as_deref()).map_or_else(
                || {
                    let mut path = parent_path.clone();
                    path.push(property.key.clone());
                    (context.clone(), path)
                },
                |child_context| (child_context.to_owned(), Vec::new()),
            );
        let next_scope =
            next_rule.map_or_else(|| scope.clone(), |rule| cwt_child_scope(&scope, rule));
        return cwt_completion_container(
            snapshot,
            next_context,
            child_path,
            property.block.clone(),
            property.bare_values.clone(),
            next_scope,
            position,
        );
    }
    let property = properties.into_iter().find(|property| contains(property.range, position));
    CwtCompletionContext { context, parent_path, scope, property }
}

fn cwt_rules_for_container<'a>(
    snapshot: &'a AnalysisSnapshot,
    context: &str,
    parent_path: &[String],
    _scope: &Eu4ScopeContext,
) -> Vec<&'a pdx_eu4::CwtSemanticRule> {
    snapshot
        .rules()
        .model()
        .cwt
        .rules
        .iter()
        .filter(|rule| {
            let context_matches = rule.context.eq_ignore_ascii_case(context)
                || (context.strip_prefix("type:").is_some_and(|type_name| {
                    rule.context.eq_ignore_ascii_case(&format!("root:{type_name}"))
                }));
            context_matches && cwt_parent_path_matches(snapshot, &rule.parent_path, parent_path)
        })
        .collect()
}

fn add_cwt_key_items(
    snapshot: &AnalysisSnapshot,
    context: &CwtCompletionContext,
    items: &mut Vec<CompletionItem>,
    replacement_range: TextRange,
    prefix: &str,
) {
    for rule in
        cwt_rules_for_container(snapshot, &context.context, &context.parent_path, &context.scope)
    {
        if matches!(rule.shape, CwtRuleShape::LeafValue) || !cwt_scope_allows(rule, &context.scope)
        {
            continue;
        }
        let documentation = (!rule.documentation.is_empty()).then(|| rule.documentation.join("\n"));
        match &rule.key {
            CwtKeyMatcher::Exact(label) => push_completion(
                items,
                CompletionItem {
                    label: label.clone(),
                    kind: CompletionKind::Key,
                    detail: cwt_rule_detail(rule),
                    documentation,
                    replacement_range,
                    insert_text: label.clone(),
                    sort_score: if rule.required { 2 } else { 5 },
                    deprecated: false,
                },
                prefix,
            ),
            CwtKeyMatcher::Type(type_name) => {
                for label in workspace_member_names(snapshot, type_name) {
                    push_completion(
                        items,
                        CompletionItem {
                            label: label.clone(),
                            kind: CompletionKind::Key,
                            detail: format!("CWT type key <{type_name}>"),
                            documentation: documentation.clone(),
                            replacement_range,
                            insert_text: label,
                            sort_score: 8,
                            deprecated: false,
                        },
                        prefix,
                    );
                }
            }
            CwtKeyMatcher::Enum(enum_name) => {
                for label in enum_member_names(snapshot, enum_name) {
                    push_completion(
                        items,
                        CompletionItem {
                            label: label.clone(),
                            kind: CompletionKind::Key,
                            detail: format!("CWT enum key enum[{enum_name}]"),
                            documentation: documentation.clone(),
                            replacement_range,
                            insert_text: label,
                            sort_score: 8,
                            deprecated: false,
                        },
                        prefix,
                    );
                }
            }
            CwtKeyMatcher::Dynamic(kind) => {
                for label in workspace_member_names(snapshot, kind) {
                    push_completion(
                        items,
                        CompletionItem {
                            label: label.clone(),
                            kind: CompletionKind::Key,
                            detail: format!("CWT dynamic key value_set[{kind}]"),
                            documentation: documentation.clone(),
                            replacement_range,
                            insert_text: label,
                            sort_score: 8,
                            deprecated: false,
                        },
                        prefix,
                    );
                }
            }
            CwtKeyMatcher::AnyScalar => {}
        }
    }
}

fn add_cwt_value_items(
    snapshot: &AnalysisSnapshot,
    context: &CwtCompletionContext,
    property: &ScriptProperty,
    items: &mut Vec<CompletionItem>,
    replacement_range: TextRange,
    prefix: &str,
) {
    let matching =
        cwt_rules_for_container(snapshot, &context.context, &context.parent_path, &context.scope)
            .into_iter()
            .filter(|rule| {
                !matches!(rule.shape, CwtRuleShape::LeafValue)
                    && cwt_key_matches(snapshot, &rule.key, &property.key)
                    && rule
                        .operator
                        .as_deref()
                        .is_none_or(|operator| property.operator.as_deref() == Some(operator))
            })
            .filter(|rule| cwt_scope_allows(rule, &context.scope))
            .collect::<Vec<_>>();
    for rule in matching {
        let documentation = (!rule.documentation.is_empty()).then(|| rule.documentation.join("\n"));
        match &rule.value {
            CwtValueMatcher::Exact(label) => add_value_completion(
                items,
                label,
                &cwt_value_matcher_label(&rule.value),
                documentation.clone(),
                replacement_range,
                prefix,
            ),
            CwtValueMatcher::Bool => {
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
            CwtValueMatcher::Int { min, max } => {
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
            CwtValueMatcher::Float { min, max } => {
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
            CwtValueMatcher::Type(type_name) => {
                for label in workspace_member_names(snapshot, type_name) {
                    add_value_completion(
                        items,
                        &label,
                        &format!("<{type_name}>"),
                        documentation.clone(),
                        replacement_range,
                        prefix,
                    );
                }
            }
            CwtValueMatcher::Enum(enum_name) => {
                for label in enum_member_names(snapshot, enum_name) {
                    add_value_completion(
                        items,
                        &label,
                        &format!("enum[{enum_name}]"),
                        documentation.clone(),
                        replacement_range,
                        prefix,
                    );
                }
            }
            CwtValueMatcher::Scope(expected) => {
                for label in ["root", "this", "from", "prev", "country", "province", "trade_node"] {
                    if expected.as_deref().is_none_or(|scope| scope_compatible(label, scope)) {
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
            CwtValueMatcher::Localisation => {
                for label in workspace_member_names(snapshot, "localisation") {
                    add_value_completion(
                        items,
                        &label,
                        "localisation",
                        documentation.clone(),
                        replacement_range,
                        prefix,
                    );
                }
            }
            CwtValueMatcher::Dynamic(kind) => {
                for label in workspace_member_names(snapshot, kind) {
                    add_value_completion(
                        items,
                        &label,
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
            CwtValueMatcher::DynamicSet(_)
            | CwtValueMatcher::AnyScalar
            | CwtValueMatcher::Filepath
            | CwtValueMatcher::Opaque(_) => {}
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
        add_value_completion(items, label, detail, documentation, replacement_range, prefix);
    }
}

fn cwt_rule_detail(rule: &pdx_eu4::CwtSemanticRule) -> String {
    let shape = match rule.shape {
        CwtRuleShape::Node => "block",
        CwtRuleShape::Leaf => "scalar",
        CwtRuleShape::LeafValue => "bare value",
        CwtRuleShape::ValueClause => "value clause",
    };
    format!("CWT {shape}")
}

fn workspace_member_names(snapshot: &AnalysisSnapshot, type_name: &str) -> Vec<String> {
    let base = type_name.split_once('.').map_or(type_name, |(kind, _)| kind);
    let alias = eu4_member_kind_alias(base);
    let mut names = snapshot
        .index()
        .definitions_iter()
        .filter(|definition| {
            definition.kind.eq_ignore_ascii_case(type_name)
                || definition.kind.eq_ignore_ascii_case(base)
                || alias.is_some_and(|alias| definition.kind.eq_ignore_ascii_case(alias))
        })
        .map(|definition| definition.name.clone())
        .collect::<Vec<_>>();
    names.sort_by_key(|name| name.to_ascii_lowercase());
    names.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
    names
}

fn enum_member_names(snapshot: &AnalysisSnapshot, enum_name: &str) -> Vec<String> {
    let mut names =
        snapshot.rules().model().cwt.enum_values.get(enum_name).cloned().unwrap_or_default();
    names.extend(workspace_member_names(snapshot, enum_name));
    names.sort_by_key(|name| name.to_ascii_lowercase());
    names.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
    names
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
    let input = input_for_document(snapshot, document)?;
    let range = word_range(&input.source, position);
    let word = input.source_text(range)?.trim_matches('"').to_owned();
    if word.is_empty() {
        return None;
    }
    let all = all_semantics(snapshot);
    if let Some(reference) = all.references.iter().find(|reference| {
        reference.document.as_ref() == Some(document) && contains(reference.range, position)
    }) {
        return Some(hover_for_symbol(snapshot, &all, &reference.kind, &reference.name, range));
    }
    if let Some(definition) = all.definitions.iter().find(|definition| {
        definition.document.as_ref() == Some(document)
            && contains(definition.symbol.selection_range, position)
    }) {
        return Some(hover_for_symbol(snapshot, &all, &definition.kind, &definition.name, range));
    }
    if let Some(details) = cwt_rule_documentation_at(snapshot, &input, position) {
        return Some(Hover {
            contents: format!("EU4 property `{word}`\n\n{details}"),
            range: Some(range),
        });
    }
    let known = known_keys(snapshot);
    if known.contains(&word) {
        let contents = cwt_rule_documentation(snapshot, &word).map_or_else(
            || format!("EU4 property `{word}`"),
            |details| format!("EU4 property `{word}`\n\n{details}"),
        );
        return Some(Hover { contents, range: Some(range) });
    }
    Some(Hover { contents: format!("EU4 value `{word}`"), range: Some(range) })
}

fn cwt_rule_documentation(snapshot: &AnalysisSnapshot, key: &str) -> Option<String> {
    let mut rules = snapshot
        .rules()
        .model()
        .cwt
        .rules
        .iter()
        .filter(|rule| match &rule.key {
            CwtKeyMatcher::Exact(expected) => expected.eq_ignore_ascii_case(key),
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
    cwt_rule_documentation_for_rule(rule)
}

fn cwt_rule_documentation_at(
    snapshot: &AnalysisSnapshot,
    input: &ParsedInput,
    position: TextSize,
) -> Option<String> {
    let context = cwt_completion_context(snapshot, input, position)?;
    let property = context.property.as_ref()?;
    let mut rules =
        cwt_rules_for_container(snapshot, &context.context, &context.parent_path, &context.scope)
            .into_iter()
            .filter(|rule| {
                !matches!(rule.shape, CwtRuleShape::LeafValue)
                    && cwt_key_matches(snapshot, &rule.key, &property.key)
                    && cwt_scope_allows(rule, &context.scope)
            })
            .collect::<Vec<_>>();
    rules.sort_by_key(|rule| (&rule.context, &rule.parent_path, &rule.id));
    rules.into_iter().find_map(cwt_rule_documentation_for_rule)
}

fn cwt_rule_documentation_for_rule(rule: &pdx_eu4::CwtSemanticRule) -> Option<String> {
    let mut lines = rule.documentation.clone();
    if rule.required {
        lines.push("required".to_owned());
    }
    if let Some(min) = rule.min_occurs {
        lines.push(format!("minimum occurrences: {min}"));
    }
    if let Some(max) = rule.max_occurs {
        lines.push(format!("maximum occurrences: {max}"));
    }
    if !rule.allowed_scopes.is_empty() {
        lines.push(format!("scopes: {}", rule.allowed_scopes.join(", ")));
    }
    (!lines.is_empty()).then(|| lines.join("\n"))
}

/// Resolves the symbol at a position. Ambiguous and unresolved references deliberately return no
/// location so a client can never be sent to an arbitrary candidate.
#[must_use]
pub fn definition(
    snapshot: &AnalysisSnapshot,
    document: &DocumentId,
    position: TextSize,
) -> Vec<Location> {
    if input_for_document(snapshot, document).is_none() {
        return Vec::new();
    }
    let all = all_semantics(snapshot);
    let Some((kind, name)) = symbol_at(&all, document, position) else {
        return Vec::new();
    };
    match resolve_symbol(snapshot, &all, &kind, &name) {
        Resolution::Unique(definition) => vec![definition.location],
        Resolution::Ambiguous | Resolution::Missing => Vec::new(),
    }
}

/// Returns resolved references for the symbol at a position.
#[must_use]
pub fn references(
    snapshot: &AnalysisSnapshot,
    document: &DocumentId,
    position: TextSize,
    include_declaration: bool,
) -> Vec<Location> {
    let Some(input) = input_for_document(snapshot, document) else {
        return Vec::new();
    };
    let _ = input;
    let all = all_semantics(snapshot);
    let Some((kind, name)) = symbol_at(&all, document, position) else {
        return Vec::new();
    };
    let Resolution::Unique(target) = resolve_symbol(snapshot, &all, &kind, &name) else {
        return Vec::new();
    };
    let mut result = Vec::new();
    if include_declaration {
        result.push(target.location.clone());
    }
    for reference in &all.references {
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
            location.path.as_ref().map_or(String::new(), |path| path.as_str().to_owned()),
            location.range.start(),
        )
    });
    result.dedup();
    result
}

/// Returns the identifier range when the cursor is on a uniquely resolved, writable symbol.
pub fn prepare_rename(
    snapshot: &AnalysisSnapshot,
    document: &DocumentId,
    position: TextSize,
) -> Result<PrepareRenameResult, RenameError> {
    let target = rename_target(snapshot, document, position)?;
    let input = input_for_document(snapshot, document).ok_or(RenameError::NoSymbol)?;
    let placeholder =
        input.source_text(target.cursor_range).ok_or(RenameError::NoSymbol)?.to_owned();
    Ok(PrepareRenameResult { range: target.cursor_range, placeholder })
}

/// Builds a safe, editor-neutral WorkspaceEdit for a semantic rename.
pub fn rename(
    snapshot: &AnalysisSnapshot,
    document: &DocumentId,
    position: TextSize,
    new_name: &str,
) -> Result<WorkspaceEditPlan, RenameError> {
    if !valid_rename_name(new_name) {
        return Err(RenameError::InvalidName);
    }
    let target = rename_target(snapshot, document, position)?;
    let all = all_semantics(snapshot);
    check_rename_conflict(snapshot, &all, &target, new_name)?;

    let mut edits = vec![WorkspaceTextEdit {
        location: Location {
            range: target.definition.selection_range,
            ..target.definition.location.clone()
        },
        new_text: new_name.to_owned(),
    }];
    let overlay_files = overlay_file_ids(snapshot);
    for reference in &all.references {
        if reference.kind != target.kind || !same_name(&reference.name, &target.name) {
            continue;
        }
        // A document overlay replaces its disk candidate.  Do not return edits for the hidden
        // disk text as that would overwrite user changes when the client applies the WorkspaceEdit.
        if reference.document.is_none()
            && reference.file.is_some_and(|file| overlay_files.contains(&file))
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
            .then_with(|| right.location.range.start().cmp(&left.location.range.start()))
            .then_with(|| right.location.range.end().cmp(&left.location.range.end()))
    });
    edits
        .dedup_by(|left, right| left.location == right.location && left.new_text == right.new_text);
    Ok(WorkspaceEditPlan { revision: snapshot.revision(), edits })
}

/// Returns symbols declared by one document.
#[must_use]
pub fn document_symbols(snapshot: &AnalysisSnapshot, document: &DocumentId) -> Vec<Symbol> {
    let Some(input) = input_for_document(snapshot, document) else {
        return Vec::new();
    };
    let data = semantic_data(&input);
    data.definitions.into_iter().map(|definition| definition.symbol).collect()
}

/// Returns active workspace symbols using deterministic prefix/fuzzy ranking.
#[must_use]
pub fn workspace_symbols(snapshot: &AnalysisSnapshot, query: &str) -> Vec<WorkspaceSymbol> {
    let all = all_semantics(snapshot);
    let query = query.trim().to_ascii_lowercase();
    let mut result = Vec::new();
    for definition in &all.definitions {
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
        (*score, symbol.name.to_ascii_lowercase(), symbol.kind.clone())
    });
    result.into_iter().map(|(_, symbol)| symbol).collect()
}

#[derive(Clone, Debug)]
struct ParsedInput {
    document: Option<DocumentId>,
    file: Option<SourceFileId>,
    path: Option<LogicalPath>,
    format: Eu4FileFormat,
    source: String,
    parsed: ParsedContent,
}

#[derive(Clone, Debug)]
enum ParsedContent {
    Text(ParsedFile),
    Csv(CsvParsedFile),
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
    key_range: TextRange,
    value: Option<(String, TextRange)>,
    top_level: bool,
    path: Vec<String>,
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
    let path = document.path().and_then(|path| logical_path(snapshot, path)).or_else(|| {
        id.as_str()
            .split(['/', '\\'])
            .next_back()
            .filter(|name| name.contains('.'))
            .and_then(|name| LogicalPath::parse(name).ok())
    });
    let file = document
        .path()
        .and_then(|path| snapshot.source_files().values().find(|file| file.physical_path == path))
        .map(|file| file.id);
    let format = parser_for(snapshot, path.as_ref(), document.path());
    let source = document.text().to_owned();
    parse_input(Some(id.clone()), file, path, format?, source)
}

fn input_for_source_file(snapshot: &AnalysisSnapshot, id: SourceFileId) -> Option<ParsedInput> {
    let file = snapshot.source_files().get(&id)?;
    let source = snapshot.source_text(id)?.to_owned();
    let parser = snapshot
        .rules()
        .classify(&file.logical_path)
        .map(|category| category.parser.clone())
        .or_else(|| parser_for(snapshot, Some(&file.logical_path), Some(&file.physical_path)))?;
    parse_input(None, Some(id), Some(file.logical_path.clone()), parser, source)
}

fn parse_input(
    document: Option<DocumentId>,
    file: Option<SourceFileId>,
    path: Option<LogicalPath>,
    parser: ParserKind,
    source: String,
) -> Option<ParsedInput> {
    match parser {
        ParserKind::PdxScript => Some(ParsedInput {
            document,
            file,
            path,
            format: Eu4FileFormat::PdxScript,
            parsed: ParsedContent::Text(parse_eu4(Eu4FileFormat::PdxScript, &source)),
            source,
        }),
        ParserKind::Localisation => Some(ParsedInput {
            document,
            file,
            path,
            format: Eu4FileFormat::Localisation,
            parsed: ParsedContent::Text(parse_eu4(Eu4FileFormat::Localisation, &source)),
            source,
        }),
        ParserKind::Csv(dialect) => {
            let dialect = match dialect {
                RuleCsvDialect::Comma => CsvDialect::Comma,
                RuleCsvDialect::Tab => CsvDialect::Tab,
                RuleCsvDialect::Semicolon => CsvDialect::Semicolon,
            };
            Some(ParsedInput {
                document,
                file,
                path,
                format: Eu4FileFormat::Csv,
                parsed: ParsedContent::Csv(parse_eu4_csv_file(&source, dialect)),
                source,
            })
        }
        ParserKind::Asset | ParserKind::SyntaxOnly => None,
    }
}

fn parser_for(
    snapshot: &AnalysisSnapshot,
    path: Option<&LogicalPath>,
    physical: Option<&Path>,
) -> Option<ParserKind> {
    if let Some(path) = path {
        if let Some(category) = snapshot.rules().classify(path) {
            return Some(category.parser.clone());
        }
    }
    let extension = physical
        .and_then(Path::extension)
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .or_else(|| {
            path.and_then(|path| {
                path.as_str().rsplit_once('.').map(|(_, ext)| ext.to_ascii_lowercase())
            })
        })?;
    Some(match extension.as_str() {
        "yml" | "yaml" => ParserKind::Localisation,
        "csv" => ParserKind::Csv(RuleCsvDialect::Semicolon),
        "txt" | "gui" | "gfx" | "asset" | "sfx" => ParserKind::PdxScript,
        _ => return None,
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
            path.file_name().and_then(|name| LogicalPath::parse(&name.to_string_lossy()).ok())
        })
}

fn analyze_input(snapshot: &AnalysisSnapshot, input: &ParsedInput) -> FileAnalysis {
    let semantic = semantic_data(input);
    let all = all_semantics(snapshot);
    let mut diagnostics = syntax_diagnostics(input);
    diagnostics.extend(cwt_rule_diagnostics(snapshot, input));
    let known = known_keys(snapshot);
    let mut unknown_scope_reported = false;
    for property in properties(input) {
        let is_dynamic_command = all.definitions.iter().any(|definition| {
            matches!(definition.kind.as_str(), "scripted_effect" | "scripted_trigger")
                && same_name(&definition.name, &property.key)
        });
        if !property.top_level
            && !cwt_validates_path(snapshot, &property.path)
            && !known.contains(&property.key.to_ascii_lowercase())
            && !is_dynamic_command
            && looks_unknown_key(&property.key)
        {
            diagnostics.push(Diagnostic {
                code: DiagnosticCode::UnknownKey,
                severity: DiagnosticCode::UnknownKey.severity(),
                range: property.key_range,
                message: format!("unknown EU4 key `{}`", property.key),
            });
        }
        if property.key.eq_ignore_ascii_case("scope")
            && let Some((value, range)) = property.value.as_ref()
            && !known_scope(value)
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
        match resolve_symbol(snapshot, &all, &reference.kind, &reference.name) {
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
        (diagnostic.range.start(), diagnostic.range.end(), diagnostic.code)
    });
    diagnostics.dedup_by(|left, right| {
        left.code == right.code
            && left.severity == right.severity
            && left.range == right.range
            && left.message == right.message
    });
    FileAnalysis {
        revision: snapshot.revision(),
        document: input.document.clone(),
        file: input.file,
        format: Some(input.format),
        scope: Scope::Unknown,
        diagnostics,
        symbols: semantic.definitions.into_iter().map(|definition| definition.symbol).collect(),
        references: semantic
            .references
            .into_iter()
            .map(|reference| {
                let location = reference.location();
                ReferenceInfo { kind: reference.kind, name: reference.name, location }
            })
            .collect(),
    }
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
struct Eu4ScopeContext {
    root: String,
    current: String,
    from: Vec<String>,
    previous: Vec<String>,
}

impl Default for Eu4ScopeContext {
    fn default() -> Self {
        Self {
            root: "any".to_owned(),
            current: "any".to_owned(),
            from: Vec::new(),
            previous: Vec::new(),
        }
    }
}

fn cwt_rule_diagnostics(snapshot: &AnalysisSnapshot, input: &ParsedInput) -> Vec<Diagnostic> {
    if input.format != Eu4FileFormat::PdxScript || snapshot.rules().model().cwt.rules.is_empty() {
        return Vec::new();
    }
    let ParsedContent::Text(parsed) = &input.parsed else { return Vec::new() };
    let roots = script_properties(input, parsed.root());
    let mut diagnostics = Vec::new();
    for property in roots {
        let Some(context) = cwt_root_context(snapshot, &property.key, input.path.as_ref()) else {
            continue;
        };
        let scope = cwt_initial_scope(snapshot, &context, &property.key);
        if let Some(type_name) = context.strip_prefix("type:")
            && snapshot.rules().model().cwt.type_descriptors.get(type_name).is_some_and(
                |descriptor| {
                    descriptor.skip_root_paths.iter().any(|path| {
                        path.first().is_some_and(|key| {
                            key.eq_ignore_ascii_case("any")
                                || key.eq_ignore_ascii_case(&property.key)
                        })
                    })
                },
            )
        {
            for child in &property.block {
                let child_scope = cwt_initial_scope(snapshot, &context, &child.key);
                validate_cwt_container(
                    snapshot,
                    &context,
                    &[],
                    &child.block,
                    &child.bare_values,
                    &child_scope,
                    &mut diagnostics,
                );
            }
            continue;
        }
        validate_cwt_container(
            snapshot,
            &context,
            &[],
            &property.block,
            &property.bare_values,
            &scope,
            &mut diagnostics,
        );
    }
    diagnostics
}

fn cwt_initial_scope(
    snapshot: &AnalysisSnapshot,
    context: &str,
    root_key: &str,
) -> Eu4ScopeContext {
    let mut scope = Eu4ScopeContext::default();
    if let Some(type_name) = context.strip_prefix("type:")
        && let Some(root_scope) = snapshot
            .rules()
            .model()
            .cwt
            .type_root_scopes
            .get(type_name)
            .and_then(|roots| roots.get(root_key))
    {
        scope.root.clone_from(root_scope);
        scope.current.clone_from(root_scope);
        return scope;
    }
    match root_key.to_ascii_lowercase().as_str() {
        "country_event" => {
            scope.root = "country".to_owned();
            scope.current = "country".to_owned();
        }
        "province_event" => {
            scope.root = "province".to_owned();
            scope.current = "province".to_owned();
        }
        _ => {}
    }
    scope
}

fn cwt_root_context(
    snapshot: &AnalysisSnapshot,
    key: &str,
    logical_path: Option<&LogicalPath>,
) -> Option<String> {
    let rules = &snapshot.rules().model().cwt.rules;
    if rules.iter().any(|rule| rule.context.eq_ignore_ascii_case(key)) {
        return Some(key.to_owned());
    }
    let root = format!("root:{key}");
    if rules.iter().any(|rule| rule.context.eq_ignore_ascii_case(&root)) {
        return Some(root);
    }
    snapshot
        .rules()
        .model()
        .cwt
        .type_root_keys
        .iter()
        .find(|(type_name, roots)| {
            let descriptor = snapshot.rules().model().cwt.type_descriptors.get(*type_name);
            (roots.iter().any(|root| root.eq_ignore_ascii_case(key))
                || descriptor.is_some_and(|descriptor| {
                    descriptor.starts_with.as_deref().is_some_and(|prefix| {
                        key.to_ascii_lowercase().starts_with(&prefix.to_ascii_lowercase())
                    }) || descriptor.skip_root_paths.iter().any(|path| {
                        path.first().is_some_and(|root| {
                            root.eq_ignore_ascii_case("any") || root.eq_ignore_ascii_case(key)
                        })
                    })
                }))
                && descriptor
                    .is_none_or(|descriptor| cwt_type_path_matches(descriptor, logical_path))
        })
        .map(|(type_name, _)| format!("type:{type_name}"))
        .or_else(|| {
            if !logical_path.is_some_and(|path| path.as_str().contains('/')) {
                return None;
            }
            snapshot
                .rules()
                .model()
                .cwt
                .type_descriptors
                .iter()
                .find(|(type_name, descriptor)| {
                    let starts_with = descriptor.starts_with.as_deref().is_some_and(|prefix| {
                        key.to_ascii_lowercase().starts_with(&prefix.to_ascii_lowercase())
                    });
                    (!snapshot.rules().model().cwt.type_root_keys.contains_key(*type_name)
                        || starts_with)
                        && (starts_with
                            || descriptor.skip_root_paths.iter().any(|path| {
                                path.first().is_some_and(|root| {
                                    root.eq_ignore_ascii_case("any")
                                        || root.eq_ignore_ascii_case(key)
                                })
                            }))
                        && cwt_type_path_matches(descriptor, logical_path)
                })
                .map(|(type_name, _)| format!("type:{type_name}"))
        })
}

fn cwt_type_path_matches(
    descriptor: &pdx_eu4::CwtTypeDescriptor,
    logical_path: Option<&LogicalPath>,
) -> bool {
    let Some(logical_path) = logical_path else { return true };
    let path = logical_path.as_str().replace('\\', "/").to_ascii_lowercase();
    if !path.contains('/') {
        return true;
    }
    if let Some(prefix) = descriptor.path.as_deref() {
        let prefix = prefix
            .trim_matches('/')
            .strip_prefix("game/")
            .unwrap_or(prefix.trim_matches('/'))
            .to_ascii_lowercase();
        let prefix_match = path == prefix || path.starts_with(&format!("{prefix}/"));
        if !prefix_match {
            return false;
        }
        if descriptor.path_strict
            && path.strip_prefix(&format!("{prefix}/")).is_some_and(|rest| rest.contains('/'))
        {
            return false;
        }
    }
    if let Some(file) = descriptor.path_file.as_deref()
        && !path.ends_with(&file.to_ascii_lowercase())
    {
        return false;
    }
    if let Some(extension) = descriptor.path_extension.as_deref() {
        let extension = extension.trim_start_matches('.').to_ascii_lowercase();
        if !path.ends_with(&format!(".{extension}")) {
            return false;
        }
    }
    true
}

fn cwt_validates_path(snapshot: &AnalysisSnapshot, path: &[String]) -> bool {
    let Some(root) = path.first() else { return false };
    cwt_root_context(snapshot, root, None).is_some()
}

fn script_properties(input: &ParsedInput, parent: &CstNode) -> Vec<ScriptProperty> {
    parent
        .children()
        .iter()
        .filter(|node| node.kind() == CstKind::Property)
        .filter_map(|node| {
            let (key, key_range) = property_key(input, node)?;
            let value = node.children().iter().find(|child| child.kind() == CstKind::Value);
            let block_node = value.and_then(|value| {
                value.children().iter().find(|child| child.kind() == CstKind::Block)
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

fn validate_cwt_container(
    snapshot: &AnalysisSnapshot,
    context: &str,
    parent_path: &[String],
    properties: &[ScriptProperty],
    bare_values: &[(String, TextRange)],
    scope: &Eu4ScopeContext,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let rules = snapshot
        .rules()
        .model()
        .cwt
        .rules
        .iter()
        .filter(|rule| {
            let context_matches = rule.context.eq_ignore_ascii_case(context)
                || (context.strip_prefix("type:").is_some_and(|type_name| {
                    rule.context.eq_ignore_ascii_case(&format!("root:{type_name}"))
                }));
            context_matches && cwt_parent_path_matches(snapshot, &rule.parent_path, parent_path)
        })
        .collect::<Vec<_>>();
    if rules.is_empty() {
        return;
    }
    let selected_alternative =
        cwt_selected_alternative(snapshot, &rules, properties, bare_values, scope);
    let mut counts = std::collections::BTreeMap::<String, u32>::new();
    for property in properties {
        let key = property.key.to_ascii_lowercase();
        let count = counts.entry(key).or_default();
        *count = count.saturating_add(1);
        let matching = rules
            .iter()
            .filter(|rule| {
                !matches!(rule.shape, CwtRuleShape::LeafValue)
                    && cwt_key_matches(snapshot, &rule.key, &property.key)
            })
            .copied()
            .collect::<Vec<_>>();
        if matching.is_empty() {
            diagnostics.push(Diagnostic {
                code: DiagnosticCode::UnknownKey,
                severity: DiagnosticCode::UnknownKey.severity(),
                range: property.key_range,
                message: format!("unexpected key `{}` in CWT context `{context}`", property.key),
            });
        } else {
            let scoped_matching = matching
                .iter()
                .filter(|rule| cwt_scope_allows(rule, scope))
                .copied()
                .collect::<Vec<_>>();
            if scoped_matching.is_empty() {
                diagnostics.push(Diagnostic {
                    code: DiagnosticCode::WrongScope,
                    severity: cwt_rule_severity(
                        matching.iter().copied(),
                        DiagnosticCode::WrongScope,
                    ),
                    range: property.key_range,
                    message: format!(
                        "`{}` is not available in EU4 scope `{}`",
                        property.key, scope.current
                    ),
                });
            }
            let applicable = if scoped_matching.is_empty() { &matching } else { &scoped_matching };
            let valid =
                applicable.iter().any(|rule| cwt_property_matches(snapshot, rule, property, scope));
            if !valid {
                diagnostics.push(Diagnostic {
                    code: DiagnosticCode::InvalidValue,
                    severity: cwt_rule_severity(
                        applicable.iter().copied(),
                        DiagnosticCode::InvalidValue,
                    ),
                    range: property.scalar.as_ref().map_or(property.key_range, |(_, range)| *range),
                    message: format!("value of `{}` does not match the CWT rule", property.key),
                });
            }
            let max_occurs = applicable
                .iter()
                .filter(|rule| cwt_rule_is_selected(rule, selected_alternative.as_deref()))
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
                        "`{}` occurs {} times, but CWT cardinality allows at most {}",
                        property.key, count, max_occurs
                    ),
                });
            }
        }
        let next_rule = matching
            .iter()
            .filter(|rule| cwt_rule_is_selected(rule, selected_alternative.as_deref()))
            .find(|rule| cwt_scope_allows(rule, scope))
            .copied()
            .or_else(|| matching.iter().find(|rule| cwt_scope_allows(rule, scope)).copied());
        let (next_context, child_path) =
            next_rule.and_then(|rule| rule.child_context.as_deref()).map_or_else(
                || {
                    let mut child_path = parent_path.to_vec();
                    child_path.push(property.key.clone());
                    (context.to_owned(), child_path)
                },
                |child_context| (child_context.to_owned(), Vec::new()),
            );
        let next_scope =
            next_rule.map_or_else(|| scope.clone(), |rule| cwt_child_scope(scope, rule));
        validate_cwt_container(
            snapshot,
            &next_context,
            &child_path,
            &property.block,
            &property.bare_values,
            &next_scope,
            diagnostics,
        );
    }
    for (value, value_range) in bare_values {
        let matching = rules
            .iter()
            .filter(|rule| {
                matches!(rule.shape, CwtRuleShape::LeafValue)
                    && cwt_leaf_value_matches(snapshot, rule, value, scope)
            })
            .copied()
            .collect::<Vec<_>>();
        if matching.is_empty() {
            diagnostics.push(Diagnostic {
                code: DiagnosticCode::InvalidValue,
                severity: DiagnosticCode::InvalidValue.severity(),
                range: *value_range,
                message: format!("bare value `{value}` does not match the CWT value clause"),
            });
        }
    }
    let empty_range =
        properties.first().map_or_else(|| TextRange::empty(0), |property| property.key_range);
    for rule in rules.iter().filter(|rule| cwt_scope_allows(rule, scope)) {
        if !cwt_rule_is_selected(rule, selected_alternative.as_deref()) {
            continue;
        }
        if matches!(rule.shape, CwtRuleShape::LeafValue) {
            let count = bare_values
                .iter()
                .filter(|(value, _)| cwt_leaf_value_matches(snapshot, rule, value, scope))
                .count();
            let count = u32::try_from(count).unwrap_or(u32::MAX);
            if let Some(min_occurs) = rule.min_occurs
                && count < min_occurs
            {
                diagnostics.push(Diagnostic {
                    code: DiagnosticCode::Cardinality,
                    severity: cwt_min_cardinality_severity(rule),
                    range: empty_range,
                    message: format!(
                        "CWT value clause requires at least {min_occurs} value(s), but `{}` occurs {count} times",
                        cwt_value_matcher_label(&rule.value)
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
                        "CWT value clause allows at most {max_occurs} value(s), but found {count}"
                    ),
                });
            }
            continue;
        }
        let Some(min_occurs) = rule.min_occurs else { continue };
        let count = properties
            .iter()
            .filter(|property| {
                cwt_key_matches(snapshot, &rule.key, &property.key)
                    && !matches!(rule.shape, CwtRuleShape::LeafValue)
            })
            .count();
        let count = u32::try_from(count).unwrap_or(u32::MAX);
        if count < min_occurs {
            diagnostics.push(Diagnostic {
                code: DiagnosticCode::Cardinality,
                severity: cwt_min_cardinality_severity(rule),
                range: empty_range,
                message: format!(
                    "CWT rule requires at least {min_occurs} occurrence(s), but `{}` occurs {count} times",
                    cwt_matcher_label(&rule.key)
                ),
            });
        }
    }
}

fn cwt_rule_is_selected(rule: &pdx_eu4::CwtSemanticRule, selected: Option<&str>) -> bool {
    rule.alternative_id.as_deref().is_none_or(|alternative| selected == Some(alternative))
}

fn cwt_selected_alternative(
    snapshot: &AnalysisSnapshot,
    rules: &[&pdx_eu4::CwtSemanticRule],
    properties: &[ScriptProperty],
    bare_values: &[(String, TextRange)],
    scope: &Eu4ScopeContext,
) -> Option<String> {
    let mut alternatives = Vec::<String>::new();
    for rule in rules {
        if let Some(alternative) = rule.alternative_id.as_ref()
            && !alternatives.iter().any(|known| known == alternative)
        {
            alternatives.push(alternative.clone());
        }
    }
    let mut best: Option<(usize, usize, String)> = None;
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
                !matches!(rule.shape, CwtRuleShape::LeafValue)
                    && cwt_key_matches(snapshot, &rule.key, &property.key)
            });
            if matching.clone().next().is_some() {
                present += 1;
            }
            if matching
                .filter(|rule| cwt_scope_allows(rule, scope))
                .any(|rule| cwt_property_matches(snapshot, rule, property, scope))
            {
                valid += 1;
            }
        }
        valid += bare_values
            .iter()
            .filter(|(value, _)| {
                group.iter().any(|rule| {
                    matches!(rule.shape, CwtRuleShape::LeafValue)
                        && cwt_leaf_value_matches(snapshot, rule, value, scope)
                })
            })
            .count();
        let score = (valid, present, alternative.clone());
        if best.as_ref().is_none_or(|current| {
            score.0 > current.0 || (score.0 == current.0 && score.1 > current.1)
        }) {
            best = Some(score);
        }
    }
    best.map(|(_, _, alternative)| alternative)
}

fn cwt_leaf_value_matches(
    snapshot: &AnalysisSnapshot,
    rule: &pdx_eu4::CwtSemanticRule,
    value: &str,
    scope: &Eu4ScopeContext,
) -> bool {
    match &rule.value {
        CwtValueMatcher::Dynamic(kind) => cwt_dynamic_value_matches(snapshot, kind, value, scope),
        CwtValueMatcher::DynamicSet(_) => !value.is_empty(),
        matcher => matcher.matches(
            value,
            |type_name, member| workspace_member(snapshot, type_name, member),
            |enum_name, member| enum_member(snapshot, enum_name, member),
            |scope_name, member| scope_member(scope_name, member, scope),
        ),
    }
}

fn cwt_value_matcher_label(matcher: &CwtValueMatcher) -> String {
    match matcher {
        CwtValueMatcher::AnyScalar => "scalar".to_owned(),
        CwtValueMatcher::Exact(value) => value.clone(),
        CwtValueMatcher::Bool => "bool".to_owned(),
        CwtValueMatcher::Int { .. } => "int".to_owned(),
        CwtValueMatcher::Float { .. } => "float".to_owned(),
        CwtValueMatcher::Type(value) => format!("<{value}>"),
        CwtValueMatcher::Enum(value) => format!("enum[{value}]"),
        CwtValueMatcher::Scope(value) => {
            value.as_deref().map_or_else(|| "scope".to_owned(), |value| format!("scope[{value}]"))
        }
        CwtValueMatcher::Localisation => "localisation".to_owned(),
        CwtValueMatcher::Filepath => "filepath".to_owned(),
        CwtValueMatcher::Dynamic(value) => format!("value[{value}]"),
        CwtValueMatcher::DynamicSet(value) => format!("value_set[{value}]"),
        CwtValueMatcher::Opaque(value) => value.clone(),
    }
}

fn cwt_matcher_label(matcher: &CwtKeyMatcher) -> String {
    match matcher {
        CwtKeyMatcher::Exact(value) => value.clone(),
        CwtKeyMatcher::Type(value) => format!("<{value}>"),
        CwtKeyMatcher::Enum(value) => format!("enum[{value}]"),
        CwtKeyMatcher::AnyScalar => "scalar".to_owned(),
        CwtKeyMatcher::Dynamic(value) => format!("value_set[{value}]"),
    }
}

fn cwt_parent_path_matches(
    snapshot: &AnalysisSnapshot,
    expected: &[String],
    actual: &[String],
) -> bool {
    expected.len() == actual.len()
        && expected.iter().zip(actual).all(|(expected, actual)| {
            if let Some(type_name) =
                expected.strip_prefix('<').and_then(|name| name.strip_suffix('>'))
            {
                workspace_member(snapshot, type_name, actual)
            } else if let Some(enum_name) =
                expected.strip_prefix("enum[").and_then(|name| name.strip_suffix(']'))
            {
                enum_member(snapshot, enum_name, actual)
            } else {
                expected.eq_ignore_ascii_case(actual)
            }
        })
}

fn cwt_rule_severity<'a>(
    rules: impl IntoIterator<Item = &'a pdx_eu4::CwtSemanticRule>,
    fallback: DiagnosticCode,
) -> u8 {
    rules.into_iter().filter_map(|rule| rule.severity).min().unwrap_or_else(|| fallback.severity())
}

fn cwt_min_cardinality_severity(rule: &pdx_eu4::CwtSemanticRule) -> u8 {
    if !rule.strict_min {
        2
    } else {
        rule.severity.unwrap_or(DiagnosticCode::Cardinality.severity())
    }
}

fn cwt_scope_allows(rule: &pdx_eu4::CwtSemanticRule, scope: &Eu4ScopeContext) -> bool {
    rule.allowed_scopes.is_empty()
        || rule.allowed_scopes.iter().any(|expected| scope_compatible(&scope.current, expected))
}

fn cwt_child_scope(parent: &Eu4ScopeContext, rule: &pdx_eu4::CwtSemanticRule) -> Eu4ScopeContext {
    let mut child = parent.clone();
    if let Some(push_scope) = &rule.push_scope
        && !push_scope.eq_ignore_ascii_case("any")
    {
        child.previous.insert(0, child.current.clone());
        child.current.clone_from(push_scope);
    }
    for (register, value) in &rule.replace_scope {
        let register = register.to_ascii_lowercase().replace('_', "");
        match register.as_str() {
            "root" => child.root.clone_from(value),
            "this" => child.current.clone_from(value),
            _ if register.starts_with("from") => {
                let depth = register.matches("from").count().saturating_sub(1);
                set_scope_register(&mut child.from, depth, value);
            }
            _ if register.starts_with("prev") || register.starts_with("previous") => {
                let prefix = if register.starts_with("previous") { "previous" } else { "prev" };
                let depth = register.matches(prefix).count().saturating_sub(1);
                set_scope_register(&mut child.previous, depth, value);
            }
            _ => {}
        }
    }
    child
}

fn set_scope_register(registers: &mut Vec<String>, depth: usize, value: &str) {
    if registers.len() <= depth {
        registers.resize(depth + 1, "any".to_owned());
    }
    registers[depth] = value.to_owned();
}

fn cwt_key_matches(snapshot: &AnalysisSnapshot, matcher: &CwtKeyMatcher, key: &str) -> bool {
    matcher.matches(
        key,
        |type_name, member| workspace_member(snapshot, type_name, member),
        |enum_name, member| enum_member(snapshot, enum_name, member),
    )
}

fn cwt_property_matches(
    snapshot: &AnalysisSnapshot,
    rule: &pdx_eu4::CwtSemanticRule,
    property: &ScriptProperty,
    scope_context: &Eu4ScopeContext,
) -> bool {
    let shape_matches = match rule.shape {
        CwtRuleShape::Node => property.block_range.is_some(),
        CwtRuleShape::ValueClause => {
            property.block_range.is_some() && !property.bare_values.is_empty()
        }
        CwtRuleShape::Leaf | CwtRuleShape::LeafValue => property.scalar.is_some(),
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
        return matches!(rule.value, CwtValueMatcher::AnyScalar | CwtValueMatcher::Opaque(_));
    };
    if let CwtValueMatcher::Dynamic(kind) = &rule.value {
        return cwt_dynamic_value_matches(snapshot, kind, value, scope_context);
    }
    if let CwtValueMatcher::DynamicSet(_) = &rule.value {
        return !value.is_empty();
    }
    rule.value.matches(
        value,
        |type_name, member| workspace_member(snapshot, type_name, member),
        |enum_name, member| enum_member(snapshot, enum_name, member),
        |scope, member| scope_member(scope, member, scope_context),
    )
}

fn cwt_dynamic_value_matches(
    snapshot: &AnalysisSnapshot,
    kind: &str,
    value: &str,
    scope_context: &Eu4ScopeContext,
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
    let base = type_name.split_once('.').map_or(type_name, |(kind, _)| kind);
    let candidates = [type_name, base, eu4_member_kind_alias(base).unwrap_or(base)];
    snapshot.index().definitions_iter().any(|definition| {
        candidates.iter().any(|candidate| definition.kind.eq_ignore_ascii_case(candidate))
            && definition.name.eq_ignore_ascii_case(member)
    })
}

fn enum_member(snapshot: &AnalysisSnapshot, enum_name: &str, member: &str) -> bool {
    let static_member = snapshot
        .rules()
        .model()
        .cwt
        .enum_values
        .get(enum_name)
        .is_some_and(|values| values.iter().any(|value| value.eq_ignore_ascii_case(member)));
    static_member
        || eu4_member_kind_alias(enum_name)
            .is_some_and(|kind| workspace_member(snapshot, kind, member))
        || workspace_member(snapshot, enum_name, member)
        || (enum_name.eq_ignore_ascii_case("scripted_effect_params")
            && (member.eq_ignore_ascii_case("scaled_skill")
                || workspace_member(snapshot, "scripted_effect_param", member)))
        || (enum_name.eq_ignore_ascii_case("scripted_effect_params_dollar")
            && workspace_member(snapshot, "scripted_effect_param_dollar", member))
}

fn eu4_member_kind_alias(type_name: &str) -> Option<&'static str> {
    Some(match type_name.to_ascii_lowercase().as_str() {
        "country_tags" | "country_tag" => "country_tag",
        "trade_nodes" | "tradenodes" | "trade_node" => "trade_node",
        "colonial_regions" | "colonial_region" => "colonial_region",
        "government_reforms" | "government_reform" => "government_reform",
        "subject_types" | "subject_type" => "subject_type",
        "mercenary_companies" | "mercenary_company" => "mercenary_company",
        "trade_companies" | "trade_company" => "trade_company",
        "event_modifiers" | "event_modifier" => "event_modifier",
        "static_modifiers" | "static_modifier" => "static_modifier",
        "timed_modifiers" | "timed_modifier" => "timed_modifier",
        "triggered_modifiers" | "triggered_modifier" => "triggered_modifier",
        "peace_treaties" | "peace_treaty" => "peace_treaty",
        "wargoal_types" | "wargoal_type" => "wargoal_type",
        "advisortypes" | "advisor_type" => "advisor_type",
        "leader_personalities" | "leader_personality" => "leader_personality",
        "ruler_personalities" | "ruler_personality" => "ruler_personality",
        "idea_groups" | "idea_group" => "idea_group",
        "buildings" | "building" => "building",
        "technologies" | "technology" => "technology",
        "religions" | "religion" => "religion",
        "cultures" | "culture" => "culture",
        "scripted_effect_params" => "scripted_effect_param",
        "scripted_effect_params_dollar" => "scripted_effect_param_dollar",
        "hardcoded_legacygovernments" | "hardcoded_legacy_only_governments" => {
            "hardcoded_legacy_government"
        }
        "modifiers" | "modifier" => "static_modifier",
        _ => return None,
    })
}

fn scope_member(scope: Option<&str>, member: &str, context: &Eu4ScopeContext) -> bool {
    let lowered = member.to_ascii_lowercase().replace('_', "");
    let resolved = if lowered == "root" {
        Some(context.root.as_str())
    } else if lowered == "this" {
        Some(context.current.as_str())
    } else if lowered.starts_with("from") {
        context.from.get(lowered.matches("from").count().saturating_sub(1)).map(String::as_str)
    } else if lowered.starts_with("prev") || lowered.starts_with("previous") {
        let prefix = if lowered.starts_with("previous") { "previous" } else { "prev" };
        context.previous.get(lowered.matches(prefix).count().saturating_sub(1)).map(String::as_str)
    } else {
        Some(member)
    };
    let Some(resolved) = resolved else { return false };
    known_scope(resolved) && scope.is_none_or(|expected| scope_compatible(resolved, expected))
}

fn scope_compatible(actual: &str, expected: &str) -> bool {
    if expected.eq_ignore_ascii_case("any") || actual.eq_ignore_ascii_case("any") {
        return true;
    }
    actual.eq_ignore_ascii_case(expected)
        || (actual.eq_ignore_ascii_case("trade_node") && expected.eq_ignore_ascii_case("province"))
}

fn syntax_diagnostics(input: &ParsedInput) -> Vec<Diagnostic> {
    match &input.parsed {
        ParsedContent::Text(parsed) => parsed.errors().iter().map(diagnostic_from_syntax).collect(),
        ParsedContent::Csv(parsed) => parsed.errors().iter().map(diagnostic_from_syntax).collect(),
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

fn semantic_data(input: &ParsedInput) -> SemanticFile {
    let mut data = SemanticFile { definitions: Vec::new(), references: Vec::new() };
    if let ParsedContent::Text(parsed) = &input.parsed {
        let mut properties = Vec::new();
        collect_text_semantics(input, parsed.root(), true, &mut data, &mut properties);
    } else if let ParsedContent::Csv(_) = &input.parsed {
        // CSV has syntax diagnostics and record ranges, but no generic symbol semantics.
    }
    data
}

fn collect_text_semantics(
    input: &ParsedInput,
    node: &CstNode,
    top_level: bool,
    data: &mut SemanticFile,
    properties: &mut Vec<PropertyInfo>,
) {
    match node.kind() {
        CstKind::LocalisationEntry => {
            if let Some(key) =
                node.children().iter().find(|child| child.kind() == CstKind::LocalisationKey)
                && let Some(name) = text(input, key.range())
            {
                let name = name.trim().to_owned();
                data.definitions.push(make_definition(
                    input,
                    "localisation",
                    name,
                    node.range(),
                    key.range(),
                ));
            }
        }
        CstKind::Property => {
            if let Some((key, key_range)) = property_key(input, node) {
                let value = property_scalar(input, node);
                properties.push(PropertyInfo {
                    key: key.clone(),
                    key_range,
                    value: value.clone(),
                    top_level,
                    path: Vec::new(),
                });
                if top_level && let Some(kind) = definition_kind(input.path.as_ref(), &key, node) {
                    let (name, selection_range) = definition_name(input, node, &key, key_range);
                    data.definitions.push(make_definition(
                        input,
                        &kind,
                        name,
                        node.range(),
                        selection_range,
                    ));
                }
                if let Some((kind, name, range)) = reference_from_property(input, &key, node) {
                    data.references.push(ReferenceInternal {
                        kind,
                        name,
                        range,
                        document: input.document.clone(),
                        file: input.file,
                        path: input.path.clone(),
                    });
                }
            }
        }
        _ => {}
    }
    for child in node.children() {
        let child_top_level = top_level && node.kind() == CstKind::Document;
        collect_text_semantics(input, child, child_top_level, data, properties);
    }
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
        symbol: Symbol { name, kind: kind.to_owned(), range, selection_range, location },
        document: input.document.clone(),
        file: input.file,
    }
}

fn definition_kind(path: Option<&LogicalPath>, key: &str, node: &CstNode) -> Option<String> {
    let path = path.map_or_else(String::new, |path| path.as_str().to_ascii_lowercase());
    if path.contains("scripted_effect") {
        return Some("scripted_effect".to_owned());
    }
    if path.contains("scripted_trigger") {
        return Some("scripted_trigger".to_owned());
    }
    if path.contains("events/") || key.to_ascii_lowercase().ends_with("_event") {
        return Some("event".to_owned());
    }
    if matches!(key, "country_event" | "province_event")
        && node.children().iter().any(|child| child.kind() == CstKind::Value)
    {
        return Some("event".to_owned());
    }
    None
}

fn definition_name(
    input: &ParsedInput,
    node: &CstNode,
    key: &str,
    key_range: TextRange,
) -> (String, TextRange) {
    if matches!(key, "country_event" | "province_event")
        && let Some((name, range)) = find_nested_property(input, node, "id")
    {
        return (name, range);
    }
    (key.to_owned(), key_range)
}

fn find_nested_property(
    input: &ParsedInput,
    node: &CstNode,
    wanted: &str,
) -> Option<(String, TextRange)> {
    if node.kind() == CstKind::Property
        && let Some((key, _)) = property_key(input, node)
        && key.eq_ignore_ascii_case(wanted)
    {
        return property_scalar(input, node);
    }
    node.children().iter().find_map(|child| find_nested_property(input, child, wanted))
}

fn reference_from_property(
    input: &ParsedInput,
    key: &str,
    node: &CstNode,
) -> Option<(String, String, TextRange)> {
    let lower = key.to_ascii_lowercase();
    let kind = if matches!(lower.as_str(), "event" | "events" | "event_id" | "trigger_event")
        || lower.ends_with("_event")
    {
        Some("event")
    } else if lower.contains("scripted_effect")
        || lower == "call_effect"
        || lower.ends_with("_effect")
    {
        Some("scripted_effect")
    } else if lower.contains("scripted_trigger")
        || lower == "call_trigger"
        || lower.ends_with("_trigger")
    {
        Some("scripted_trigger")
    } else if matches!(
        lower.as_str(),
        "localisation" | "localization" | "loc_key" | "name" | "desc" | "title" | "tooltip"
    ) {
        Some("localisation")
    } else {
        None
    }?;
    let (value, range) = property_scalar(input, node)?;
    if value.is_empty() || value == "yes" || value == "no" || value.parse::<f64>().is_ok() {
        return None;
    }
    Some((kind.to_owned(), value, range))
}

fn property_key(input: &ParsedInput, node: &CstNode) -> Option<(String, TextRange)> {
    let key = node.children().iter().find(|child| child.kind() == CstKind::Key)?;
    let text = text(input, key.range())?.trim().to_owned();
    Some((text, key.range()))
}

fn property_scalar(input: &ParsedInput, node: &CstNode) -> Option<(String, TextRange)> {
    let value = node.children().iter().find(|child| child.kind() == CstKind::Value)?;
    let scalar = value
        .children()
        .iter()
        .find(|child| matches!(child.kind(), CstKind::BareValue | CstKind::QuotedString))?;
    let raw = text(input, scalar.range())?.trim();
    let value =
        raw.strip_prefix('"').and_then(|value| value.strip_suffix('"')).unwrap_or(raw).to_owned();
    Some((value, scalar.range()))
}

fn text(input: &ParsedInput, range: TextRange) -> Option<&str> {
    input.source_text(range)
}

fn properties(input: &ParsedInput) -> Vec<PropertyInfo> {
    let ParsedContent::Text(parsed) = &input.parsed else { return Vec::new() };
    let mut properties = Vec::new();
    collect_properties(input, parsed.root(), true, &[], &mut properties);
    properties
}

fn collect_properties(
    input: &ParsedInput,
    node: &CstNode,
    top_level: bool,
    parent_path: &[String],
    output: &mut Vec<PropertyInfo>,
) {
    if node.kind() == CstKind::Property {
        let Some((key, key_range)) = property_key(input, node) else { return };
        let mut path = parent_path.to_vec();
        path.push(key.clone());
        output.push(PropertyInfo {
            key,
            key_range,
            value: property_scalar(input, node),
            top_level,
            path: path.clone(),
        });
        if let Some(value) = node.children().iter().find(|child| child.kind() == CstKind::Value)
            && let Some(block) =
                value.children().iter().find(|child| child.kind() == CstKind::Block)
        {
            collect_properties(input, block, false, &path, output);
        }
        return;
    }
    for child in node.children() {
        collect_properties(
            input,
            child,
            top_level && node.kind() == CstKind::Document,
            parent_path,
            output,
        );
    }
}

fn all_semantics(snapshot: &AnalysisSnapshot) -> SemanticWorkspace {
    let mut all = SemanticWorkspace::default();
    for file in snapshot.source_files().values() {
        if let Some(input) = input_for_source_file(snapshot, file.id) {
            let semantic = semantic_data(&input);
            all.definitions.extend(semantic.definitions);
            all.references.extend(semantic.references);
        }
    }
    for document in snapshot.documents().values() {
        if document.source() != DocumentSource::Overlay {
            continue;
        }
        if let Some(input) = input_for_document(snapshot, document.id()) {
            let semantic = semantic_data(&input);
            all.definitions.extend(semantic.definitions);
            all.references.extend(semantic.references);
        }
    }
    for definition in snapshot.index().definitions_iter() {
        if all.definitions.iter().any(|existing| {
            existing.kind == definition.kind
                && same_name(&existing.name, &definition.name)
                && existing.file == Some(definition.file_id)
                && existing.symbol.range == definition.range
        }) {
            continue;
        }
        all.definitions.push(index_definition_info(snapshot, definition));
    }
    all
}

fn resolve_symbol(
    snapshot: &AnalysisSnapshot,
    all: &SemanticWorkspace,
    kind: &str,
    name: &str,
) -> Resolution {
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
    if candidates.is_empty() {
        return Resolution::Missing;
    }
    let policy = snapshot
        .rules()
        .model()
        .symbol_descriptors
        .iter()
        .find(|descriptor| descriptor.kind_id.eq_ignore_ascii_case(kind))
        .map_or(SymbolResolutionPolicy::ReplaceBySymbol, |descriptor| descriptor.resolution);
    if matches!(policy, SymbolResolutionPolicy::Merge | SymbolResolutionPolicy::Unique) {
        return if candidates.len() == 1 {
            Resolution::Unique(candidates.remove(0))
        } else {
            Resolution::Ambiguous
        };
    }
    let highest = candidates.iter().map(|candidate| candidate.priority).max().unwrap_or(0);
    candidates.retain(|candidate| candidate.priority == highest);
    if candidates.len() == 1 {
        Resolution::Unique(candidates.remove(0))
    } else {
        Resolution::Ambiguous
    }
}

fn definition_priority(snapshot: &AnalysisSnapshot, definition: &DefinitionInfo) -> u64 {
    if definition.document.is_some() {
        return 20_000;
    }
    let Some(file) = definition.file.and_then(|id| snapshot.source_files().get(&id)) else {
        return 0;
    };
    let Some(root) = snapshot.source_roots().iter().find(|root| root.id == file.root_id) else {
        return 0;
    };
    match root.kind {
        pdx_workspace::SourceRootKind::Vanilla => 0,
        pdx_workspace::SourceRootKind::Dependency => 1_000 + u64::from(root.order),
        pdx_workspace::SourceRootKind::CurrentMod => 10_000 + u64::from(root.order),
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
        selection_range: definition.range,
        priority: definition_priority_for_file(snapshot, definition.file_id),
    }
}

fn index_definition_info(snapshot: &AnalysisSnapshot, definition: &Definition) -> DefinitionInfo {
    let path =
        snapshot.source_files().get(&definition.file_id).map(|file| file.logical_path.clone());
    let location =
        Location { document: None, file: Some(definition.file_id), path, range: definition.range };
    DefinitionInfo {
        kind: definition.kind.clone(),
        name: definition.name.clone(),
        symbol: Symbol {
            name: definition.name.clone(),
            kind: definition.kind.clone(),
            range: definition.range,
            selection_range: definition.range,
            location,
        },
        document: None,
        file: Some(definition.file_id),
    }
}

fn definition_priority_for_file(snapshot: &AnalysisSnapshot, id: SourceFileId) -> u64 {
    let Some(file) = snapshot.source_files().get(&id) else { return 0 };
    let Some(root) = snapshot.source_roots().iter().find(|root| root.id == file.root_id) else {
        return 0;
    };
    match root.kind {
        pdx_workspace::SourceRootKind::Vanilla => 0,
        pdx_workspace::SourceRootKind::Dependency => 1_000 + u64::from(root.order),
        pdx_workspace::SourceRootKind::CurrentMod => 10_000 + u64::from(root.order),
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

fn hover_for_symbol(
    snapshot: &AnalysisSnapshot,
    all: &SemanticWorkspace,
    kind: &str,
    name: &str,
    range: TextRange,
) -> Hover {
    let contents = match resolve_symbol(snapshot, all, kind, name) {
        Resolution::Unique(definition) => {
            let path = definition
                .location
                .path
                .as_ref()
                .map_or_else(|| "<open document>".to_owned(), |path| path.as_str().to_owned());
            format!("{} `{}`\n\nDefined in `{}`", kind, name, path)
        }
        Resolution::Ambiguous => format!("ambiguous {} `{}`", kind, name),
        Resolution::Missing => format!("unresolved {} `{}`", kind, name),
    };
    Hover { contents, range: Some(range) }
}

fn known_keys(snapshot: &AnalysisSnapshot) -> BTreeSet<String> {
    const BUILTINS: &[&str] = &[
        "id",
        "name",
        "desc",
        "title",
        "picture",
        "type",
        "trigger",
        "immediate",
        "option",
        "options",
        "ai_chance",
        "effect",
        "hidden",
        "is_triggered_only",
        "country_event",
        "province_event",
        "event",
        "always",
        "limit",
        "else",
        "if",
        "custom_tooltip",
        "tooltip",
        "text",
        "scope",
        "from",
        "root",
        "prev",
        "owner",
        "controller",
        "capital",
        "location",
        "value",
        "factor",
        "modifier",
        "add",
        "remove",
        "set",
        "yes",
        "no",
        "true",
        "false",
        "random",
        "weight",
        "mean_time_to_happen",
        "days",
        "months",
        "years",
        "chance",
        "is_valid",
        "allow",
        "target",
        "file",
        "path",
        "color",
        "culture",
        "religion",
        "province",
        "country",
        "tag",
        "flag",
        "has_country_flag",
        "set_country_flag",
        "clr_country_flag",
        "has_global_flag",
        "set_global_flag",
        "clr_global_flag",
        "add_manpower",
        "add_prestige",
        "add_stability",
        "add_treasury",
        "change_variable",
        "check_variable",
        "set_variable",
        "save_event_target_as",
        "fire_event",
        "call_scripted_effect",
        "call_scripted_trigger",
        "scripted_effect",
        "scripted_trigger",
        "localisation",
        "localization",
        "loc_key",
    ];
    let mut keys = BUILTINS.iter().map(|key| (*key).to_owned()).collect::<BTreeSet<_>>();
    for record in &snapshot.rules().model().records {
        keys.extend(record.fields.keys().map(|key| key.to_ascii_lowercase()));
    }
    // The imported descriptor catalog is the authoritative extension point for semantic keys.
    // Keep the small bootstrap list above so syntax-only/degraded servers remain useful, then
    // admit every descriptor name supplied by a validated EU4 rules artifact.
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

fn looks_unknown_key(key: &str) -> bool {
    !key.trim().is_empty()
}

fn known_scope(value: &str) -> bool {
    matches!(
        value.to_ascii_lowercase().as_str(),
        "any"
            | "root"
            | "from"
            | "prev"
            | "previous"
            | "prev_prev"
            | "this"
            | "owner"
            | "controller"
            | "capital"
            | "capital_scope"
            | "location"
            | "province"
            | "country"
            | "trade_node"
            | "unit"
            | "monarch"
            | "heir"
            | "consort"
            | "mercenary_company"
            | "rebel_faction"
            | "religion"
            | "culture"
            | "advisor"
            | "leader"
            | "trade_company"
            | "global"
            | "none"
            | "overlord"
            | "event_target"
            | "global_event_target"
    )
}

fn completion_value_context(input: &ParsedInput, position: TextSize) -> bool {
    let offset = usize::try_from(position).unwrap_or(input.source.len()).min(input.source.len());
    let line_start = input.source[..offset].rfind('\n').map_or(0, |index| index + 1);
    let line = &input.source[line_start..offset];
    if input.format == Eu4FileFormat::Localisation {
        return line.contains(':') && !line.trim_start().starts_with('#');
    }
    let equals = line.rfind('=');
    let open = line.rfind('{');
    equals.is_some_and(|equals| open.is_none_or(|open| equals > open))
}

fn add_scalar_items(items: &mut Vec<CompletionItem>, range: TextRange, prefix: &str) {
    for (label, score) in
        [("yes", 0), ("no", 0), ("true", 5), ("false", 5), ("ROOT", 10), ("FROM", 10), ("PREV", 10)]
    {
        push_completion(
            items,
            CompletionItem {
                label: label.to_owned(),
                kind: CompletionKind::Value,
                detail: "EU4 scalar".to_owned(),
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
    if prefix.is_empty()
        || item.label.to_ascii_lowercase().starts_with(&prefix.to_ascii_lowercase())
    {
        items.push(item);
    }
}

fn word_range(source: &str, position: TextSize) -> TextRange {
    let mut offset = usize::try_from(position).unwrap_or(source.len()).min(source.len());
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
    TextRange::new(u32::try_from(start).unwrap_or(u32::MAX), u32::try_from(end).unwrap_or(u32::MAX))
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
) -> Result<RenameTarget, RenameError> {
    let input = input_for_document(snapshot, document).ok_or(RenameError::NoSymbol)?;
    let all = all_semantics(snapshot);
    let Some((kind, name)) = symbol_at(&all, document, position) else {
        return Err(RenameError::NoSymbol);
    };
    let definition = match resolve_symbol(snapshot, &all, &kind, &name) {
        Resolution::Unique(definition) => definition,
        Resolution::Ambiguous => return Err(RenameError::Ambiguous),
        Resolution::Missing => return Err(RenameError::Unresolved),
    };
    if !writable_location(snapshot, &definition.location) {
        return Err(RenameError::ReadOnly);
    }
    Ok(RenameTarget { kind, name, cursor_range: word_range(&input.source, position), definition })
}

fn check_rename_conflict(
    snapshot: &AnalysisSnapshot,
    all: &SemanticWorkspace,
    target: &RenameTarget,
    new_name: &str,
) -> Result<(), RenameError> {
    let policy = snapshot
        .rules()
        .model()
        .symbol_descriptors
        .iter()
        .find(|descriptor| descriptor.kind_id.eq_ignore_ascii_case(&target.kind))
        .map_or(SymbolResolutionPolicy::ReplaceBySymbol, |descriptor| descriptor.resolution);
    for definition in &all.definitions {
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
            return Err(RenameError::Conflict);
        }
    }
    Ok(())
}

fn valid_rename_name(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(is_word_byte)
}

fn writable_location(snapshot: &AnalysisSnapshot, location: &Location) -> bool {
    if let Some(file) = location.file
        && let Some(source_file) = snapshot.source_files().get(&file)
    {
        return snapshot
            .source_roots()
            .iter()
            .find(|root| root.id == source_file.root_id)
            .is_some_and(|root| matches!(root.kind, pdx_workspace::SourceRootKind::CurrentMod));
    }
    if let Some(document_id) = location.document.as_ref()
        && let Some(document) = snapshot.document(document_id)
    {
        if document.source() != DocumentSource::Overlay {
            return false;
        }
        return document.path().is_none_or(|path| {
            root_for_path(snapshot, path)
                .is_some_and(|root| matches!(root.kind, pdx_workspace::SourceRootKind::CurrentMod))
        });
    }
    false
}

fn root_for_path<'a>(
    snapshot: &'a AnalysisSnapshot,
    path: &Path,
) -> Option<&'a pdx_workspace::SourceRoot> {
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
    (2, location.path.as_ref().map_or_else(String::new, |path| path.as_str().to_owned()))
}

fn fuzzy_match(value: &str, query: &str) -> bool {
    let mut chars = value.chars();
    query.chars().all(|wanted| chars.by_ref().any(|actual| actual == wanted))
}

#[cfg(test)]
mod tests {
    use super::{
        CompletionKind, DiagnosticCode, RenameError, complete, definition, diagnostics,
        document_symbols, hover, prepare_rename, references, rename, workspace_symbols,
    };
    use pdx_eu4::{
        CwtKeyMatcher, CwtRuleShape, CwtSemanticRule, CwtValueMatcher, Eu4Rules, RulesModel,
    };
    use pdx_text::TextRange;
    use pdx_workspace::{AnalysisHost, DocumentId};

    fn snapshot(text: &str) -> (AnalysisHost, DocumentId) {
        let mut host = AnalysisHost::new(Eu4Rules::bootstrap());
        let id = DocumentId::new("file:///tmp/common/events/test.txt");
        host.open_document(id.clone(), 1, text.to_owned(), None).expect("open");
        (host, id)
    }

    fn cwt_snapshot(text: &str) -> (AnalysisHost, DocumentId) {
        cwt_snapshot_with_constraints(text, None, None, Some(1))
    }

    fn cwt_snapshot_with_severity(text: &str, severity: Option<u8>) -> (AnalysisHost, DocumentId) {
        cwt_snapshot_with_constraints(text, severity, None, Some(1))
    }

    fn cwt_snapshot_with_constraints(
        text: &str,
        severity: Option<u8>,
        min_occurs: Option<u32>,
        max_occurs: Option<u32>,
    ) -> (AnalysisHost, DocumentId) {
        let mut model = RulesModel::bootstrap();
        model.cwt.rules.push(CwtSemanticRule {
            id: "fixture:trigger:foo".to_owned(),
            context: "trigger".to_owned(),
            parent_path: Vec::new(),
            key: CwtKeyMatcher::Exact("foo".to_owned()),
            operator: None,
            value: CwtValueMatcher::Bool,
            shape: CwtRuleShape::Leaf,
            child_context: None,
            alternative_id: None,
            severity,
            required: false,
            documentation: Vec::new(),
            allowed_scopes: Vec::new(),
            push_scope: None,
            replace_scope: Vec::new(),
            min_occurs,
            strict_min: true,
            max_occurs,
            source_file: "fixture.cwt".to_owned(),
            line: 1,
        });
        let mut host = AnalysisHost::new(Eu4Rules::from_model(model));
        let id = DocumentId::new("file:///tmp/common/events/test.txt");
        host.open_document(id.clone(), 1, text.to_owned(), None).expect("open");
        (host, id)
    }

    #[test]
    fn incomplete_input_has_syntax_diagnostics_and_completion() {
        let (host, id) = snapshot("country_event = { id = test.1\n  un");
        let snapshot = host.snapshot();
        assert!(diagnostics(&snapshot, &id).iter().any(|item| item.code == DiagnosticCode::Syntax));
        let result = complete(&snapshot, &id, 35);
        assert!(!result.items.is_empty());
    }

    #[test]
    fn unresolved_symbol_is_diagnosed_without_a_definition() {
        let (host, id) = snapshot("event = missing.1\n");
        let diagnostics = diagnostics(&host.snapshot(), &id);
        assert!(diagnostics.iter().any(|item| item.code == DiagnosticCode::UnknownSymbol));
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
    }

    #[test]
    fn unknown_key_and_unknown_scope_are_independent_diagnostics() {
        let (host, id) = snapshot("country_event = { unknown_key = yes scope = nowhere }\n");
        let diagnostics = diagnostics(&host.snapshot(), &id);
        assert!(diagnostics.iter().any(|item| item.code == DiagnosticCode::UnknownKey));
        assert!(diagnostics.iter().any(|item| item.code == DiagnosticCode::UnknownScope));
        assert_eq!(
            diagnostics.iter().filter(|item| item.code == DiagnosticCode::UnknownScope).count(),
            1
        );
    }

    #[test]
    fn cwt_matcher_rejects_invalid_values_and_unknown_keys() {
        let (host, id) = cwt_snapshot("trigger = { foo = maybe unknown = yes }\n");
        let diagnostics = diagnostics(&host.snapshot(), &id);
        assert!(diagnostics.iter().any(|item| item.code == DiagnosticCode::InvalidValue));
        assert!(diagnostics.iter().any(|item| item.code == DiagnosticCode::UnknownKey));
    }

    #[test]
    fn cwt_rule_severity_reaches_editor_diagnostic() {
        let (host, id) = cwt_snapshot_with_severity("trigger = { foo = maybe }\n", Some(2));
        let diagnostics = diagnostics(&host.snapshot(), &id);
        let invalid_value = diagnostics
            .iter()
            .find(|item| item.code == DiagnosticCode::InvalidValue)
            .expect("invalid CWT value diagnostic");
        assert_eq!(invalid_value.severity, 2);
    }

    #[test]
    fn cwt_matcher_enforces_max_cardinality() {
        let (host, id) = cwt_snapshot("trigger = { foo = yes foo = no }\n");
        let results = diagnostics(&host.snapshot(), &id);
        assert!(results.iter().any(|item| item.code == DiagnosticCode::Cardinality));
    }

    #[test]
    fn cwt_matcher_enforces_min_cardinality() {
        let (host, id) = cwt_snapshot_with_constraints("trigger = { }\n", None, Some(1), Some(1));
        assert!(
            diagnostics(&host.snapshot(), &id)
                .iter()
                .any(|item| item.code == DiagnosticCode::Cardinality)
        );
    }

    #[test]
    fn cwt_value_clause_validates_bare_values_and_cardinality() {
        let mut model = RulesModel::bootstrap();
        model.cwt.rules.push(CwtSemanticRule {
            id: "fixture:terrain:color".to_owned(),
            context: "terrain".to_owned(),
            parent_path: Vec::new(),
            key: CwtKeyMatcher::Exact("color".to_owned()),
            operator: Some("=".to_owned()),
            value: CwtValueMatcher::AnyScalar,
            shape: CwtRuleShape::ValueClause,
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
            source_file: "fixture.cwt".to_owned(),
            line: 1,
        });
        model.cwt.rules.push(CwtSemanticRule {
            id: "fixture:terrain:color:int".to_owned(),
            context: "terrain".to_owned(),
            parent_path: vec!["color".to_owned()],
            key: CwtKeyMatcher::AnyScalar,
            operator: None,
            value: CwtValueMatcher::Int { min: Some(0), max: Some(255) },
            shape: CwtRuleShape::LeafValue,
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
            source_file: "fixture.cwt".to_owned(),
            line: 2,
        });
        let mut host = AnalysisHost::new(Eu4Rules::from_model(model));
        let id = DocumentId::new("file:///tmp/common/terrain/test.txt");
        host.open_document(id.clone(), 1, "terrain = { color = { 1 2 300 } }\n".to_owned(), None)
            .expect("open");
        let diagnostics = diagnostics(&host.snapshot(), &id);
        assert!(diagnostics.iter().any(|item| item.code == DiagnosticCode::InvalidValue));
        assert!(diagnostics.iter().any(|item| item.code == DiagnosticCode::Cardinality));
    }

    #[test]
    fn cwt_rules_drive_value_completion_and_hover() {
        let (host, id) = cwt_snapshot("trigger = { foo = yes }\n");
        let snapshot = host.snapshot();
        let value = u32::try_from("trigger = { foo = ".len()).expect("offset");
        let result = complete(&snapshot, &id, value);
        assert!(result.items.iter().any(|item| item.label == "yes"));
        let hover = hover(&snapshot, &id, 18).expect("CWT hover");
        assert!(hover.contents.contains("foo") || hover.contents.contains("EU4"));
    }

    #[test]
    fn committed_cwt_artifact_drives_runtime_value_diagnostics() {
        let rules_path =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../rules/eu4.pdxrules");
        let rules = Eu4Rules::load(&rules_path).expect("load committed CWT artifact");
        assert!(!rules.model().cwt.rules.is_empty());
        assert!(rules.model().cwt.rules.iter().any(|rule| rule.severity == Some(2)));
        assert!(rules.model().cwt.rules.iter().any(|rule| rule.min_occurs == Some(1)));
        let mut host = AnalysisHost::new(rules);
        let id = DocumentId::new("file:///tmp/common/events/test.txt");
        host.open_document(
            id.clone(),
            1,
            "trigger = { ai = maybe definitely_not_a_trigger = yes }\n".to_owned(),
            None,
        )
        .expect("open");
        let diagnostics = diagnostics(&host.snapshot(), &id);
        assert!(diagnostics.iter().any(|item| item.code == DiagnosticCode::InvalidValue));
        assert!(diagnostics.iter().any(|item| item.code == DiagnosticCode::UnknownKey));
    }

    #[test]
    fn cwt_type_selector_applies_event_rules_to_country_event() {
        let rules_path =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../rules/eu4.pdxrules");
        let rules = Eu4Rules::load(&rules_path).expect("load committed CWT artifact");
        let mut host = AnalysisHost::new(rules);
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
    fn eu4_starts_with_type_selector_applies_on_action_rules() {
        let rules_path =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../rules/eu4.pdxrules");
        let rules = Eu4Rules::load(&rules_path).expect("load committed CWT artifact");
        let mut host = AnalysisHost::new(rules);
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
    fn eu4_scope_links_switch_effect_context_and_scope() {
        let rules_path =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../rules/eu4.pdxrules");
        let rules = Eu4Rules::load(&rules_path).expect("load committed CWT artifact");
        let mut host = AnalysisHost::new(rules);
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
    fn eu4_alias_alternatives_do_not_cross_report_cardinality() {
        let rules_path =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../rules/eu4.pdxrules");
        let rules = Eu4Rules::load(&rules_path).expect("load committed CWT artifact");
        let mut host = AnalysisHost::new(rules);
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
            !results.iter().any(|item| item.code == DiagnosticCode::Cardinality),
            "unexpected diagnostics: {results:?}"
        );
    }

    #[test]
    fn eu4_common_links_allow_owner_to_push_province_scope_to_country() {
        let rules_path =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../rules/eu4.pdxrules");
        let rules = Eu4Rules::load(&rules_path).expect("load committed CWT artifact");
        let mut host = AnalysisHost::new(rules);
        let id = DocumentId::new("file:///tmp/events/owner.txt");
        host.open_document(
            id.clone(),
            1,
            "province_event = { immediate = { owner = { add_treasury = nope } } }\n".to_owned(),
            None,
        )
        .expect("open");
        let results = diagnostics(&host.snapshot(), &id);
        assert!(results.iter().any(|item| item.code == DiagnosticCode::InvalidValue));
        assert!(!results.iter().any(|item| item.code == DiagnosticCode::UnknownKey));
    }

    #[test]
    fn eu4_dynamic_culture_definition_is_used_by_cwt_type_matcher() {
        use pdx_workspace::{SourceRoot, SourceRootId, SourceRootKind, WorkspaceChange};
        use std::fs;

        let root =
            std::env::temp_dir().join(format!("pdx-analysis-cwt-dynamic-{}", std::process::id()));
        fs::create_dir_all(root.join("common/cultures")).expect("culture directory");
        fs::write(root.join("common/cultures/00_test.txt"), "french = { }\n")
            .expect("culture definition");
        let rules_path =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../rules/eu4.pdxrules");
        let rules = Eu4Rules::load(&rules_path).expect("load committed CWT artifact");
        let mut host = AnalysisHost::new(rules);
        host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot {
            id: SourceRootId::new(1),
            kind: SourceRootKind::CurrentMod,
            path: root.clone(),
            order: 0,
            writable: true,
        }]));
        host.refresh_source_roots().expect("scan culture definition");
        let id = DocumentId::new("file:///tmp/events/culture.txt");
        host.open_document(
            id.clone(),
            1,
            "country_event = { trigger = { culture = french } }\n".to_owned(),
            None,
        )
        .expect("open");
        let results = diagnostics(&host.snapshot(), &id);
        assert!(results.iter().all(|item| item.code != DiagnosticCode::InvalidValue));
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn eu4_province_definition_csv_feeds_province_id_matcher() {
        use pdx_workspace::{SourceRoot, SourceRootId, SourceRootKind, WorkspaceChange};
        use std::fs;

        let root =
            std::env::temp_dir().join(format!("pdx-analysis-cwt-province-{}", std::process::id()));
        fs::create_dir_all(root.join("map")).expect("map directory");
        fs::write(root.join("map/definition.csv"), "1;0;0;0;0;0\n").expect("province definition");
        let rules_path =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../rules/eu4.pdxrules");
        let rules = Eu4Rules::load(&rules_path).expect("load committed CWT artifact");
        let mut host = AnalysisHost::new(rules);
        host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot {
            id: SourceRootId::new(1),
            kind: SourceRootKind::CurrentMod,
            path: root.clone(),
            order: 0,
            writable: true,
        }]));
        host.refresh_source_roots().expect("scan province definition");
        let id = DocumentId::new("file:///tmp/events/province.txt");
        host.open_document(
            id.clone(),
            1,
            "country_event = { trigger = { capital = 1 } }\n".to_owned(),
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
    fn eu4_country_tag_definition_feeds_dynamic_enum_matcher() {
        use pdx_workspace::{SourceRoot, SourceRootId, SourceRootKind, WorkspaceChange};
        use std::fs;

        let root =
            std::env::temp_dir().join(format!("pdx-analysis-cwt-tags-{}", std::process::id()));
        fs::create_dir_all(root.join("common/country_tags")).expect("country tag directory");
        fs::write(
            root.join("common/country_tags/00_test.txt"),
            "countries = { FRA = \"countries/France.txt\" }\n",
        )
        .expect("country tag definition");
        let rules_path =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../rules/eu4.pdxrules");
        let rules = Eu4Rules::load(&rules_path).expect("load committed CWT artifact");
        let mut host = AnalysisHost::new(rules);
        host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot {
            id: SourceRootId::new(1),
            kind: SourceRootKind::CurrentMod,
            path: root.clone(),
            order: 0,
            writable: true,
        }]));
        host.refresh_source_roots().expect("scan country tag definition");
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
        use pdx_workspace::{SourceRoot, SourceRootId, SourceRootKind, WorkspaceChange};
        use std::fs;

        let root =
            std::env::temp_dir().join(format!("pdx-analysis-cwt-flags-{}", std::process::id()));
        fs::create_dir_all(root.join("events")).expect("event directory");
        fs::write(
            root.join("events/00_flags.txt"),
            "country_event = { immediate = { set_country_flag = known_flag } }\n",
        )
        .expect("flag definition");
        let rules_path =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../rules/eu4.pdxrules");
        let rules = Eu4Rules::load(&rules_path).expect("load committed CWT artifact");
        let mut host = AnalysisHost::new(rules);
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
    fn eu4_scripted_effect_params_are_dynamic_enum_members() {
        use pdx_workspace::{SourceRoot, SourceRootId, SourceRootKind, WorkspaceChange};
        use std::fs;

        let root =
            std::env::temp_dir().join(format!("pdx-analysis-cwt-params-{}", std::process::id()));
        fs::create_dir_all(root.join("common/scripted_effects"))
            .expect("scripted effect directory");
        fs::write(
            root.join("common/scripted_effects/00_test.txt"),
            "apply = { value = $amount$ }\n",
        )
        .expect("scripted effect definition");
        let rules_path =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../rules/eu4.pdxrules");
        let rules = Eu4Rules::load(&rules_path).expect("load committed CWT artifact");
        let mut host = AnalysisHost::new(rules);
        host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot {
            id: SourceRootId::new(1),
            kind: SourceRootKind::CurrentMod,
            path: root.clone(),
            order: 0,
            writable: true,
        }]));
        host.refresh_source_roots().expect("scan scripted effect definition");
        let id = DocumentId::new("file:///tmp/events/params.txt");
        host.open_document(
            id.clone(),
            1,
            "country_event = { immediate = { apply = { amount = 1 } } }\n".to_owned(),
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
    fn eu4_legacy_governments_use_eu4_reform_semantics() {
        use pdx_workspace::{SourceRoot, SourceRootId, SourceRootKind, WorkspaceChange};
        use std::fs;

        let root =
            std::env::temp_dir().join(format!("pdx-analysis-cwt-legacy-{}", std::process::id()));
        fs::create_dir_all(root.join("common/government_reforms")).expect("reform directory");
        fs::write(
            root.join("common/government_reforms/00_test.txt"),
            "reform_a = { legacy_government = yes }\n",
        )
        .expect("legacy reform definition");
        let rules_path =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../rules/eu4.pdxrules");
        let rules = Eu4Rules::load(&rules_path).expect("load committed CWT artifact");
        let mut host = AnalysisHost::new(rules);
        host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot {
            id: SourceRootId::new(1),
            kind: SourceRootKind::CurrentMod,
            path: root.clone(),
            order: 0,
            writable: true,
        }]));
        host.refresh_source_roots().expect("scan legacy reform definition");
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
        let mut host = AnalysisHost::new(Eu4Rules::bootstrap());
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
    fn navigation_and_hover_use_local_event_definition() {
        let text = "country_event = { id = test.1 }\nevent = test.1\n";
        let (host, id) = snapshot(text);
        let snapshot = host.snapshot();
        let symbols = document_symbols(&snapshot, &id);
        assert_eq!(symbols.len(), 1);
        let definition_location = definition(&snapshot, &id, 40);
        assert_eq!(definition_location.len(), 1);
        assert!(hover(&snapshot, &id, 40).is_some());
        assert_eq!(references(&snapshot, &id, 40, true).len(), 2);
        assert!(!workspace_symbols(&snapshot, "test").is_empty());
        assert!(TextRange::new(0, 1).is_some());
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
        host.apply_document_changes(&id, 2, &[pdx_workspace::TextChange::full(changed)])
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
        use pdx_workspace::{SourceRoot, SourceRootId, SourceRootKind, WorkspaceChange};
        use std::fs;

        let nonce = std::process::id();
        let root = std::env::temp_dir().join(format!("pdx-analysis-rename-{nonce}"));
        let dependency = root.join("dependency/common/events");
        fs::create_dir_all(&dependency).expect("dependency directory");
        let path = dependency.join("events.txt");
        fs::write(&path, "country_event = { id = read_only.1 }\n").expect("dependency event");

        let mut host = AnalysisHost::new(Eu4Rules::bootstrap());
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
