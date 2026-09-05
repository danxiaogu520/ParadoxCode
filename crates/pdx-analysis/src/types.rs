use std::sync::Arc;
#[cfg(test)]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicBool, Ordering};

use pdx_engine::hir::Scope;
use pdx_engine::{DocumentId, SourceFileId};
use pdx_parser::FileFormat;
use pdx_text::{LogicalPath, TextRange, TextSize};

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
    /// A localisation key reference has no definition in the workspace or
    /// vanilla index.
    UnknownLocalisationKey,
    /// A definition shadows an earlier definition of the same name; later
    /// definitions win at load time.
    AmbiguousDefinition,
    /// A scalar or block does not satisfy the selected semantic matcher,
    /// including unrecognised target and scope-name values.
    InvalidValue,
    /// A semantic rule cardinality constraint was violated.
    Cardinality,
    /// A key or value is known to the semantic rule set but is used from the
    /// wrong game scope, including a dynamic definition called outside its
    /// inferred entry contract.
    WrongScope,
    /// Recursive dynamic-definition expansion reached a definition already on the active stack.
    DynamicDefinitionCycle,
    /// A mission-tree dependency is illegal: missing target, a cycle, or a
    /// required mission whose `[slot, position]` cannot precede its dependent.
    InvalidDependency,
    /// A boolean logic container (AND/OR/NOT) has a degenerate or misleading shape.
    LogicalContainer,
    /// A condition evaluates to a statically known truth value.
    ConstantCondition,
    /// An effect-side `if`/`else_if` block lacks the `limit` that carries its condition.
    MissingLimit,
    /// A conditional block has an empty body.
    EmptyBlock,
    /// An `else`/`else_if` block has no preceding `if`/`else_if` sibling.
    OrphanElse,
    /// A dynamic definition's inferred entry-scope contract is empty, so no scope
    /// can run its body correctly.
    EmptyScopeContract,
    /// An effect applies a typed definition whose scope-attributed attributes
    /// cannot act in the effect's scope.
    ModifierScopeMismatch,
}

impl DiagnosticCode {
    /// Every diagnostic category emitted by the analysis layer, in stable wire order.
    pub const ALL: &'static [Self] = &[
        Self::Syntax,
        Self::UnknownKey,
        Self::UnknownLocalisationKey,
        Self::AmbiguousDefinition,
        Self::InvalidValue,
        Self::Cardinality,
        Self::WrongScope,
        Self::DynamicDefinitionCycle,
        Self::InvalidDependency,
        Self::LogicalContainer,
        Self::ConstantCondition,
        Self::MissingLimit,
        Self::EmptyBlock,
        Self::OrphanElse,
        Self::EmptyScopeContract,
        Self::ModifierScopeMismatch,
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
            Self::UnknownLocalisationKey => "UnknownLocalisationKey",
            Self::AmbiguousDefinition => "AmbiguousDefinition",
            Self::InvalidValue => "InvalidValue",
            Self::Cardinality => "Cardinality",
            Self::WrongScope => "WrongScope",
            Self::DynamicDefinitionCycle => "DynamicDefinitionCycle",
            Self::InvalidDependency => "InvalidDependency",
            Self::LogicalContainer => "LogicalContainer",
            Self::ConstantCondition => "ConstantCondition",
            Self::MissingLimit => "MissingLimit",
            Self::EmptyBlock => "EmptyBlock",
            Self::OrphanElse => "OrphanElse",
            Self::EmptyScopeContract => "EmptyScopeContract",
            Self::ModifierScopeMismatch => "ModifierScopeMismatch",
        }
    }

    /// Returns the default severity for this category.
    #[must_use]
    pub const fn severity(self) -> Severity {
        match self {
            // An unknown key is silently ignored by the game, so the authored line is
            // ineffective code; it is an error once the surrounding context is known.
            Self::UnknownKey => Severity::Error,
            // The game renders a missing localisation key as its raw spelling, so a
            // missing key is a data-quality warning rather than a script error.
            Self::UnknownLocalisationKey => Severity::Warning,
            // The game resolves same-name definitions deterministically by load
            // priority, so a shadowing definition is a warning at the definition.
            Self::AmbiguousDefinition => Severity::Warning,
            // Structural lints describe valid-but-misleading script shapes; the game
            // still loads them, so they stay below error severity except for control
            // flow that cannot match any branch chain.
            Self::LogicalContainer
            | Self::ConstantCondition
            | Self::MissingLimit
            | Self::EmptyBlock => Severity::Warning,
            Self::Syntax
            | Self::InvalidValue
            | Self::Cardinality
            | Self::WrongScope
            | Self::DynamicDefinitionCycle
            // An illegal mission dependency never loads the way the author intends.
            | Self::InvalidDependency
            // An empty contract means the definition is unusable in every
            // scope; rejecting it at the definition follows the same Rust
            // principle as rejecting a recursive expansion.
            | Self::EmptyScopeContract
            | Self::OrphanElse => Severity::Error,
            // The game still loads cross-class modifier applications, so the
            // scope class of the applied attributes is recorded as information
            // rather than rejected.
            Self::ModifierScopeMismatch => Severity::Information,
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

/// A safe, editor-neutral source edit suggested by a diagnostic.
///
/// The analysis layer owns the semantic decision and only returns a bounded, explicit
/// replacement. Protocol adapters convert the UTF-8 range to the editor's native position
/// representation and never infer edits from the diagnostic message.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QuickFix {
    /// Short action title shown by the editor.
    pub title: String,
    /// Source range to replace.
    pub range: TextRange,
    /// Replacement text, already escaped for the source language.
    pub new_text: String,
    /// Whether this is the single confident resolution, mapped to the
    /// protocol's preferred-action flag.
    pub preferred: bool,
}

impl QuickFix {
    /// Creates a replacement edit.
    #[must_use]
    pub fn replace(title: String, range: TextRange, new_text: String) -> Self {
        Self {
            title,
            range,
            new_text,
            preferred: false,
        }
    }

    /// Creates a replacement edit that is the confident resolution of its
    /// diagnostic, such as a did-you-mean spelling correction.
    #[must_use]
    pub fn suggestion(title: String, range: TextRange, new_text: String) -> Self {
        Self {
            preferred: true,
            ..Self::replace(title, range, new_text)
        }
    }
}

/// Editor rendering hints, mirroring the protocol's diagnostic tags.
///
/// Tags change presentation only (strikethrough, dimming); they never
/// alter severity or suppress a finding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticTag {
    /// The flagged source is redundant, such as an occurrence past a quota.
    Unnecessary,
    /// The flagged usage belongs to a deprecated declaration.
    Deprecated,
}

