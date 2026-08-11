#![allow(unused_imports)]

use super::support::*;

#[test]
fn quoted_script_diagnostics_reuse_semantic_validation_with_exact_ranges() {
    let text = "trigger = { embedded = \"\n foo = maybe\n unknown = yes\n\" }\n";
    let (host, id) = quoted_script_snapshot(text);
    let diagnostics = diagnostics(&host.snapshot(), &id);
    let invalid = diagnostics
        .iter()
        .find(|item| item.code == DiagnosticCode::InvalidValue && item.message.contains("foo"))
        .expect("quoted child value diagnostic");
    assert_eq!(
        invalid.range.start(),
        u32::try_from(text.find("maybe").expect("maybe")).expect("offset")
    );
    let unknown = diagnostics
        .iter()
        .find(|item| item.code == DiagnosticCode::UnknownKey && item.message.contains("unknown"))
        .expect("quoted child key diagnostic");
    assert_eq!(
        unknown.range.start(),
        u32::try_from(text.find("unknown").expect("unknown")).expect("offset")
    );
}

#[test]
fn quoted_script_diagnostics_map_nested_escapes_and_recovered_syntax() {
    let text = "trigger = { embedded = \"nested = \\\"foo = maybe\\\"\nbroken = {\" }\n";
    let (host, id) = quoted_script_snapshot(text);
    let diagnostics = diagnostics(&host.snapshot(), &id);
    assert!(diagnostics.iter().any(|item| {
        item.code == DiagnosticCode::InvalidValue
            && item.range.start()
                == u32::try_from(text.find("maybe").expect("maybe")).expect("offset")
    }));
    assert!(diagnostics.iter().any(|item| {
        item.code == DiagnosticCode::Syntax
            && item.range.start()
                >= u32::try_from(text.find("broken").expect("broken")).expect("offset")
    }));
}

#[test]
fn scripted_macro_bare_parameter_validates_quoted_effect_payload_at_call_site() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-analysis-macro-quoted-diag-{nonce}"));
    let definitions = root.join("common/scripted_effects");
    std::fs::create_dir_all(&definitions).expect("definition directory");
    std::fs::write(definitions.join("00_validate.txt"), "inject = { $BODY$ }\n")
        .expect("macro definition");
    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(1),
        SourceRootKind::CurrentMod,
        root.clone(),
    )]));
    host.refresh_source_roots().expect("scan definitions");
    let id = DocumentId::new("file:///tmp/events/macro-quoted-diagnostic.txt");
    let text = "country_event = { immediate = { inject = { BODY = \"definitely_unknown_effect = yes\" } } }\n";
    host.open_document(id.clone(), 1, text.to_owned(), None)
        .expect("open call");
    let expected_start =
        u32::try_from(text.find("definitely_unknown_effect").expect("key")).expect("range start");

    let diagnostics = diagnostics(&host.snapshot(), &id);
    let unknown = diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == DiagnosticCode::UnknownKey)
        .unwrap_or_else(|| panic!("missing quoted macro diagnostic: {diagnostics:?}"));

    assert_eq!(unknown.range.start(), expected_start);
    assert_eq!(
        unknown.range.end(),
        expected_start + u32::try_from("definitely_unknown_effect".len()).expect("length")
    );
    assert!(unknown.message.contains("in expansion of `inject`"));
    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn first_party_mission_trigger_and_effect_accept_quoted_script_forms() {
    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(1),
        SourceRootKind::CurrentMod,
        std::path::PathBuf::from("/tmp"),
    )]));
    let id = DocumentId::new("file:///tmp/missions/quoted_mission.txt");
    let text = concat!(
        "test_series = { slot = 1 generic = no ai = yes has_country_shield = no ",
        "test_mission = { icon = mission_unknown position = 1 required_missions = { } ",
        "trigger = \"definitely_unknown_trigger = yes\" ",
        "effect = \"definitely_unknown_effect = yes\" } }\n",
    );
    host.open_document(
        id.clone(),
        1,
        text.to_owned(),
        Some(std::path::PathBuf::from("/tmp/missions/quoted_mission.txt")),
    )
    .expect("open mission");

    let diagnostics = diagnostics(&host.snapshot(), &id);

    for key in ["definitely_unknown_trigger", "definitely_unknown_effect"] {
        let start = u32::try_from(text.find(key).expect("inner key")).expect("offset");
        assert!(
            diagnostics.iter().any(|diagnostic| {
                diagnostic.code == DiagnosticCode::UnknownKey && diagnostic.range.start() == start
            }),
            "missing quoted mission diagnostic for {key}: {diagnostics:?}"
        );
    }
    assert!(
        diagnostics.iter().all(|diagnostic| {
            diagnostic.code != DiagnosticCode::InvalidValue
                || (!contains_text_range(text, diagnostic.range, "trigger")
                    && !contains_text_range(text, diagnostic.range, "effect"))
        }),
        "quoted mission containers must not be rejected as node values: {diagnostics:?}"
    );
}

