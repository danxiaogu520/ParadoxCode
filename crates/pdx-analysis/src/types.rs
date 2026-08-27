use std::sync::Arc;
#[cfg(test)]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicBool, Ordering};

use pdx_engine::hir::Scope;
use pdx_engine::{DocumentId, SourceFileId};
use pdx_parser::FileFormat;
use pdx_text::{LogicalPath, TextRange};

/// Shared cooperative-cancellation state for editor-neutral analysis queries.
///
/// Clones observe the same flag. Query implementations check it while traversing workspace and
/// semantic rule data so protocol adapters can stop obsolete work without introducing protocol types here.
#[derive(Clone, Debug)]
pub struct CancellationToken {
    pub(crate) cancelled: Arc<AtomicBool>,
    #[cfg(test)]
    pub(crate) remaining_checkpoints: Arc<AtomicUsize>,
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

    pub(crate) fn checkpoint(&self) -> Result<(), Cancelled> {
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
    pub(crate) fn cancel_after(checkpoints: usize) -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            remaining_checkpoints: Arc::new(AtomicUsize::new(checkpoints)),
        }
    }
}

/// Marker returned when a cooperative analysis query stops early.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Cancelled;

pub(crate) fn uncancelled<T>(result: Result<T, Cancelled>) -> T {
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
    /// A bare scalar has no matching rule value, rather than merely having the wrong shape.
    UnknownBareValue,
    /// A semantic rule cardinality constraint was violated.
    Cardinality,
    /// A key or value is known to the semantic rule set but is used from the wrong game scope.
    RuleWrongScope,
    /// A target expression is syntactically valid but cannot be resolved to a target.
    InvalidTarget,
    /// A target resolves, but its scope is incompatible with the rule's expected scope.
    TargetWrongScope,
    /// A scope-changing command is not valid in the current scope context.
    InvalidScopeCommand,
    /// Recursive scripted-macro expansion reached a definition already on the active stack.
    MacroExpansionCycle,
    /// Scripted-macro expansion exceeded a bounded work or size limit.
    AnalysisIncomplete,
}

impl DiagnosticCode {
    /// Every diagnostic category emitted by the analysis layer, in stable wire order.
    pub const ALL: &'static [Self] = &[
        Self::Syntax,
        Self::UnknownKey,
        Self::UnknownSymbol,
        Self::AmbiguousSymbol,
        Self::UnknownScope,
        Self::InvalidValue,
        Self::UnknownBareValue,
        Self::Cardinality,
        Self::RuleWrongScope,
        Self::InvalidTarget,
        Self::TargetWrongScope,
        Self::InvalidScopeCommand,
        Self::MacroExpansionCycle,
        Self::AnalysisIncomplete,
    ];

    /// Parses a wire-facing diagnostic category.
    #[must_use]
    pub fn parse_name(value: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|code| code.as_str().eq_ignore_ascii_case(value))
    }

    /// Returns the stable wire-facing code.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Syntax => "SyntaxError",
            Self::UnknownKey => "UnknownKey",
            Self::UnknownSymbol => "UnknownSymbol",
            Self::AmbiguousSymbol => "AmbiguousSymbol",
            Self::UnknownScope => "UnknownScope",
            Self::InvalidValue => "InvalidValue",
            Self::UnknownBareValue => "UnknownBareValue",
            Self::Cardinality => "Cardinality",
            Self::RuleWrongScope => "RuleWrongScope",
            Self::InvalidTarget => "InvalidTarget",
            Self::TargetWrongScope => "TargetWrongScope",
            Self::InvalidScopeCommand => "InvalidScopeCommand",
            Self::MacroExpansionCycle => "MacroExpansionCycle",
            Self::AnalysisIncomplete => "AnalysisIncomplete",
        }
    }

    /// Returns the default severity for this category.
    #[must_use]
    pub const fn severity(self) -> Severity {
        match self {
            // An unknown key is silently ignored by the game, so the authored line is
            // ineffective code; it is an error once the surrounding context is known.
            Self::UnknownKey
            | Self::UnknownBareValue
            | Self::InvalidTarget
            | Self::TargetWrongScope
            | Self::InvalidScopeCommand => Severity::Error,
            // Expansion limits are analysis-side work bounds; reaching one means this
            // file was not fully validated, not that the file itself is wrong.
            Self::AnalysisIncomplete => Severity::Information,
            Self::Syntax
            | Self::UnknownSymbol
            | Self::AmbiguousSymbol
            | Self::UnknownScope
            | Self::InvalidValue
            | Self::Cardinality
            | Self::RuleWrongScope
            | Self::MacroExpansionCycle => Severity::Error,
        }
    }
}

/// Severity used by editor-neutral diagnostics.
///
/// Keeping this as a domain type prevents rule/compiler integer values from leaking through the
/// analysis API. Conversion to the LSP's numeric representation happens in `pdx-lsp`.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Severity {
    Error,
    Warning,
    Information,
    Hint,
}

impl Severity {
    /// Converts to the LSP diagnostic severity numbering.
    #[must_use]
    pub const fn lsp_number(self) -> u8 {
        match self {
            Self::Error => 1,
            Self::Warning => 2,
            Self::Information => 3,
            Self::Hint => 4,
        }
    }

    /// Converts a validated rule severity (1..=3), conservatively treating unknown values as
    /// errors at the analysis boundary.
    #[must_use]
    pub const fn from_rule_number(value: u8) -> Self {
        match value {
            2 => Self::Warning,
            3 => Self::Information,
            4 => Self::Hint,
            _ => Self::Error,
        }
    }