impl DiagnosticTag {
    /// Returns the stable wire number (LSP tag values).
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        match self {
            Self::Unnecessary => 1,
            Self::Deprecated => 2,
        }
    }
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
    /// Secondary explanation lines; protocol adapters append these to the
    /// message as additional lines.
    pub notes: Vec<String>,
    /// The constraint the source violated, phrased for users and shared with
    /// hover labels (for example "a country scope").
    pub expected: Option<String>,
    /// Locations that give the finding context, such as the earlier
    /// definition a later one shadows.
    pub related: Vec<RelatedLocation>,
    /// Editor rendering hints (redundant or deprecated source).
    pub tags: Vec<DiagnosticTag>,
    /// Optional internal rule/source provenance for explainability.
    pub provenance: Option<DiagnosticProvenance>,
    /// Safe source edits that directly address this diagnostic.
    pub fixes: Vec<QuickFix>,
}

/// One contextual location attached to a diagnostic.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RelatedLocation {
    /// Where the related symbol or token lives.
    pub location: Location,
    /// Why the location matters, such as "earlier definition".
    pub message: String,
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
            notes: Vec::new(),
            expected: None,
            related: Vec::new(),
            tags: Vec::new(),
            provenance: None,
            fixes: Vec::new(),
        }
    }

    /// Sets the confidence independently from the severity.
    #[must_use]
    pub const fn with_certainty(mut self, certainty: DiagnosticCertainty) -> Self {
        self.certainty = certainty;
        self
    }

    /// Appends one secondary explanation line.
    #[must_use]
    pub fn with_note(mut self, note: String) -> Self {
        self.notes.push(note);
        self
    }

    /// States the constraint the source violated.
    #[must_use]
    pub fn with_expected(mut self, expected: String) -> Self {
        self.expected = Some(expected);
        self
    }

    /// Attaches one contextual location.
    #[must_use]
    pub fn with_related(mut self, related: RelatedLocation) -> Self {
        self.related.push(related);
        self
    }

    /// Attaches one editor rendering hint.
    #[must_use]
    pub fn with_tag(mut self, tag: DiagnosticTag) -> Self {
        self.tags.push(tag);
        self
    }

    /// Attaches structured provenance.
    #[must_use]
    pub fn with_provenance(mut self, provenance: DiagnosticProvenance) -> Self {
        self.provenance = Some(provenance);
        self
    }

    /// Attaches one safe source edit to this diagnostic.
    #[must_use]
    pub fn with_fix(mut self, fix: QuickFix) -> Self {
        self.fixes.push(fix);
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
    /// A workspace-defined `scripted_effect` or `scripted_trigger` definition.
    DynamicDefinition,
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
    /// A parameter declared by a dynamic definition.
    DynamicParameter,
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

/// One rule-proven scope transition suitable for an editor inlay hint. The position is the byte
/// offset at the beginning of the block value (`key = { ... }`); protocol adapters convert it to
/// the client's position encoding and choose presentation details.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScopeInlayHint {
    /// Byte offset at which the hint should be inserted.
    pub position: TextSize,
    /// Concrete scope name selected by the rule transition.
    pub scope: String,
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