fn contains_text_range(text: &str, range: TextRange, needle: &str) -> bool {
    let start = usize::try_from(range.start()).unwrap_or(text.len());
    let end = usize::try_from(range.end()).unwrap_or(text.len());
    text.get(start..end)
        .is_some_and(|slice| slice.contains(needle))
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
fn scripted_macro_blocks_validate_required_and_duplicate_parameters() {
    use std::fs;

    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-analysis-macro-args-{nonce}"));
    let definitions = root.join("common/scripted_effects");
    fs::create_dir_all(&definitions).expect("scripted effect directory");
    fs::write(
        definitions.join("definitions.txt"),
        "apply = { value = $amount$ [[optional] enabled = $optional$ ] }\n",
    )
    .expect("scripted effect definition");

    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(1),
        SourceRootKind::CurrentMod,
        root.clone(),
    )]));
    host.refresh_source_roots().expect("scan definitions");
    let id = DocumentId::new("file:///tmp/events/macro-arguments.txt");
    host.open_document(
        id.clone(),
        1,
        concat!(
            "country_event = { immediate = { ",
            "apply = { optional = yes } ",
            "apply = { amount = 1 amount = 2 } ",
            "} }\n",
        )
        .to_owned(),
        None,
    )
    .expect("open macro calls");

    let results = diagnostics(&host.snapshot(), &id);
    assert!(results.iter().any(|item| {
        item.code == DiagnosticCode::Cardinality
            && item
                .message
                .contains("missing required parameter(s): `amount`")
    }));
    assert!(results.iter().any(|item| {
        item.code == DiagnosticCode::Cardinality
            && item.severity == 2
            && item
                .message
                .contains("parameter `amount` is provided more than once")
    }));

    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn scripted_macro_definition_parameters_do_not_trigger_value_or_key_diagnostics() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-analysis-macro-placeholders-{nonce}"));
    let definition_path = root.join("common/scripted_effects/00_placeholders.txt");
    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(1),
        SourceRootKind::CurrentMod,
        root,
    )]));
    let id = DocumentId::new("file:///tmp/common/scripted_effects/00_placeholders.txt");
    host.open_document(
        id.clone(),
        1,
        "probe = { add_prestige = $PRESTIGE$ $EFFECT$ = yes }\n".to_owned(),
        Some(definition_path),
    )
    .expect("open macro definition");

    let results = diagnostics(&host.snapshot(), &id);
    assert!(
        results.iter().all(|diagnostic| {
            !(diagnostic.code == DiagnosticCode::InvalidValue
                && diagnostic.message.contains("add_prestige"))
                && !(diagnostic.code == DiagnosticCode::UnknownKey
                    && diagnostic.message.contains("$EFFECT$"))
        }),
        "owner-local macro placeholders must defer binding-dependent checks: {results:?}"
    );
}

#[test]
fn dollar_tokens_outside_scripted_macro_definitions_remain_diagnosable() {
    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    let id = DocumentId::new("file:///tmp/events/literal-dollar.txt");
    host.open_document(
        id.clone(),
        1,
        "country_event = { immediate = { add_prestige = $PRESTIGE$ $EFFECT$ = yes } }\n".to_owned(),
        None,
    )
    .expect("open ordinary script");

    let results = diagnostics(&host.snapshot(), &id);
    assert!(results.iter().any(|diagnostic| {
        diagnostic.code == DiagnosticCode::InvalidValue
            && diagnostic.message.contains("add_prestige")
    }));
    assert!(results.iter().any(|diagnostic| {
        diagnostic.code == DiagnosticCode::UnknownKey && diagnostic.message.contains("$EFFECT$")
    }));
}

