pub(crate) use crate::{
    CancellationToken, Cancelled, CompletionKind, DiagnosticCode, RenameError, RenameFailure,
    complete, complete_with_cancellation, definition, diagnostics, diagnostics_with_cancellation,
    document_symbols, hover, input_for_document, prepare_rename, references, rename,
    rename_with_cancellation, scope_inlay_hints_with_cancellation, semantic_completion_context,
    semantic_root_context, workspace_symbols, workspace_symbols_with_cancellation,
};
pub(crate) use engine::{
    AnalysisHost, AnalysisSnapshot, DocumentId, IndexCache, SourceRoot, SourceRootId,
    SourceRootKind, WorkspaceChange,
};
pub(crate) use rules::{
    KeyMatcher, ProfileDefinitionRule, ProfileMatchMode, ProfileTextMatcher, RuleSet, RuleShape,
    SemanticRule, ValueMatcher,
};
pub(crate) use text::{LogicalPath, TextRange};

pub(crate) fn eu4_host(rules: RuleSet) -> AnalysisHost {
    AnalysisHost::with_profile(rules, game::eu4::profile())
}

/// Creates an isolated fixture root under the system temp directory. Cleanup
/// stays with the caller (`fs::remove_dir_all`), matching the existing tests.
pub(crate) fn temp_root(tag: &str) -> std::path::PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("ide-{tag}-{nonce}"));
    std::fs::create_dir_all(&root).expect("fixture root");
    root
}

/// A fully-defaulted [`SemanticRule`] fixture: exact `key` match in `context`
/// with a derived `fixture:<context>:<key>` id. Tests override the differing
/// fields with struct-update syntax instead of repeating all 21 fields.
pub(crate) fn semantic_rule(context: &str, key: &str) -> SemanticRule {
    SemanticRule {
        id: format!("fixture:{context}:{key}"),
        context: context.to_owned(),
        parent_path: Vec::new(),
        key: KeyMatcher::Exact(key.to_owned()),
        operator: None,
        value: ValueMatcher::AnyScalar,
        shape: RuleShape::Leaf,
        child_context: None,
        alternative_id: None,
        severity: None,
        required: false,
        deprecated: false,
        documentation: Vec::new(),
        allowed_scopes: Vec::new(),
        push_scope: None,
        replace_scope: Vec::new(),
        min_occurs: None,
        strict_min: true,
        max_occurs: None,
        source_file: "fixture.semantic".to_owned(),
        line: 1,
    }
}

pub(crate) fn snapshot(text: &str) -> (AnalysisHost, DocumentId) {
    let mut host = eu4_host(game::eu4::bootstrap_rules());
    let id = DocumentId::new("file:///tmp/common/events/test.txt");
    host.open_document(id.clone(), 1, text.to_owned(), None)
        .expect("open");
    (host, id)
}

pub(crate) fn semantic_snapshot(text: &str) -> (AnalysisHost, DocumentId) {
    semantic_snapshot_with_constraints(text, None, None, Some(1))
}

pub(crate) fn semantic_snapshot_with_severity(
    text: &str,
    severity: Option<u8>,
) -> (AnalysisHost, DocumentId) {
    semantic_snapshot_with_constraints(text, severity, None, Some(1))
}

pub(crate) fn semantic_snapshot_with_constraints(
    text: &str,
    severity: Option<u8>,
    min_occurs: Option<u32>,
    max_occurs: Option<u32>,
) -> (AnalysisHost, DocumentId) {
    let mut model = game::eu4::bootstrap_model();
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
        deprecated: false,
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

pub(crate) fn quoted_script_snapshot(text: &str) -> (AnalysisHost, DocumentId) {
    let mut model = game::eu4::bootstrap_model();
    model.semantic.rules.extend([
        SemanticRule {
            operator: Some("=".to_owned()),
            shape: RuleShape::QuotedScript,
            child_context: Some("trigger".to_owned()),
            documentation: vec!["Embedded trigger Script".to_owned()],
            line: 2,
            ..semantic_rule("trigger", "embedded")
        },
        SemanticRule {
            operator: Some("=".to_owned()),
            shape: RuleShape::QuotedScript,
            child_context: Some("trigger".to_owned()),
            line: 3,
            ..semantic_rule("trigger", "nested")
        },
        SemanticRule {
            id: "fixture:trigger:foo-quoted-child".to_owned(),
            operator: Some("=".to_owned()),
            value: ValueMatcher::Bool,
            line: 4,
            ..semantic_rule("trigger", "foo")
        },
    ]);
    let mut host = eu4_host(RuleSet::from_model(model));
    let id = DocumentId::new("file:///tmp/common/events/quoted-script.txt");
    host.open_document(id.clone(), 1, text.to_owned(), None)
        .expect("open");
    (host, id)
}
