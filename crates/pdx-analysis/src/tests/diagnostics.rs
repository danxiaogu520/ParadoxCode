#![allow(unused_imports)]

use super::support::*;

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
fn semantic_diagnostics_do_not_materialize_the_full_workspace() {
    let (host, id) = snapshot(
        "country_event = { id = direct.1 title = missing_title immediate = { always = yes } }\n",
    );
    let snapshot = host.snapshot();

    crate::ALL_SEMANTICS_CALLS.with(|calls| calls.set(0));
    let results = diagnostics(&snapshot, &id);

    assert!(
        results
            .iter()
            .any(|item| item.code == DiagnosticCode::UnknownSymbol)
    );
    crate::ALL_SEMANTICS_CALLS.with(|calls| {
        assert_eq!(
            calls.get(),
            0,
            "document diagnostics must query symbol buckets instead of cloning the workspace"
        );
    });
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
fn semantic_matcher_enforces_min_cardinality() {
    let (host, id) = semantic_snapshot_with_constraints("trigger = { }\n", None, Some(1), Some(1));
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
        deprecated: false,
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
        deprecated: false,
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