#[test]
fn scripted_macro_expansion_validates_bound_values_and_dynamic_keys_at_arguments() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-analysis-macro-expand-{nonce}"));
    let definitions = root.join("common/scripted_effects");
    std::fs::create_dir_all(&definitions).expect("definition directory");
    std::fs::write(
        definitions.join("00_expand.txt"),
        concat!(
            "scaled = { add_prestige = $AMOUNT$ }\n",
            "dynamic = { $EFFECT$ = yes }\n",
        ),
    )
    .expect("macro definitions");
    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(1),
        SourceRootKind::CurrentMod,
        root.clone(),
    )]));
    host.refresh_source_roots().expect("scan definitions");
    let id = DocumentId::new("file:///tmp/events/expanded.txt");
    let source = concat!(
        "country_event = { immediate = { ",
        "scaled = { AMOUNT = nope } ",
        "dynamic = { EFFECT = definitely_not_an_effect }",
        " } }\n",
    );
    host.open_document(id.clone(), 1, source.to_owned(), None)
        .expect("open calls");

    let results = diagnostics(&host.snapshot(), &id);
    let amount_start = u32::try_from(source.find("nope").expect("amount value")).expect("range");
    assert!(
        results.iter().any(|diagnostic| {
            diagnostic.code == DiagnosticCode::InvalidValue
                && diagnostic.range
                    == TextRange::new(amount_start, amount_start + 4).expect("amount range")
                && diagnostic.message.contains("in expansion of `scaled`")
        }),
        "{results:?}"
    );
    let effect = "definitely_not_an_effect";
    let effect_start = u32::try_from(source.find(effect).expect("effect value")).expect("range");
    assert!(
        results.iter().any(|diagnostic| {
            diagnostic.code == DiagnosticCode::UnknownKey
                && diagnostic.range
                    == TextRange::new(
                        effect_start,
                        effect_start + u32::try_from(effect.len()).expect("length"),
                    )
                    .expect("effect range")
                && diagnostic.message.contains("in expansion of `dynamic`")
        }),
        "{results:?}"
    );
    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn scripted_macro_definition_defers_parameterized_nested_invocations() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-analysis-macro-forward-{nonce}"));
    let definitions = root.join("common/scripted_effects");
    std::fs::create_dir_all(&definitions).expect("definition directory");
    let definition_path = definitions.join("00_forward.txt");
    let definitions_source = concat!(
        "helper = { add_prestige = $AMOUNT$ }\n",
        "wrapper = { helper = $X$ }\n",
        "wrapper2 = { helper = { AMOUNT = $X$ } }\n",
        "wrapper3 = { helper = { $PARAM$ = $X$ } }\n",
    );
    std::fs::write(&definition_path, definitions_source).expect("macro definitions");
    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(1),
        SourceRootKind::CurrentMod,
        root.clone(),
    )]));
    host.refresh_source_roots().expect("scan definitions");
    let definition_id = DocumentId::new("file:///tmp/common/scripted_effects/00_forward.txt");
    host.open_document(
        definition_id.clone(),
        1,
        definitions_source.to_owned(),
        Some(definition_path),
    )
    .expect("open definitions");

    let definition_diagnostics = diagnostics(&host.snapshot(), &definition_id);
    assert!(
        !definition_diagnostics.iter().any(|diagnostic| {
            diagnostic.message.contains("in expansion of `helper`")
                || diagnostic
                    .message
                    .contains("expansion requires parameter `AMOUNT`")
        }),
        "parameterized forwarding must be deferred: {definition_diagnostics:?}"
    );

    let call_id = DocumentId::new("file:///tmp/events/forward.txt");
    let call_source = concat!(
        "country_event = { immediate = { ",
        "wrapper2 = { X = nope }",
        " wrapper3 = { PARAM = AMOUNT X = nope }",
        " } }\n",
    );
    host.open_document(call_id.clone(), 1, call_source.to_owned(), None)
        .expect("open call");
    let call_diagnostics = diagnostics(&host.snapshot(), &call_id);
    assert!(
        call_diagnostics.iter().any(|diagnostic| {
            diagnostic.code == DiagnosticCode::InvalidValue
                && diagnostic.message.contains("in expansion of `wrapper2`")
                && diagnostic.message.contains("in expansion of `helper`")
        }),
        "concrete outer calls must still recursively validate: {call_diagnostics:?}"
    );
    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn scripted_macro_missing_required_parameter_is_reported_once() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-analysis-macro-required-{nonce}"));
    let definitions = root.join("common/scripted_effects");
    std::fs::create_dir_all(&definitions).expect("definition directory");
    std::fs::write(
        definitions.join("00_required.txt"),
        "helper = { add_prestige = $AMOUNT$ }\n",
    )
    .expect("macro definition");
    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(1),
        SourceRootKind::CurrentMod,
        root.clone(),
    )]));
    host.refresh_source_roots().expect("scan definition");
    let id = DocumentId::new("file:///tmp/events/required.txt");
    host.open_document(
        id.clone(),
        1,
        "country_event = { immediate = { helper = { } } }\n".to_owned(),
        None,
    )
    .expect("open call");

    let results = diagnostics(&host.snapshot(), &id);
    let missing = results
        .iter()
        .filter(|diagnostic| {
            diagnostic.code == DiagnosticCode::Cardinality && diagnostic.message.contains("AMOUNT")
        })
        .collect::<Vec<_>>();
    assert_eq!(missing.len(), 1, "{results:?}");
    assert!(missing[0].message.contains("missing required parameter"));
    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn scripted_macro_expansion_rejects_block_bindings_and_uses_last_duplicate_scalar() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-analysis-macro-binding-{nonce}"));
    let definitions = root.join("common/scripted_effects");
    std::fs::create_dir_all(&definitions).expect("definition directory");
    std::fs::write(
        definitions.join("00_binding.txt"),
        "scaled = { add_prestige = $AMOUNT$ }\n",
    )
    .expect("macro definition");
    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(1),
        SourceRootKind::CurrentMod,
        root.clone(),
    )]));
    host.refresh_source_roots().expect("scan definition");
    let id = DocumentId::new("file:///tmp/events/bindings.txt");
    let source = concat!(
        "country_event = { immediate = { ",
        "scaled = { AMOUNT = { foo = yes } } ",
        "scaled = { AMOUNT = nope AMOUNT = 1 }",
        " } }\n",
    );
    host.open_document(id.clone(), 1, source.to_owned(), None)
        .expect("open calls");

    let results = diagnostics(&host.snapshot(), &id);
    assert!(
        results.iter().any(|diagnostic| {
            diagnostic.code == DiagnosticCode::InvalidValue
                && diagnostic.message.contains("must be a scalar token")
        }),
        "{results:?}"
    );
    assert!(
        results.iter().any(|diagnostic| {
            diagnostic.code == DiagnosticCode::Cardinality
                && diagnostic.severity == 2
                && diagnostic.message.contains("provided more than once")
        }),
        "{results:?}"
    );
    let stale_start = u32::try_from(source.find("nope").expect("stale duplicate")).expect("range");
    assert!(
        !results.iter().any(|diagnostic| {
            diagnostic.code == DiagnosticCode::InvalidValue
                && diagnostic.range.start() == stale_start
                && diagnostic.message.contains("in expansion of `scaled`")
        }),
        "last-wins binding must validate the final scalar: {results:?}"
    );
    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn scripted_macro_expansion_activates_conditionals_and_reports_cycles() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-analysis-macro-cycle-{nonce}"));
    let definitions = root.join("common/scripted_effects");
    std::fs::create_dir_all(&definitions).expect("definition directory");
    std::fs::write(
        definitions.join("00_expand.txt"),
        concat!(
            "conditional = { [[ENABLED] add_prestige = $AMOUNT$ ] }\n",
            "first = { second = yes }\n",
            "second = { first = yes }\n",
        ),
    )
    .expect("macro definitions");
    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(1),
        SourceRootKind::CurrentMod,
        root.clone(),
    )]));
    host.refresh_source_roots().expect("scan definitions");
    let id = DocumentId::new("file:///tmp/events/cycle.txt");
    let source = concat!(
        "country_event = { immediate = { ",
        "conditional = { } ",
        "conditional = { ENABLED = yes } ",
        "first = yes",
        " } }\n",
    );
    host.open_document(id.clone(), 1, source.to_owned(), None)
        .expect("open calls");

    let results = diagnostics(&host.snapshot(), &id);
    assert!(
        results.iter().any(|diagnostic| {
            diagnostic.code == DiagnosticCode::Cardinality
                && diagnostic.message.contains("requires parameter `AMOUNT`")
        }),
        "{results:?}"
    );
    assert!(
        results.iter().any(|diagnostic| {
            diagnostic.code == DiagnosticCode::MacroExpansionCycle
                && diagnostic.message.contains("first -> second -> first")
        }),
        "{results:?}"
    );
    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn scripted_macro_expansion_enforces_a_global_expansion_depth_limit() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-analysis-macro-depth-{nonce}"));
    let definitions = root.join("common/scripted_effects");
    std::fs::create_dir_all(&definitions).expect("definition directory");
    let mut source = String::new();
    for depth in 0..34 {
        source.push_str(&format!(
            "macro_{depth} = {{ macro_{} = yes }}\n",
            depth + 1
        ));
    }
    source.push_str("macro_34 = { add_prestige = 1 }\n");
    std::fs::write(definitions.join("00_depth.txt"), source).expect("macro definitions");
    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(1),
        SourceRootKind::CurrentMod,
        root.clone(),
    )]));
    host.refresh_source_roots().expect("scan definitions");
    let id = DocumentId::new("file:///tmp/events/depth.txt");
    host.open_document(
        id.clone(),
        1,
        "country_event = { immediate = { macro_0 = yes } }\n".to_owned(),
        None,
    )
    .expect("open call");

    let results = diagnostics(&host.snapshot(), &id);
    assert!(
        results.iter().any(|diagnostic| {
            diagnostic.code == DiagnosticCode::MacroExpansionLimit
                && diagnostic.severity == 2
                && diagnostic.message.contains("expansion depth")
        }),
        "{results:?}"
    );
    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn empty_macro_expansion_maps_required_cardinality_to_the_call() {
    let mut model = pdx_game::eu4::first_party_rules()
        .expect("first-party rules")
        .model()
        .clone();
    model.semantic.rules.push(SemanticRule {
        id: "fixture:effect:required-in-empty-macro".to_owned(),
        context: "effect".to_owned(),
        parent_path: Vec::new(),
        key: KeyMatcher::Exact("fixture_required".to_owned()),
        operator: None,
        value: ValueMatcher::Bool,
        shape: RuleShape::Leaf,
        child_context: None,
        alternative_id: None,
        severity: None,
        required: true,
        deprecated: false,
        documentation: Vec::new(),
        allowed_scopes: Vec::new(),
        push_scope: None,
        replace_scope: Vec::new(),
        min_occurs: Some(1),
        strict_min: true,
        max_occurs: None,
        source_file: "fixture.semantic".to_owned(),
        line: 1,
    });
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-analysis-empty-macro-{nonce}"));
    let definitions = root.join("common/scripted_effects");
    std::fs::create_dir_all(&definitions).expect("definition directory");
    std::fs::write(definitions.join("00_empty.txt"), "empty_macro = { }\n")
        .expect("macro definition");
    let mut host = eu4_host(RuleSet::from_model(model));
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(1),
        SourceRootKind::CurrentMod,
        root.clone(),
    )]));
    host.refresh_source_roots().expect("scan definition");
    let id = DocumentId::new("file:///tmp/events/empty-macro.txt");
    let source = "country_event = { immediate = { empty_macro = yes } }\n";
    host.open_document(id.clone(), 1, source.to_owned(), None)
        .expect("open call");

    let results = diagnostics(&host.snapshot(), &id);
    let call_start = u32::try_from(source.find("empty_macro").expect("call")).expect("range");
    assert!(
        results.iter().any(|diagnostic| {
            diagnostic.code == DiagnosticCode::Cardinality
                && diagnostic.range.start() == call_start
                && diagnostic.message.contains("in expansion of `empty_macro`")
                && diagnostic.message.contains("fixture_required")
        }),
        "{results:?}"
    );
    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn vanilla_cache_only_macro_expands_persisted_body_semantics() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-analysis-cached-macro-{nonce}"));
    let definitions = root.join("common/scripted_effects");
    std::fs::create_dir_all(&definitions).expect("definition directory");
    std::fs::write(
        definitions.join("00_cached.txt"),
        "cached_macro = { definitely_unknown_key = yes }\n",
    )
    .expect("macro definition");
    let rules = pdx_game::eu4::first_party_rules().expect("first-party rules");
    let mut vanilla_host = eu4_host(rules.clone());
    vanilla_host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(0),
        SourceRootKind::Vanilla,
        root.clone(),
    )]));
    vanilla_host
        .refresh_source_roots()
        .expect("scan cache source");
    let cache = VanillaIndexCache::from_snapshot(&vanilla_host.snapshot()).expect("build cache");
    std::fs::remove_dir_all(&root).expect("discard Vanilla source");

    let mut host = eu4_host(rules);
    host.install_vanilla_cache(cache).expect("install cache");
    let id = DocumentId::new("file:///tmp/events/cached-macro.txt");
    host.open_document(
        id.clone(),
        1,
        "country_event = { immediate = { cached_macro = yes } }\n".to_owned(),
        None,
    )
    .expect("open call");

    let results = diagnostics(&host.snapshot(), &id);
    assert!(
        results
            .iter()
            .any(|diagnostic| { diagnostic.message.contains("definitely_unknown_key") }),
        "cache-only macro body was not expanded: {results:?}"
    );
}