    /// Downgrades a diagnostic by one level, preserving the information/hint ceiling.
    #[must_use]
    pub const fn saturating_add(self, amount: u8) -> Self {
        let mut number = self.lsp_number();
        let mut remaining = amount;
        while remaining > 0 && number < 4 {
            number += 1;
            remaining -= 1;
        }
        match number {
            2 => Self::Warning,
            3 => Self::Information,
            4 => Self::Hint,
            _ => Self::Error,
        }
    }
}

impl PartialEq<u8> for Severity {
    fn eq(&self, other: &u8) -> bool {
        self.lsp_number() == *other
    }
}

impl PartialEq<Severity> for u8 {
    fn eq(&self, other: &Severity) -> bool {
        *self == other.lsp_number()
    }
}

/// Confidence in the diagnostic's semantic conclusion.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum DiagnosticCertainty {
    /// The parser/rule set proves the issue for the current context.
    Certain,
    /// The conclusion depends on a known but context-sensitive scope transition.
    Contextual,
    /// The conclusion was inferred from a fallback, path, or dynamic symbol.
    Inferred,
    /// Analysis could not establish enough information to decide.
    Unresolved,
}

impl DiagnosticCertainty {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Certain => "certain",
            Self::Contextual => "contextual",
            Self::Inferred => "inferred",
            Self::Unresolved => "unresolved",
        }
    }
}

/// Structured provenance attached to a semantic diagnostic.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiagnosticProvenance {
    pub rule_id: Option<String>,
    pub context: Option<String>,
    pub source_file: Option<String>,
    pub source_line: Option<u32>,
}

impl DiagnosticProvenance {
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            rule_id: None,
            context: None,
            source_file: None,
            source_line: None,
        }
    }
}

/// An editor-neutral diagnostic.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Diagnostic {
    /// Stable category.
    pub code: DiagnosticCode,
    /// Domain severity. Protocol adapters convert this to their wire representation.
    pub severity: Severity,
    /// Source range.
    pub range: TextRange,
    /// Human-readable message.
    pub message: String,
    /// Confidence in the conclusion, independent of severity.
    pub certainty: DiagnosticCertainty,
    /// Optional internal rule/source provenance for explainability.
    pub provenance: Option<DiagnosticProvenance>,
}

impl Diagnostic {
    /// Creates a diagnostic with conservative, certain defaults.
    #[must_use]
    pub fn new(
        code: DiagnosticCode,
        severity: Severity,
        range: TextRange,
        message: String,
    ) -> Self {
        Self {
            code,
            severity,
            range,
            message,
            certainty: DiagnosticCertainty::Certain,
            provenance: None,
        }
    }

    /// Sets the confidence independently from the severity.
    #[must_use]
    pub const fn with_certainty(mut self, certainty: DiagnosticCertainty) -> Self {
        self.certainty = certainty;
        self
    }

    /// Attaches structured provenance.
    #[must_use]
    pub fn with_provenance(mut self, provenance: DiagnosticProvenance) -> Self {
        self.provenance = Some(provenance);
        self
    }
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
    /// Opaque token used by `completionItem/resolve` to re-derive documentation on demand.
    pub resolve_data: Option<String>,
}

/// Broad completion item categories independent of LSP enum values.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum CompletionKind {
    /// A property or structural key.
    Key,
    /// A script command or trigger predicate.
    Command,
    /// A workspace-defined `scripted_effect` or `scripted_trigger` macro.
    ScriptedMacro,
    /// A scalar value without a more specific semantic category.
    Value,
    /// A member of a statically declared enum.
    EnumMember,
    /// A scope expression such as `this`, `root`, or a scope link.
    Scope,
    /// A symbol from the workspace index.
    Symbol,
    /// A localisation key.
    Localisation,
    /// A parameter declared by a scripted macro.
    MacroParameter,
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

/// One source-ranged semantic token produced by the editor-neutral highlighting query.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SemanticToken {
    /// Source range covered by the token.
    pub range: TextRange,
    /// Stable classification used by the protocol legend.
    pub token_type: SemanticTokenType,
    /// Whether this token introduces a local definition (an `@name` variable in key position).
    pub definition: bool,
}

/// Stable semantic token types. The ordering of [`Self::ALL`] is the protocol legend contract:
/// protocol adapters must preserve it and must not invent additional types.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum SemanticTokenType {
    /// A line comment.
    Comment,
    /// A quoted or bare scalar value.
    String,
    /// A numeric scalar.
    Number,
    /// A `yes`/`no` scalar.
    Boolean,
    /// A script operator such as `=` or `==`.
    Operator,
    /// A property key that is not a rule-known key.
    Property,
    /// A property key known to the rule database or profile.
    Function,
    /// A control-flow key such as `if`, `limit`, or `not`.
    Keyword,
    /// A header-block header such as `rgb { … }`.
    Type,
    /// An `@name` scripted variable.
    Variable,
    /// A `$name$` parameter or a parameter-block condition.
    Parameter,
}

impl SemanticTokenType {
    /// Legend order; clients receive indices into this list.
    pub const ALL: [Self; 11] = [
        Self::Comment,
        Self::String,
        Self::Number,
        Self::Boolean,
        Self::Operator,
        Self::Property,
        Self::Function,
        Self::Keyword,
        Self::Type,
        Self::Variable,
        Self::Parameter,
    ];

    /// Returns the stable wire-facing legend name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Comment => "comment",
            Self::String => "string",
            Self::Number => "number",
            Self::Boolean => "boolean",
            Self::Operator => "operator",
            Self::Property => "property",
            Self::Function => "function",
            Self::Keyword => "keyword",
            Self::Type => "type",
            Self::Variable => "variable",
            Self::Parameter => "parameter",
        }
    }
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