#[test]
fn vanilla_cache_only_macro_validates_quoted_payload_at_exact_call_site_range() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-analysis-cached-quoted-diag-{nonce}"));
    let vanilla = root.join("vanilla");
    let definitions = vanilla.join("common/scripted_effects");
    std::fs::create_dir_all(&definitions).expect("definition directory");
    std::fs::write(
        definitions.join("00_cached.txt"),
        "cached_inject = { $BODY$ }\n",
    )
    .expect("macro definition");
    let rules = pdx_game::eu4::first_party_rules().expect("first-party rules");
    let mut vanilla_host = eu4_host(rules.clone());
    vanilla_host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(0),
        SourceRootKind::Vanilla,
        vanilla.clone(),
    )]));
    vanilla_host.refresh_source_roots().expect("scan Vanilla");
    let cache = VanillaIndexCache::from_snapshot(&vanilla_host.snapshot()).expect("build cache");
    let cache_path = root.join("cache/vanilla.pdxindex");
    cache.save(&cache_path).expect("save cache");
    std::fs::remove_dir_all(&vanilla).expect("discard Vanilla source");
    let cache = VanillaIndexCache::load(&cache_path).expect("load cache without source");

    let mut host = eu4_host(rules);
    host.install_vanilla_cache(cache).expect("install cache");
    let id = DocumentId::new("file:///tmp/events/cached-quoted-diagnostic.txt");
    let text = "country_event = { immediate = { cached_inject = { BODY = \"definitely_unknown_effect = yes\" } } }\n";
    host.open_document(id.clone(), 1, text.to_owned(), None)
        .expect("open call");
    let expected_start =
        u32::try_from(text.find("definitely_unknown_effect").expect("key")).expect("range");

    let results = diagnostics(&host.snapshot(), &id);
    let unknown = results
        .iter()
        .find(|diagnostic| {
            diagnostic.code == DiagnosticCode::UnknownKey
                && diagnostic.message.contains("definitely_unknown_effect")
        })
        .unwrap_or_else(|| panic!("missing cache-only quoted diagnostic: {results:?}"));
    assert_eq!(unknown.range.start(), expected_start);
    assert_eq!(
        unknown.range.end(),
        expected_start + u32::try_from("definitely_unknown_effect".len()).expect("length")
    );
    assert!(unknown.message.contains("in expansion of `cached_inject`"));
    std::fs::remove_dir_all(root).expect("cleanup");
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
fn localisation_symbols_prefer_the_english_definition_across_languages() {
    let mut host = eu4_host(pdx_game::eu4::bootstrap_rules());
    host.open_document(
        DocumentId::new("file:///tmp/localisation/l_english/test_l_english.yml"),
        1,
        "l_english:\n shared_key: \"English\"\n".to_owned(),
        Some(std::path::PathBuf::from(
            "/tmp/localisation/l_english/test_l_english.yml",
        )),
    )
    .expect("open english localisation");
    host.open_document(
        DocumentId::new("file:///tmp/localisation/l_french/test_l_french.yml"),
        1,
        "l_french:\n shared_key: \"Français\"\n".to_owned(),
        Some(std::path::PathBuf::from(
            "/tmp/localisation/l_french/test_l_french.yml",
        )),
    )
    .expect("open french localisation");
    let script = DocumentId::new("file:///tmp/events/test.txt");
    host.open_document(
        script.clone(),
        1,
        "country_event = { id = a.1 option = { name = option_a custom_tooltip = shared_key } }\n"
            .to_owned(),
        Some(std::path::PathBuf::from("/tmp/events/test.txt")),
    )
    .expect("open script");

    let results = diagnostics(&host.snapshot(), &script);
    assert!(
        !results
            .iter()
            .any(|item| item.code == DiagnosticCode::AmbiguousSymbol),
        "per-language variants of one key must not look ambiguous: {results:?}"
    );
    assert!(
        !results
            .iter()
            .any(|item| item.code == DiagnosticCode::UnknownSymbol
                && item.message.contains("shared_key")),
        "the key exists in the workspace and must resolve: {results:?}"
    );
}
