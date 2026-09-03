use super::support::*;
use crate::{
    Diagnostic, DiagnosticCertainty, DiagnosticProvenance, Severity, quick_fixes_with_cancellation,
    scripted_localisation_names,
};

#[test]
fn diagnostic_ids_are_pascal_case_and_metadata_is_structured() {
    assert_eq!(DiagnosticCode::UnknownKey.as_str(), "UnknownKey");
    assert_eq!(
        DiagnosticCode::UnknownBareValue.as_str(),
        "UnknownBareValue"
    );
    assert_eq!(
        DiagnosticCode::TargetWrongScope.as_str(),
        "TargetWrongScope"
    );
    let diagnostic = Diagnostic::new(
        DiagnosticCode::TargetWrongScope,
        Severity::Error,
        TextRange::new(2, 5).expect("range"),
        "target has the wrong scope".to_owned(),
    )
    .with_certainty(DiagnosticCertainty::Contextual)
    .with_provenance(DiagnosticProvenance {
        rule_id: Some("fixture:target".to_owned()),
        context: Some("effect".to_owned()),
        source_file: Some("fixture.json".to_owned()),
        source_line: Some(7),
    });
    assert_eq!(diagnostic.severity, Severity::Error);
    assert_eq!(diagnostic.certainty, DiagnosticCertainty::Contextual);
    assert_eq!(diagnostic.provenance.as_ref().unwrap().source_line, Some(7));
}

#[test]
fn scripted_localisation_names_feed_indexed_diagnostics_and_completion() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-analysis-scripted-loc-{nonce}"));
    let definitions = root.join("common/scripted_localisation");
    let localisation = root.join("localisation");
    std::fs::create_dir_all(&definitions).expect("scripted localisation directory");
    std::fs::create_dir_all(&localisation).expect("localisation directory");
    std::fs::write(
        definitions.join("00_defs.txt"),
        "defined_text = { name = Scripted.One text = yes }\nother = { name = Scripted.Two }\n",
    )
    .expect("scripted localisation definitions");

    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(1),
        SourceRootKind::CurrentMod,
        root.clone(),
    )]));
    host.refresh_source_roots()
        .expect("scan scripted localisation");
    let snapshot = host.snapshot();
    let names = scripted_localisation_names(&snapshot);
    assert_eq!(names, ["Scripted.One", "Scripted.Two"]);

    let definitions_path = definitions.join("00_defs.txt");
    let definitions_id = DocumentId::new("file:///tmp/scripted-localisation-definitions.txt");
    host.open_document(
        definitions_id.clone(),
        1,
        "defined_text = { name = Overlay.Only text = yes }\n".to_owned(),
        Some(definitions_path),
    )
    .expect("open scripted localisation overlay");
    assert_eq!(
        scripted_localisation_names(&host.snapshot()),
        ["Overlay.Only"],
        "an overlay must replace its backing scripted-localisation shard"
    );
    host.close_document(&definitions_id)
        .expect("close scripted localisation overlay");
    assert_eq!(
        scripted_localisation_names(&host.snapshot()),
        ["Scripted.One", "Scripted.Two"],
        "closing an overlay must invalidate its derived name cache"
    );

    let id = DocumentId::new("file:///tmp/scripted-localisation-use.yml");
    let text = "l_english:\nentry: \"[ROOT.Scripted.One] [ROOT.MissingScripted]\"\n";
    host.open_document(
        id.clone(),
        1,
        text.to_owned(),
        Some(localisation.join("use.yml")),
    )
    .expect("open localisation use site");
    let snapshot = host.snapshot();
    let diagnostics = crate::diagnostics(&snapshot, &id);
    let unknown = diagnostics
        .iter()
        .find(|diagnostic| diagnostic.message.contains("MissingScripted"))
        .expect("unknown scripted localisation command");
    assert_eq!(unknown.code, DiagnosticCode::UnknownSymbol);
    assert_eq!(unknown.severity, Severity::Warning);
    assert_eq!(
        unknown.range.start(),
        u32::try_from(text.find("MissingScripted").expect("missing command")).expect("offset")
    );

    let completion_text = "l_english:\nentry: \"[ROOT.Scripted.O]\"\n";
    let completion_id = DocumentId::new("file:///tmp/scripted-localisation-completion.yml");
    host.open_document(
        completion_id.clone(),
        1,
        completion_text.to_owned(),
        Some(localisation.join("completion.yml")),
    )
    .expect("open localisation completion");
    let position = u32::try_from(
        completion_text
            .find("Scripted.O")
            .expect("completion prefix")
            + "Scripted.O".len(),
    )
    .expect("position");
    let completion = crate::complete(&host.snapshot(), &completion_id, position);
    assert!(
        completion
            .items
            .iter()
            .any(|item| item.label == "Scripted.One"),
        "scripted localisation completion missing: {:?}",
        completion.items
    );

    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn scope_target_failures_use_distinct_categories() {
    let mut model = pdx_game::eu4::bootstrap_model();
    model.semantic.rules.extend([
        SemanticRule {
            id: "fixture:trigger:target".to_owned(),
            context: "trigger".to_owned(),
            parent_path: Vec::new(),
            key: KeyMatcher::Exact("target".to_owned()),
            operator: None,
            value: ValueMatcher::Scope(Some("country".to_owned())),
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
            line: 10,
        },
        SemanticRule {
            id: "fixture:trigger:scope-command".to_owned(),
            context: "trigger".to_owned(),
            parent_path: Vec::new(),
            key: KeyMatcher::Exact("scope".to_owned()),
            operator: None,
            value: ValueMatcher::Scope(Some("country".to_owned())),
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
            line: 11,
        },
    ]);
    let mut host = eu4_host(RuleSet::from_model(model));
    let id = DocumentId::new("file:///tmp/common/events/scope-targets.txt");
    host.open_document(
        id.clone(),
        1,
        "trigger = { target = NOWHERE scope = NOWHERE target = capital }\n".to_owned(),
        None,
    )
    .expect("open scope target fixture");
    let diagnostics = diagnostics(&host.snapshot(), &id);
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.code == DiagnosticCode::InvalidTarget && diagnostic.message.contains("NOWHERE")
    }));
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.code == DiagnosticCode::InvalidScopeCommand
            && diagnostic.message.contains("NOWHERE")
    }));
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.code == DiagnosticCode::TargetWrongScope
            && diagnostic.message.contains("capital")
    }));
}

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

    // Payload parameters are validated by parsing the quoted binding and
    // mapping diagnostics onto its in-string range, without an expansion
    // tree.
    let diagnostics = diagnostics(&host.snapshot(), &id);
    let unknown = diagnostics
        .iter()
        .find(|diagnostic| {
            diagnostic.code == DiagnosticCode::UnknownKey
                && diagnostic.message.contains("definitely_unknown_effect")
        })
        .unwrap_or_else(|| panic!("missing quoted payload diagnostic: {diagnostics:?}"));

    assert_eq!(unknown.range.start(), expected_start);
    assert_eq!(
        unknown.range.end(),
        expected_start + u32::try_from("definitely_unknown_effect".len()).expect("length")
    );
    assert!(!unknown.message.contains("in expansion of"));
    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn scripted_macro_preserves_literal_quoted_script_through_nested_expansion() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-analysis-macro-nested-quoted-{nonce}"));
    let definitions = root.join("common/scripted_effects");
    std::fs::create_dir_all(&definitions).expect("definition directory");
    std::fs::write(
        definitions.join("00_validate.txt"),
        concat!(
            "inject = { $BODY$ }\n",
            "wrapper = { inject = { BODY = \"add_prestige = 1\" } }\n",
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
    let id = DocumentId::new("file:///tmp/events/macro-nested-quoted.txt");
    host.open_document(
        id.clone(),
        1,
        "country_event = { immediate = { wrapper = yes } }\n".to_owned(),
        None,
    )
    .expect("open call");

    let diagnostics = diagnostics(&host.snapshot(), &id);
    assert!(
        diagnostics.iter().all(|diagnostic| {
            diagnostic.code != DiagnosticCode::InvalidValue
                || !diagnostic.message.contains("bare value")
        }),
        "nested quoted Script must be reparsed: {diagnostics:?}"
    );
    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn scripted_macro_omits_missing_optional_forwarded_arguments() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-analysis-macro-forwarded-{nonce}"));
    let definitions = root.join("common/scripted_effects");
    std::fs::create_dir_all(&definitions).expect("definition directory");
    std::fs::write(
        definitions.join("00_validate.txt"),
        concat!(
            "inner = { [[optional] add_prestige = $optional$ ] }\n",
            "outer = { inner = { optional = \"$optional$\" } }\n",
            "composite = { set_country_flag = PREFIX_$optional$_END }\n",
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
    let id = DocumentId::new("file:///tmp/events/macro-forwarded.txt");
    host.open_document(
        id.clone(),
        1,
        "country_event = { id = fixture.1 option = { outer = { } composite = { } } }\n".to_owned(),
        None,
    )
    .expect("open call");

    let results = diagnostics(&host.snapshot(), &id);
    assert!(
        results
            .iter()
            .all(|diagnostic| !diagnostic.message.contains("optional")),
        "omitted forwarding parameter must leave the nested conditional inactive: {results:?}"
    );
    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn runtime_branch_macro_accepts_the_amount_only_legitimacy_call() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-analysis-legitimacy-branch-{nonce}"));
    let definitions = root.join("common/scripted_effects");
    std::fs::create_dir_all(&definitions).expect("definition directory");
    std::fs::write(
        definitions.join("00_legitimacy.txt"),
        concat!(
            "add_legitimacy_or_mil_power = { ",
            "if = { limit = { government = republic } ",
            "add_republican_tradition = $republican_tradition$ } ",
            "else = { add_legitimacy = $amount$ } }\n",
        ),
    )
    .expect("macro definition");
    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(1),
        SourceRootKind::CurrentMod,
        root.clone(),
    )]));
    host.refresh_source_roots().expect("scan definition");
    let id = DocumentId::new("file:///tmp/events/legitimacy-branch.txt");
    host.open_document(
        id.clone(),
        1,
        "country_event = { immediate = { add_legitimacy_or_mil_power = { amount = 5 } } }\n"
            .to_owned(),
        None,
    )
    .expect("open call");

    let results = diagnostics(&host.snapshot(), &id);
    assert!(
        results.iter().all(|diagnostic| {
            !diagnostic.message.contains("add_legitimacy_or_mil_power")
                || (!diagnostic.message.contains("republican_tradition")
                    && !diagnostic.message.contains("active branch"))
        }),
        "amount-only call should be accepted when the alternate parameter is branch-local: {results:?}"
    );
    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn cached_runtime_branch_macro_recomputes_optional_parameters_from_the_template() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-analysis-cached-legitimacy-{nonce}"));
    let definitions = root.join("common/scripted_effects");
    std::fs::create_dir_all(&definitions).expect("definition directory");
    std::fs::write(
        definitions.join("00_legitimacy.txt"),
        concat!(
            "add_legitimacy_or_mil_power = { ",
            "if = { limit = { government = republic } ",
            "add_republican_tradition = $republican_tradition$ } ",
            "else = { add_legitimacy = $amount$ } }\n",
        ),
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
        .expect("scan definition");
    let cache = IndexCache::from_snapshot(&vanilla_host.snapshot()).expect("build cache");
    std::fs::remove_dir_all(&root).expect("discard source");

    let mut host = eu4_host(rules);
    host.install_index_cache(cache).expect("install cache");
    let id = DocumentId::new("file:///tmp/events/cached-legitimacy.txt");
    host.open_document(
        id.clone(),
        1,
        "country_event = { immediate = { add_legitimacy_or_mil_power = { amount = 5 } } }\n"
            .to_owned(),
        None,
    )
    .expect("open call");

    let results = diagnostics(&host.snapshot(), &id);
    assert!(
        results.iter().all(|diagnostic| {
            !diagnostic.message.contains("add_legitimacy_or_mil_power")
                || (!diagnostic.message.contains("republican_tradition")
                    && !diagnostic.message.contains("active branch"))
        }),
        "cached template should carry enough branch information: {results:?}"
    );
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
fn invalid_enum_value_carries_one_unique_did_you_mean_fix() {
    let mut model = pdx_game::eu4::bootstrap_model();
    model.semantic.enum_values.insert(
        "fixture_modes".to_owned(),
        vec!["historic".to_owned(), "dynamic".to_owned()],
    );
    model.semantic.rules.push(SemanticRule {
        id: "fixture:trigger:mode".to_owned(),
        context: "trigger".to_owned(),
        parent_path: Vec::new(),
        key: KeyMatcher::Exact("mode".to_owned()),
        operator: Some("=".to_owned()),
        value: ValueMatcher::Enum("fixture_modes".to_owned()),
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
    });
    let mut host = eu4_host(RuleSet::from_model(model));
    let id = DocumentId::new("file:///tmp/common/events/enum-fix.txt");
    let text = "trigger = { mode = histori }\n";
    host.open_document(id.clone(), 1, text.to_owned(), None)
        .expect("open enum fixture");

    let diagnostics = diagnostics(&host.snapshot(), &id);
    let invalid = diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == DiagnosticCode::InvalidValue)
        .expect("invalid enum value diagnostic");
    assert_eq!(invalid.fixes.len(), 1);
    let fix = &invalid.fixes[0];
    assert_eq!(fix.title, "Did you mean 'historic'?");
    assert_eq!(fix.new_text, "\"historic\"");
    assert_eq!(
        fix.range.start(),
        u32::try_from(text.find("histori").expect("invalid value")).expect("offset")
    );
    let fixes = quick_fixes_with_cancellation(
        &host.snapshot(),
        &id,
        Some(fix.range),
        &CancellationToken::new(),
    )
    .expect("quick-fix query");
    assert_eq!(fixes, vec![fix.clone()]);
}

#[test]
fn ambiguous_enum_suggestions_do_not_produce_a_fix() {
    let mut model = pdx_game::eu4::bootstrap_model();
    model.semantic.enum_values.insert(
        "fixture_modes".to_owned(),
        vec!["cat".to_owned(), "bat".to_owned()],
    );
    model.semantic.rules.push(SemanticRule {
        id: "fixture:trigger:mode".to_owned(),
        context: "trigger".to_owned(),
        parent_path: Vec::new(),
        key: KeyMatcher::Exact("mode".to_owned()),
        operator: Some("=".to_owned()),
        value: ValueMatcher::Enum("fixture_modes".to_owned()),
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
    });
    let mut host = eu4_host(RuleSet::from_model(model));
    let id = DocumentId::new("file:///tmp/common/events/enum-tie.txt");
    host.open_document(id.clone(), 1, "trigger = { mode = rat }\n".to_owned(), None)
        .expect("open enum tie fixture");

    let diagnostics = diagnostics(&host.snapshot(), &id);
    let invalid = diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == DiagnosticCode::InvalidValue)
        .expect("invalid enum value diagnostic");
    assert!(
        invalid.fixes.is_empty(),
        "ambiguous values must not be auto-fixed"
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
fn dynamic_rule_call_site_validates_argument_values_and_dispatch_keys() {
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
                && diagnostic
                    .message
                    .contains("for parameter `AMOUNT` of scripted `scaled`")
                && diagnostic
                    .message
                    .contains("does not match its usage in the definition body")
        }),
        "{results:?}"
    );
    let effect = "definitely_not_an_effect";
    let effect_start = u32::try_from(source.find(effect).expect("effect value")).expect("range");
    assert!(
        results.iter().any(|diagnostic| {
            diagnostic.code == DiagnosticCode::InvalidValue
                && diagnostic.range
                    == TextRange::new(
                        effect_start,
                        effect_start + u32::try_from(effect.len()).expect("length"),
                    )
                    .expect("effect range")
                && diagnostic
                    .message
                    .contains("for parameter `EFFECT` of scripted `dynamic`")
                && diagnostic
                    .message
                    .contains("does not name a known effect key")
        }),
        "{results:?}"
    );
    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn dynamic_rule_dispatch_accepts_known_keys_and_scope_registers() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-analysis-macro-dispatch-ok-{nonce}"));
    let definitions = root.join("common/scripted_effects");
    std::fs::create_dir_all(&definitions).expect("definition directory");
    std::fs::write(
        definitions.join("00_dispatch.txt"),
        concat!(
            "handler = { $EFFECT$ = yes }\n",
            "scoped = { $SCOPE$ = { add_prestige = 1 } }\n",
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
    let id = DocumentId::new("file:///tmp/events/dispatch-ok.txt");
    let source = concat!(
        "country_event = { immediate = { ",
        "handler = { EFFECT = add_prestige } ",
        "scoped = { SCOPE = ROOT }",
        " } }\n",
    );
    host.open_document(id.clone(), 1, source.to_owned(), None)
        .expect("open calls");

    // Rendered keys that name real effects or scope registers must pass the
    // dispatch check without instantiation.
    let results = diagnostics(&host.snapshot(), &id);
    assert!(
        !results.iter().any(|diagnostic| {
            diagnostic.code == DiagnosticCode::InvalidValue
                && diagnostic.message.contains("does not name a known")
        }),
        "{results:?}"
    );
    let snapshot = host.snapshot();
    let handler = crate::dynamic_rules::dynamic_rule_row(&snapshot, "scripted_effect", "handler")
        .expect("handler row");
    assert!(handler.dispatches_dynamically);
    assert!(
        handler
            .parameters
            .iter()
            .any(|parameter| parameter.name == "EFFECT" && parameter.used_in_key),
        "{handler:?}"
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
        "numeric_helper = { [[1] always = $1$ ] }\n",
        "numeric_wrapper = { numeric_helper = { 1 = \"$1$\" } }\n",
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
                    .contains("in expansion of `numeric_helper`")
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
    // Forwarding edges borrow the callee parameter's own constraints: `X`
    // flows into `helper`'s `AMOUNT` (an `add_prestige` float site), so a
    // non-numeric binding is rejected at the outer call site.
    assert!(
        call_diagnostics.iter().any(|diagnostic| {
            diagnostic.code == DiagnosticCode::InvalidValue
                && diagnostic
                    .message
                    .contains("for parameter `X` of scripted `wrapper2`")
                && diagnostic
                    .message
                    .contains("does not match its usage in the definition body")
        }),
        "forwarded arguments inherit callee constraints: {call_diagnostics:?}"
    );
    assert!(
        call_diagnostics.iter().any(|diagnostic| {
            diagnostic.code == DiagnosticCode::InvalidValue
                && diagnostic
                    .message
                    .contains("for parameter `X` of scripted `wrapper3`")
        }),
        "rendered-key forwarding must also be constraint-checked: {call_diagnostics:?}"
    );
    // `$PARAM$ = $X$` inside `wrapper3` renders `helper`'s parameter NAME as a
    // key; argument blocks are not effect statements and must not dispatch.
    assert!(
        !call_diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("does not name a known")),
        "argument-list keys are not dispatch keys: {call_diagnostics:?}"
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
fn dynamic_rule_arguments_reject_block_bindings_and_use_last_duplicate_scalar() {
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
                && diagnostic
                    .message
                    .contains("is used as a scalar value in its body")
                && diagnostic.message.contains("must be provided as one")
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
    // `ENABLED = yes` activates the branch, making `AMOUNT` required for
    // this invocation; the call without `AMOUNT` is rejected at the call
    // site, from the persisted template alone.
    let branch_call_start =
        u32::try_from(source.find("conditional = { ENABLED").expect("branch call")).expect("range");
    assert!(
        results.iter().any(|diagnostic| {
            diagnostic.code == DiagnosticCode::Cardinality
                && diagnostic.range.start() == branch_call_start
                && diagnostic
                    .message
                    .contains("requires parameter(s) `AMOUNT` in the active branch")
        }),
        "{results:?}"
    );
    // The guard-less invocation leaves the branch inactive and stays clean.
    let plain_call_start =
        u32::try_from(source.find("conditional = { }").expect("plain call")).expect("range");
    assert!(
        !results.iter().any(|diagnostic| {
            diagnostic.range.start() == plain_call_start
                && diagnostic.message.contains("active branch")
        }),
        "{results:?}"
    );
    // Statically known cycles are now rejected at the definitions themselves
    // (Rust-style definition-site checking); the call site must not duplicate.
    assert!(
        !results
            .iter()
            .any(|diagnostic| diagnostic.code == DiagnosticCode::MacroExpansionCycle),
        "call site must not duplicate the definition-site report: {results:?}"
    );
    let snapshot = host.snapshot();
    let definitions_file = snapshot
        .source_files()
        .iter()
        .find(|(_, file)| {
            file.logical_path
                .as_str()
                .ends_with("scripted_effects/00_expand.txt")
        })
        .map(|(id, _)| id)
        .expect("definitions file is indexed");
    let definition_results = crate::source_file_diagnostics_with_cancellation(
        &snapshot,
        *definitions_file,
        &CancellationToken::new(),
    )
    .expect("definition diagnostics");
    assert!(
        definition_results.iter().any(|diagnostic| {
            diagnostic.code == DiagnosticCode::MacroExpansionCycle
                && diagnostic.message.contains("`first`")
                && diagnostic.message.contains("first -> second -> first")
        }),
        "the cycle must be reported at the participating definitions: {definition_results:?}"
    );
    assert!(
        definition_results.iter().any(|diagnostic| {
            diagnostic.code == DiagnosticCode::MacroExpansionCycle
                && diagnostic.message.contains("`second`")
                && diagnostic.message.contains("second -> first -> second")
        }),
        "both participants report the rotated cycle walk: {definition_results:?}"
    );
    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn deeply_nested_dynamic_rule_chains_validate_without_expansion_budgets() {
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
    // Row derivation has no node/depth budgets: a 35-deep chain validates the
    // same as a shallow one, and no incomplete-analysis placeholder appears.
    assert!(
        !results
            .iter()
            .any(|diagnostic| diagnostic.code == DiagnosticCode::AnalysisIncomplete),
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
    // Body-container cardinality now surfaces through the ordinary container
    // check at the invocation's own container, unprefixed by expansion
    // bookkeeping.
    assert!(
        results.iter().any(|diagnostic| {
            diagnostic.code == DiagnosticCode::Cardinality
                && diagnostic.range.start() == call_start
                && diagnostic.message.contains("fixture_required")
                && !diagnostic.message.contains("in expansion of")
        }),
        "{results:?}"
    );
    let snapshot = host.snapshot();
    let row = crate::dynamic_rules::dynamic_rule_row(&snapshot, "scripted_effect", "empty_macro")
        .expect("empty definition still derives a row");
    assert!(row.parameters.is_empty());
    assert!(row.body_findings.is_empty());
    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn vanilla_cache_only_dynamic_row_records_unknown_body_statement() {
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
    let cache = IndexCache::from_snapshot(&vanilla_host.snapshot()).expect("build cache");
    std::fs::remove_dir_all(&root).expect("discard Vanilla source");

    let mut host = eu4_host(rules);
    host.install_index_cache(cache).expect("install cache");
    let id = DocumentId::new("file:///tmp/events/cached-macro.txt");
    host.open_document(
        id.clone(),
        1,
        "country_event = { immediate = { cached_macro = yes } }\n".to_owned(),
        None,
    )
    .expect("open call");

    // The persisted template is enough to derive the body finding without the
    // original source; publishing definition-site findings is P3.
    let snapshot = host.snapshot();
    let row = crate::dynamic_rules::dynamic_rule_row(&snapshot, "scripted_effect", "cached_macro")
        .expect("cache-only definition derives a row");
    assert!(
        row.body_findings.iter().any(|finding| {
            finding.kind == crate::dynamic_rules::DynamicBodyFindingKind::UnknownStatement
                && finding.statement == "definitely_unknown_key"
        }),
        "{row:?}"
    );
    let results = diagnostics(&snapshot, &id);
    assert!(
        !results
            .iter()
            .any(|diagnostic| diagnostic.message.contains("definitely_unknown_key")),
        "definition-site findings stay unpublished in the shadow phase: {results:?}"
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
    let cache = IndexCache::from_snapshot(&vanilla_host.snapshot()).expect("build cache");
    let cache_path = root.join("cache/vanilla.pdxindex");
    cache.save(&cache_path).expect("save cache");
    std::fs::remove_dir_all(&vanilla).expect("discard Vanilla source");
    let cache = IndexCache::load(&cache_path).expect("load cache without source");

    let mut host = eu4_host(rules);
    host.install_index_cache(cache).expect("install cache");
    let id = DocumentId::new("file:///tmp/events/cached-quoted-diagnostic.txt");
    let text = "country_event = { immediate = { cached_inject = { BODY = \"definitely_unknown_effect = yes\" } } }\n";
    host.open_document(id.clone(), 1, text.to_owned(), None)
        .expect("open call");
    let expected_start =
        u32::try_from(text.find("definitely_unknown_effect").expect("key")).expect("range");

    // The persisted template marks BODY a payload parameter, so the
    // cache-only definition still gets row-driven payload validation at the
    // exact in-string range.
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
    assert!(!unknown.message.contains("in expansion of"));
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
fn ancestor_personality_localisation_uses_vanilla_key_templates() {
    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(1),
        SourceRootKind::CurrentMod,
        std::path::PathBuf::from("/tmp"),
    )]));
    host.open_document(
        DocumentId::new("file:///tmp/localisation/ancestor_l_english.yml"),
        1,
        "l_english:\n ancestor_test_personality:0 \"Test\"\n desc_ancestor_test_personality:0 \"Description\"\n"
            .to_owned(),
        Some(std::path::PathBuf::from(
            "/tmp/localisation/ancestor_l_english.yml",
        )),
    )
    .expect("open localisation");
    let id = DocumentId::new("file:///tmp/common/ancestor_personalities/test.txt");
    host.open_document(
        id.clone(),
        1,
        "ancestor_test_personality = { global_tax_modifier = 0.1 }\n".to_owned(),
        Some(std::path::PathBuf::from(
            "/tmp/common/ancestor_personalities/test.txt",
        )),
    )
    .expect("open ancestor personality");

    let results = diagnostics(&host.snapshot(), &id);
    assert!(
        results
            .iter()
            .all(|item| item.code != DiagnosticCode::UnknownSymbol),
        "ancestor localisation must use `$` and `desc_$` only: {results:?}"
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

#[test]
fn duplicate_localisation_keys_do_not_produce_ambiguous_diagnostics() {
    let mut host = eu4_host(pdx_game::eu4::bootstrap_rules());
    for (name, text) in [
        ("base_l_english.yml", "l_english:\n shared_key: \"Base\"\n"),
        (
            "replace_l_english.yml",
            "l_english:\n shared_key: \"Replacement\"\n",
        ),
    ] {
        host.open_document(
            DocumentId::new(format!("file:///tmp/localisation/{name}")),
            1,
            text.to_owned(),
            Some(std::path::PathBuf::from(format!(
                "/tmp/localisation/{name}"
            ))),
        )
        .expect("open localisation");
    }
    let script = DocumentId::new("file:///tmp/events/test.txt");
    host.open_document(
        script.clone(),
        1,
        "country_event = { id = a.1 option = { name = shared_key } }\n".to_owned(),
        Some(std::path::PathBuf::from("/tmp/events/test.txt")),
    )
    .expect("open script");

    let results = diagnostics(&host.snapshot(), &script);
    assert!(
        !results.iter().any(|item| {
            item.code == DiagnosticCode::AmbiguousSymbol && item.message.contains("shared_key")
        }),
        "localisation overrides must not be reported as ambiguous: {results:?}"
    );
}

#[test]
fn game_age_ability_definitions_are_collected_only_below_abilities() {
    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(1),
        SourceRootKind::CurrentMod,
        std::path::PathBuf::from("/tmp"),
    )]));
    let id = DocumentId::new("file:///tmp/common/ages/test.txt");
    host.open_document(
        id.clone(),
        1,
        "age_one = { can_start = { always = yes } abilities = { ab_one = { modifier = { global_tax_modifier = 0.1 } } } }\n".to_owned(),
        Some(std::path::PathBuf::from("/tmp/common/ages/test.txt")),
    )
    .expect("open age source");

    let results = diagnostics(&host.snapshot(), &id);
    assert!(
        results.iter().all(|item| {
            !item.message.contains("localisation symbol `can_start`")
                && !item.message.contains("localisation symbol `abilities`")
                && !item.message.contains("unexpected key `ab_one`")
                && !item.message.contains("`<game_age_ability>` occurs 0 times")
        }),
        "age structure was mistaken for ability definitions: {results:?}"
    );
}

#[test]
fn custom_government_attributes_remain_open_world() {
    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    let id = DocumentId::new("file:///tmp/events/test.txt");
    host.open_document(
        id.clone(),
        1,
        "country_event = { id = a.1 trigger = { has_government_attribute = my_custom_attribute } }\n"
            .to_owned(),
        Some(std::path::PathBuf::from("/tmp/events/test.txt")),
    )
    .expect("open event");

    let results = diagnostics(&host.snapshot(), &id);
    assert!(
        results.iter().all(|item| {
            item.code != DiagnosticCode::InvalidValue
                || !item.message.contains("has_government_attribute")
        }),
        "custom attributes must use the open alternative: {results:?}"
    );
}

#[test]
fn runtime_flags_remain_open_world_for_semantic_validation() {
    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    let id = DocumentId::new("file:///tmp/events/runtime-flags.txt");
    let text = "country_event = { id = runtime.1 trigger = { has_country_flag = engine_supplied_flag } immediate = { clr_country_flag = engine_supplied_flag } }\n";
    host.open_document(id.clone(), 1, text.to_owned(), None)
        .expect("open event");

    let diagnostics = diagnostics(&host.snapshot(), &id);
    assert!(
        diagnostics.iter().all(|diagnostic| {
            diagnostic.code != DiagnosticCode::InvalidValue
                || (!diagnostic.message.contains("has_country_flag")
                    && !diagnostic.message.contains("clr_country_flag"))
        }),
        "runtime flags must not require a statically indexed setter: {diagnostics:?}"
    );
}

#[test]
fn unresolved_macro_placeholders_do_not_become_symbol_errors() {
    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    let id = DocumentId::new("file:///tmp/common/scripted_triggers/placeholders.txt");
    let text = "wrapper = { $global_trigger$ = yes custom_trigger_tooltip = { tooltip = $tooltip$ always = yes } }\n";
    host.open_document(id.clone(), 1, text.to_owned(), None)
        .expect("open placeholder definition");

    let results = diagnostics(&host.snapshot(), &id);
    assert!(
        results.iter().all(|diagnostic| {
            diagnostic.code != DiagnosticCode::UnknownSymbol || !diagnostic.message.contains('$')
        }),
        "unbound definition parameters cannot be resolved before invocation: {results:?}"
    );
}

#[test]
fn named_event_targets_are_valid_scope_values_and_wrappers() {
    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    let id = DocumentId::new("file:///tmp/events/named-event-targets.txt");
    let text = "country_event = { id = target.1 trigger = { war_with = event_target:agenda_country event_target:agenda_country = { exists = yes } } immediate = { global_event_target:agenda_province = { add_base_tax = 1 } } }\n";
    host.open_document(id.clone(), 1, text.to_owned(), None)
        .expect("open event");

    let diagnostics = diagnostics(&host.snapshot(), &id);
    assert!(
        diagnostics.iter().all(|diagnostic| {
            !diagnostic.message.contains("event_target:agenda_country")
                && !diagnostic
                    .message
                    .contains("global_event_target:agenda_province")
        }),
        "named event targets should stay conservative until runtime: {diagnostics:?}"
    );
}

#[test]
fn runtime_event_targets_and_exiled_characters_remain_valid() {
    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    let id = DocumentId::new("file:///tmp/events/runtime-targets.txt");
    let text = concat!(
        "country_event = { id = target.2 ",
        "trigger = { has_saved_event_target = engine_supplied_target } ",
        "immediate = { exile_ruler_as = saved_ruler set_ruler = saved_ruler ",
        "exile_heir_as = saved_heir set_heir = saved_heir } }\n",
    );
    host.open_document(id.clone(), 1, text.to_owned(), None)
        .expect("open event");

    let diagnostics = diagnostics(&host.snapshot(), &id);
    assert!(
        diagnostics.iter().all(|diagnostic| {
            diagnostic.code != DiagnosticCode::InvalidValue
                || (!diagnostic.message.contains("has_saved_event_target")
                    && !diagnostic.message.contains("set_ruler")
                    && !diagnostic.message.contains("set_heir"))
        }),
        "runtime targets and locally exiled characters must validate: {diagnostics:?}"
    );
}

#[test]
fn vanilla_dynamic_names_empty_event_lists_and_inherited_contexts_are_valid() {
    let rules = pdx_game::eu4::first_party_rules().expect("first-party rules");
    for (path, text, forbidden) in [
        (
            "/tmp/common/on_actions/test.txt",
            "on_test = { events = { } }\n",
            "value of `events`",
        ),
        (
            "/tmp/common/policies/test.txt",
            "test_policy = { monarch_power = ADM potential = { always = yes } allow = { always = yes } global_tax_modifier = 0.1 }\n",
            "unexpected key `global_tax_modifier`",
        ),
        (
            "/tmp/common/triggered_modifiers/test.txt",
            "test_modifier = { potential = { always = yes } trigger = { always = yes } global_unrest = -1 }\n",
            "unexpected key `global_unrest`",
        ),
        (
            "/tmp/common/ruler_personalities/test.txt",
            "test_personality = { nation_designer_cost = 0 gift_chance = 10 }\n",
            "unexpected key `gift_chance`",
        ),
        (
            "/tmp/events/test.txt",
            "country_event = { id = test.1 trigger = { dynasty = \"de' Medici\" has_heir = \"Ladislaus Postumus\" culture_group = owner religion_group = owner } }\n",
            "does not match the semantic rule",
        ),
    ] {
        let mut host = eu4_host(rules.clone());
        let id = DocumentId::new(format!("file://{path}"));
        host.open_document(
            id.clone(),
            1,
            text.to_owned(),
            Some(std::path::PathBuf::from(path)),
        )
        .expect("open Vanilla-shaped fixture");
        let results = diagnostics(&host.snapshot(), &id);
        assert!(
            results
                .iter()
                .all(|diagnostic| !diagnostic.message.contains(forbidden)),
            "{path} retained `{forbidden}`: {results:?}"
        );
    }
}

#[test]
fn vanilla_dates_filtered_sprite_roots_and_runtime_tags_do_not_false_positive() {
    let rules = pdx_game::eu4::first_party_rules().expect("first-party rules");
    for (path, text, forbidden) in [
        (
            "/tmp/history/provinces/1 - Test.txt",
            "owner = FRA\n1444.11.11 = { add_local_autonomy = 10 }\n",
            "1444.11.11",
        ),
        (
            "/tmp/interface/test.gfx",
            "spriteTypes = { pdxmesh = { name = test_mesh file = test.mesh } spriteType = { name = test texturefile = test.dds } }\n",
            "pdxmesh",
        ),
        (
            "/tmp/events/test.txt",
            "country_event = { id = test.1 trigger = { tag = F00 tag = T99 } }\n",
            "value of `tag`",
        ),
        (
            "/tmp/events/random-list.txt",
            "country_event = { id = test.2 immediate = { random_list = { 11 = { trigger = { always = yes } add_treasury = 1 } } } }\n",
            "unexpected key `trigger`",
        ),
        (
            "/tmp/common/church_aspects/test.txt",
            "test_aspect = { cost = 1 modifier = { monthly_asha_vahishta = 0.5 } }\n",
            "monthly_asha_vahishta",
        ),
    ] {
        let mut host = eu4_host(rules.clone());
        let id = DocumentId::new(format!("file://{path}"));
        host.open_document(
            id.clone(),
            1,
            text.to_owned(),
            Some(std::path::PathBuf::from(path)),
        )
        .expect("open Vanilla-shaped fixture");
        let results = diagnostics(&host.snapshot(), &id);
        assert!(
            results
                .iter()
                .all(|diagnostic| !diagnostic.message.contains(forbidden)),
            "{path} retained `{forbidden}`: {results:?}"
        );
    }
}

#[test]
fn nested_government_mechanic_powers_feed_dynamic_value_validation() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-analysis-government-powers-{nonce}"));
    let mechanics = root.join("common/government_mechanics");
    std::fs::create_dir_all(&mechanics).expect("mechanics directory");
    std::fs::write(
        mechanics.join("00_test.txt"),
        "test_mechanic = { powers = { test_power = { max = 100 } } }\n",
    )
    .expect("mechanic source");
    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(1),
        SourceRootKind::CurrentMod,
        root.clone(),
    )]));
    host.refresh_source_roots().expect("scan mechanic power");
    let id = DocumentId::new("file:///tmp/events/government-power.txt");
    let text = "country_event = { id = power.1 immediate = { add_government_power = { mechanic_type = test_mechanic power_type = test_power value = 1 } } }\n";
    host.open_document(id.clone(), 1, text.to_owned(), None)
        .expect("open event");

    let diagnostics = diagnostics(&host.snapshot(), &id);
    assert!(
        diagnostics.iter().all(|diagnostic| {
            diagnostic.code != DiagnosticCode::InvalidValue
                || !diagnostic.message.contains("power_type")
        }),
        "nested powers should enter the workspace index: {diagnostics:?}"
    );
    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn typed_name_fields_do_not_inherit_the_localisation_fallback() {
    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    let id = DocumentId::new("file:///tmp/events/typed-name.txt");
    let text = "country_event = { id = names.1 option = { name = missing_option_loc } immediate = { add_country_modifier = { name = runtime_modifier duration = 1 } } }\n";
    host.open_document(id.clone(), 1, text.to_owned(), None)
        .expect("open event");

    let diagnostics = diagnostics(&host.snapshot(), &id);
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.code == DiagnosticCode::UnknownSymbol
            && diagnostic.message.contains("missing_option_loc")
    }));
    assert!(
        diagnostics.iter().all(|diagnostic| {
            diagnostic.code != DiagnosticCode::UnknownSymbol
                || !diagnostic.message.contains("runtime_modifier")
        }),
        "modifier identifiers must not be treated as localisation: {diagnostics:?}"
    );
}

#[test]
fn exported_modifier_keys_are_numeric_modifier_rules() {
    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(1),
        SourceRootKind::CurrentMod,
        std::path::PathBuf::from("/tmp"),
    )]));
    let id = DocumentId::new("file:///tmp/common/advisortypes/test.txt");
    host.open_document(
        id.clone(),
        1,
        "philosopher = { monarch_power = ADM prestige = 1 modifier = { meritocracy = 1 monthly_russian_modernization = 0.02 } }\n"
            .to_owned(),
        Some(std::path::PathBuf::from(
            "/tmp/common/advisortypes/test.txt",
        )),
    )
    .expect("open advisor type");

    let results = diagnostics(&host.snapshot(), &id);
    assert!(
        results.iter().all(|item| {
            (!item.message.contains("meritocracy")
                && !item.message.contains("monthly_russian_modernization")
                && !item.message.contains("prestige"))
                || !matches!(
                    item.code,
                    DiagnosticCode::UnknownKey | DiagnosticCode::InvalidValue
                )
        }),
        "exported modifier names must accept numeric values: {results:?}"
    );
}

#[test]
fn vanilla_powerprojection_file_and_static_modifier_blocks_validate() {
    let rules = pdx_game::eu4::first_party_rules().expect("first-party rules");
    for (path, text, forbidden) in [
        (
            "/tmp/common/powerprojection/00_static.txt",
            concat!(
                "great_power_1 = { power = 25 }\n",
                "MAY_reform_passed = { max = 10 min = 0 yearly_decay = 1 }\n",
                "zim_african_great_power = { max = 25 min = 25 power = 1 decay = no }\n",
            ),
            "power",
        ),
        (
            "/tmp/common/static_modifiers/test.txt",
            "power_projection = { defensiveness = 0.1 global_trade_power = 0.2 prestige = 0.5 }\n",
            "power_projection",
        ),
    ] {
        let mut host = eu4_host(rules.clone());
        let id = DocumentId::new(format!("file://{path}"));
        host.open_document(
            id.clone(),
            1,
            text.to_owned(),
            Some(std::path::PathBuf::from(path)),
        )
        .expect("open Vanilla-shaped fixture");
        let results = diagnostics(&host.snapshot(), &id);
        assert!(
            results.iter().all(|diagnostic| {
                diagnostic.code != DiagnosticCode::UnknownKey
                    && !diagnostic.message.contains(forbidden)
            }),
            "{path} retained unexpected diagnostics: {results:?}"
        );
    }
}

#[test]
fn severity_review_quoted_names_and_non_instance_scalars_are_not_localisation() {
    let rules = pdx_game::eu4::first_party_rules().expect("first-party rules");
    for (path, text, forbidden) in [
        (
            "/tmp/history/countries/FRA - France.txt",
            "FRA = {\n\tmonarch = {\n\t\tname = \"Charles VI\"\n\t\tname = Charles\n\t}\n\trevolt = { type = nationalist_rebels size = 2 name = \"Les Gueux\" }\n}\n",
            "localisation symbol",
        ),
        (
            "/tmp/interface/ages_view.gfx",
            "spriteTypes = { pdxmesh = { name = \"aow_reformation_reformed_mesh\" file = test.mesh } }\n",
            "localisation symbol",
        ),
        (
            "/tmp/common/religions/00_religion.txt",
            "christian = {\n\tdefender_of_faith = yes\n\tcan_form_personal_unions = yes\n}\n",
            "`defender_of_faith`",
        ),
    ] {
        let mut host = eu4_host(rules.clone());
        let id = DocumentId::new(format!("file://{path}"));
        host.open_document(
            id.clone(),
            1,
            text.to_owned(),
            Some(std::path::PathBuf::from(path)),
        )
        .expect("open Vanilla-shaped fixture");
        let results = diagnostics(&host.snapshot(), &id);
        assert!(
            results.iter().all(|diagnostic| {
                diagnostic.code != DiagnosticCode::UnknownSymbol
                    || (!diagnostic.message.contains(forbidden)
                        && !diagnostic.message.contains("`Charles`"))
            }),
            "{path} retained literal-text false positives: {results:?}"
        );
    }
}

#[test]
fn severity_review_unknown_keys_are_errors_and_known_keys_are_accepted() {
    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    let id = DocumentId::new("file:///tmp/map/terrain.txt");
    host.open_document(
        id.clone(),
        1,
        "grasslands = { type = grasslands color = { 0 1 2 } }\ncompletely_wrong_key = { type = grasslands }\n"
            .to_owned(),
        Some(std::path::PathBuf::from("/tmp/map/terrain.txt")),
    )
    .expect("open terrain");
    let terrain_results = diagnostics(&host.snapshot(), &id);
    assert!(
        terrain_results.iter().all(|diagnostic| {
            diagnostic.code != DiagnosticCode::UnknownKey
                || !diagnostic.message.contains("grasslands")
        }),
        "terrain names must be accepted: {terrain_results:?}"
    );

    let technology = DocumentId::new("file:///tmp/common/technology.txt");
    host.open_document(
        technology.clone(),
        1,
        "groups = { adm = { adm_tech = \"technologies/adm.txt\" totally_wrong = 1 } }\n".to_owned(),
        Some(std::path::PathBuf::from("/tmp/common/technology.txt")),
    )
    .expect("open technology");
    let technology_results = diagnostics(&host.snapshot(), &technology);
    assert!(
        technology_results.iter().all(|diagnostic| {
            diagnostic.code != DiagnosticCode::UnknownKey
                || !diagnostic.message.contains("adm_tech")
        }),
        "technology table keys must be accepted: {technology_results:?}"
    );
    let unknown = technology_results
        .iter()
        .find(|diagnostic| {
            diagnostic.code == DiagnosticCode::UnknownKey
                && diagnostic.message.contains("totally_wrong")
        })
        .unwrap_or_else(|| panic!("unknown key must be reported: {technology_results:?}"));
    assert_eq!(
        unknown.severity, 1,
        "unknown keys are errors: {technology_results:?}"
    );
}

#[test]
fn severity_review_fallback_unknown_keys_and_unknown_bare_values_are_errors() {
    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));

    // `natives_test` is selected through the path-only type fallback. That uncertainty must not
    // turn a key the game ignores into a warning.
    let unknown_key = DocumentId::new("file:///tmp/common/natives/unknown-key.txt");
    host.open_document(
        unknown_key.clone(),
        1,
        "natives_test = { definitely_wrong = yes }\n".to_owned(),
        Some(std::path::PathBuf::from(
            "/tmp/common/natives/unknown-key.txt",
        )),
    )
    .expect("open fallback-context key fixture");
    let key_diagnostics = diagnostics(&host.snapshot(), &unknown_key);
    let unknown = key_diagnostics
        .iter()
        .find(|diagnostic| {
            diagnostic.code == DiagnosticCode::UnknownKey
                && diagnostic.message.contains("definitely_wrong")
        })
        .unwrap_or_else(|| panic!("unknown key must be reported: {key_diagnostics:?}"));
    assert_eq!(unknown.severity, 1, "unknown keys are errors");

    // A numeric range overflow is an intentional first-party warning, but a non-numeric value
    // that cannot be understood by the same leaf-value clause is an error.
    let unknown_bare = DocumentId::new("file:///tmp/common/natives/unknown-bare.txt");
    host.open_document(
        unknown_bare.clone(),
        1,
        "natives_test = { color = { not_a_color_component } }\n".to_owned(),
        Some(std::path::PathBuf::from(
            "/tmp/common/natives/unknown-bare.txt",
        )),
    )
    .expect("open unknown bare value fixture");
    let bare_diagnostics = diagnostics(&host.snapshot(), &unknown_bare);
    let bare = bare_diagnostics
        .iter()
        .find(|diagnostic| {
            diagnostic.code == DiagnosticCode::UnknownBareValue
                && diagnostic.message.contains("not_a_color_component")
        })
        .unwrap_or_else(|| panic!("unknown bare value must be reported: {bare_diagnostics:?}"));
    assert_eq!(bare.severity, 1, "unknown bare values are errors");
}

#[test]
fn severity_review_incident_options_keep_trigger_wrappers() {
    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    let id = DocumentId::new("file:///tmp/common/imperial_incidents/00_test.txt");
    host.open_document(
        id.clone(),
        1,
        "incident_test = {\n\tevent = test.1\n\tdefault_option = 0\n\toption = {\n\t\tOR = {\n\t\t\tNOT = { emperor = { is_rival = TEU } }\n\t\t}\n\t}\n}\n"
            .to_owned(),
        Some(std::path::PathBuf::from(
            "/tmp/common/imperial_incidents/00_test.txt",
        )),
    )
    .expect("open incident");
    let results = diagnostics(&host.snapshot(), &id);
    assert!(
        results.iter().all(|diagnostic| {
            diagnostic.code != DiagnosticCode::UnknownKey
                || (!diagnostic.message.contains("`OR`") && !diagnostic.message.contains("`NOT`"))
        }),
        "trigger wrappers must stay valid inside incident options: {results:?}"
    );
}

#[test]
fn severity_review_color_overflow_is_a_warning_and_missing_ruler_an_error() {
    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    let natives = DocumentId::new("file:///tmp/common/natives/00_test.txt");
    host.open_document(
        natives.clone(),
        1,
        "natives_test = { graphical_culture = inuitgfx color = { 0 255 400 } }\n".to_owned(),
        Some(std::path::PathBuf::from("/tmp/common/natives/00_test.txt")),
    )
    .expect("open natives");
    let natives_results = diagnostics(&host.snapshot(), &natives);
    let overflow = natives_results
        .iter()
        .find(|diagnostic| {
            diagnostic.code == DiagnosticCode::InvalidValue && diagnostic.message.contains("400")
        })
        .unwrap_or_else(|| panic!("color overflow must be reported: {natives_results:?}"));
    assert_eq!(overflow.severity, 2, "clamped color values are warnings");

    let event = DocumentId::new("file:///tmp/events/ruler-missing.txt");
    host.open_document(
        event.clone(),
        1,
        "country_event = { immediate = { set_ruler = bloody_mary } }\n".to_owned(),
        Some(std::path::PathBuf::from("/tmp/events/ruler-missing.txt")),
    )
    .expect("open event");
    let event_results = diagnostics(&host.snapshot(), &event);
    let missing_ruler = event_results
        .iter()
        .find(|diagnostic| {
            diagnostic.code == DiagnosticCode::InvalidValue
                && diagnostic.message.contains("set_ruler")
        })
        .unwrap_or_else(|| panic!("missing exiled ruler must be reported: {event_results:?}"));
    assert_eq!(missing_ruler.severity, 1, "missing ruler is an error");
}

#[test]
fn vanilla_optional_fields_and_disaster_weights_preserve_their_actual_shapes() {
    let rules = pdx_game::eu4::first_party_rules().expect("first-party rules");
    let model = rules.model();
    for id in [
        "common/buildings:33:rule:root:building:manufactory",
        "common/casus_belli_and_war_goals:4:rule:type:casus_belli:is_triggered_only",
        "common/ideas_and_native_advancements:76:rule:root:customideas:ai_will_do",
        "common/ideas_and_native_advancements:77:rule:root:customideas:category",
        "common/religions_and_related:396:rule:root:aspects_and_blessings:is_blessing",
        "common/religions_and_related:79:rule:type:aspects_and_blessings:is_blessing",
        "common/buildings:3:rule:type:building:cost",
        "common/buildings:7:rule:root:building:cost",
        "common/buildings:8:rule:root:building:time",
        "common/religions_and_related:380:rule:root:aspects_and_blessings:cost",
        "common/diplomatic_actions_new:42:rule:root:new_diplomatic_action:ai_value",
        "common/governments_and_reforms:33:rule:type:government_name:government_reform",
        "common/governments_and_reforms:600:rule:root:government_name:government_reform",
        "common/ideas_and_native_advancements:37:rule:root:idea_group:ai_will_do",
        "common/ideas_and_native_advancements:38:rule:root:idea_group:category",
        "common/ideas_and_native_advancements:6:rule:type:idea_group:category",
        "common/modifiers_consolidated:1:rule:type:event_modifier:scalar",
        "common/imperial_reforms:7:rule:type:imperial_reform:emperor",
        "common/religions_and_related:114:leaf-value:root:religion_group:int[0..255]",
        "common/religions_and_related:116:leaf-value:root:religion_group:float[0.0..1.0]",
        "common/religions_and_related:355:rule:root:religion_propagation:trading_policy",
        "common/religions_and_related:63:rule:type:religion_propagation:trading_policy",
    ] {
        let rule = model
            .semantic
            .rules
            .iter()
            .find(|rule| rule.id == id)
            .unwrap_or_else(|| panic!("missing semantic rule {id}"));
        assert_eq!(rule.min_occurs, Some(0), "{id} is optional in Vanilla");
    }
    for id in [
        "common/disasters:28:rule:root:disaster:int",
        "common/disasters:29:rule:root:disaster:int",
    ] {
        let rule = model
            .semantic
            .rules
            .iter()
            .find(|rule| rule.id == id)
            .unwrap_or_else(|| panic!("missing semantic rule {id}"));
        assert_eq!(rule.key, KeyMatcher::AnyScalar);
    }
    let disaster_events = model
        .semantic
        .rules
        .iter()
        .find(|rule| rule.id == "common/disasters:27:rule:root:disaster:events")
        .expect("disaster events rule");
    assert_eq!(disaster_events.shape, RuleShape::Node);

    let on_action_weights = model
        .semantic
        .rules
        .iter()
        .filter(|rule| {
            rule.context == "root:on_action"
                && rule.parent_path == ["random_events"]
                && rule.key == KeyMatcher::AnyScalar
        })
        .count();
    assert_eq!(on_action_weights, 452);
    assert!(
        model.semantic.rules.iter().all(|rule| {
            !matches!(&rule.value, ValueMatcher::Exact(value) if value == "localisation_synced")
        }),
        "the source sentinel must compile to an unconstrained scalar matcher"
    );
    let date_keys = model
        .semantic
        .rules
        .iter()
        .filter(|rule| rule.key == KeyMatcher::Date)
        .count();
    assert_eq!(date_keys, 7);
    let event_picture = model
        .semantic
        .rules
        .iter()
        .find(|rule| rule.id == "events/events:17:rule:root:event:picture")
        .expect("event picture rule");
    assert_eq!(event_picture.max_occurs, None);
    let government_rank_keys = model
        .semantic
        .rules
        .iter()
        .filter(|rule| {
            rule.context == "root:government_name"
                && rule.key == KeyMatcher::AnyScalar
                && rule.parent_path.first().is_some_and(|parent| {
                    [
                        "rank",
                        "ruler_male",
                        "ruler_female",
                        "consort_male",
                        "consort_female",
                        "heir_male",
                        "heir_female",
                    ]
                    .contains(&parent.as_str())
                })
        })
        .count();
    assert_eq!(government_rank_keys, 14);
    // Color components are ints 0..255; Vanilla ships `color = { 5 371 129 }`
    // (state_edicts/zzz_chinese_industrialization.txt) and the game clamps
    // out-of-range components, so the rules keep the bound but report at warning
    // severity (runs fine, but imperfect).
    for id in [
        "common/00_small_types_consolidated:207:leaf-value:root:edict:int[0..255",
        "common/trade_consolidated:54:leaf-value:root:trade_company:int[0..255",
    ] {
        let rule = model
            .semantic
            .rules
            .iter()
            .find(|rule| rule.id == id)
            .unwrap_or_else(|| panic!("missing semantic rule {id}"));
        assert_eq!(
            rule.value,
            ValueMatcher::Int {
                min: Some(0),
                max: Some(255),
            }
        );
        assert_eq!(rule.severity, Some(2), "{id} must report as a warning");
    }
    assert!(model.semantic.rules.iter().any(|rule| {
        rule.id == "eu4:trade_node:outgoing_path"
            && rule.parent_path == ["outgoing", "path"]
            && rule.key == KeyMatcher::AnyScalar
    }));
}

#[test]
fn vanilla_leader_names_and_custom_idea_metadata_are_not_false_symbols() {
    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    let achievement = DocumentId::new("file:///tmp/common/achievements.txt");
    host.open_document(
        achievement.clone(),
        1,
        "achievement = { happened = { has_leader = \"The French Paradox\" } }\n".to_owned(),
        None,
    )
    .expect("open achievement");
    let achievement_diagnostics = diagnostics(&host.snapshot(), &achievement);
    assert!(
        achievement_diagnostics.iter().all(|diagnostic| {
            diagnostic.code != DiagnosticCode::InvalidValue
                || !diagnostic.message.contains("has_leader")
        }),
        "literal leader names are valid: {achievement_diagnostics:?}"
    );

    let custom_ideas = DocumentId::new("file:///tmp/common/custom_ideas/test.txt");
    host.open_document(
        custom_ideas.clone(),
        1,
        "group = { category = ADM custom_idea = { global_tax_modifier = 0.05 } }\n".to_owned(),
        None,
    )
    .expect("open custom ideas");
    let custom_diagnostics = diagnostics(&host.snapshot(), &custom_ideas);
    assert!(
        custom_diagnostics
            .iter()
            .all(|diagnostic| !diagnostic.message.contains("category_desc")),
        "metadata keys are not custom-idea definitions: {custom_diagnostics:?}"
    );
    assert!(
        custom_diagnostics.iter().all(|diagnostic| {
            diagnostic.code != DiagnosticCode::UnknownKey
                || !diagnostic.message.contains("global_tax_modifier")
        }),
        "custom ideas inherit modifier keys: {custom_diagnostics:?}"
    );
}

#[test]
fn common_alerts_and_units_display_use_path_specific_semantics() {
    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));

    let alerts = DocumentId::new("file:///tmp/common/alerts.txt");
    host.open_document(
        alerts.clone(),
        1,
        "sound = { HIGH = new_alert MEDIUM = new_alert LOW = new_alert }\nicon = { HIGH = GFX_alerticon_banner }\nalerts = { alert_bankrupt = { category = HIGH } }\n"
            .to_owned(),
        Some(std::path::PathBuf::from("/tmp/common/alerts.txt")),
    )
    .expect("open alerts");

    let tags = DocumentId::new("file:///tmp/common/country_tags/test.txt");
    host.open_document(
        tags.clone(),
        1,
        "SPA = \"countries/Spain.txt\"\nCAS = \"countries/Castile.txt\"\n".to_owned(),
        Some(std::path::PathBuf::from(
            "/tmp/common/country_tags/test.txt",
        )),
    )
    .expect("open country tags");
    let alert_diagnostics = diagnostics(&host.snapshot(), &alerts);
    assert!(
        alert_diagnostics.iter().all(|diagnostic| {
            diagnostic.code != DiagnosticCode::UnknownKey
                && diagnostic.code != DiagnosticCode::UnknownBareValue
        }),
        "valid alert declarations must use the alerts rule contexts: {alert_diagnostics:?}"
    );

    let units = DocumentId::new("file:///tmp/common/units_display/test.txt");
    host.open_document(
        units.clone(),
        1,
        "cavalry = { factor = 1 modifier = { factor = 3 OR = { always = yes } } modifier = { factor = 2 has_country_flag = MUG_more_chance_for_elephants_flag } }\n".to_owned(),
        Some(std::path::PathBuf::from(
            "/tmp/common/units_display/test.txt",
        )),
    )
    .expect("open units display");
    let unit_diagnostics = diagnostics(&host.snapshot(), &units);
    assert!(
        unit_diagnostics.iter().all(|diagnostic| {
            diagnostic.code != DiagnosticCode::UnknownKey
                && diagnostic.code != DiagnosticCode::UnknownBareValue
        }),
        "unit display factors and trigger clauses must use the path-specific rules: {unit_diagnostics:?}"
    );

    let lucky = DocumentId::new("file:///tmp/common/historial_lucky.txt");
    host.open_document(
        lucky.clone(),
        1,
        "CAS = { NOT = { exists = SPA } is_year = 1700 }\n".to_owned(),
        Some(std::path::PathBuf::from("/tmp/common/historial_lucky.txt")),
    )
    .expect("open historial lucky");
    let lucky_diagnostics = diagnostics(&host.snapshot(), &lucky);
    assert!(
        lucky_diagnostics.iter().all(|diagnostic| {
            diagnostic.code != DiagnosticCode::UnknownKey
                && diagnostic.code != DiagnosticCode::UnknownBareValue
        }),
        "lucky-country trigger blocks must use the trigger context: {lucky_diagnostics:?}"
    );
    assert!(
        lucky_diagnostics.is_empty(),
        "scalar triggers directly inside lucky-country blocks must validate against the trigger rules, not the file root rule: {lucky_diagnostics:?}"
    );
}

#[test]
fn lucky_country_blocks_accept_scalar_triggers() {
    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(1),
        SourceRootKind::CurrentMod,
        std::path::PathBuf::from("/tmp"),
    )]));
    let lucky = DocumentId::new("file:///tmp/common/historial_lucky.txt");
    host.open_document(
        lucky.clone(),
        1,
        "RUS = { always = yes }\nPRU = { is_year = 1700 always = no }\n".to_owned(),
        Some(std::path::PathBuf::from("/tmp/common/historial_lucky.txt")),
    )
    .expect("open historial lucky");
    let lucky_diagnostics = diagnostics(&host.snapshot(), &lucky);
    assert!(
        lucky_diagnostics.is_empty(),
        "boolean and scalar triggers directly inside lucky-country blocks are valid: {lucky_diagnostics:?}"
    );
}

#[test]
fn imperial_incident_entries_keep_their_structured_body_context() {
    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(1),
        SourceRootKind::CurrentMod,
        std::path::PathBuf::from("/tmp"),
    )]));
    let id = DocumentId::new("file:///tmp/common/imperial_incidents/00_test.txt");
    host.open_document(
        id.clone(),
        1,
        concat!(
            "incident_probe = {\n",
            "    event = incidents_probe.1\n",
            "    default_option = 0\n",
            "    can_stop = { NOT = { exists = BUR } }\n",
            "    0 = { factor = 1 }\n",
            "    1 = {\n",
            "        factor = 1\n",
            "        modifier = { factor = 100 is_year = 1500 }\n",
            "    }\n",
            "}\n",
        )
        .to_owned(),
        Some(std::path::PathBuf::from(
            "/tmp/common/imperial_incidents/00_test.txt",
        )),
    )
    .expect("open imperial incident");
    let results = diagnostics(&host.snapshot(), &id);
    assert!(
        results
            .iter()
            .all(|diagnostic| diagnostic.code != DiagnosticCode::UnknownKey),
        // The any_scalar option wrapper describes the numeric entry children,
        // not the entry key itself; entry bodies keep the structured context.
        "imperial incident entry bodies must validate against their own keys: {results:?}"
    );
}

#[test]
fn type_per_file_rules_validate_the_document_root_once() {
    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(1),
        SourceRootKind::CurrentMod,
        std::path::PathBuf::from("/tmp"),
    )]));
    let id = DocumentId::new("file:///tmp/common/countries/Test.txt");
    host.open_document(
        id.clone(),
        1,
        "graphical_culture = westerngfx\ncolor = { 20 50 210 }\nleader_names = { Blittersdorf \"von Gelnhausen\" }\n"
            .to_owned(),
        Some(std::path::PathBuf::from("/tmp/common/countries/Test.txt")),
    )
    .expect("open country file");

    let results = diagnostics(&host.snapshot(), &id);
    assert!(
        results.iter().all(|item| {
            item.code != DiagnosticCode::InvalidValue
                || !["`20`", "`50`", "`210`"]
                    .iter()
                    .any(|value| item.message.contains(value))
        }),
        "country color values must be validated inside the document root: {results:?}"
    );
    assert!(
        results.iter().all(|item| {
            item.code != DiagnosticCode::InvalidValue
                || (!item.message.contains("graphical_culture")
                    && !item.message.contains("Blittersdorf")
                    && !item.message.contains("von Gelnhausen"))
        }),
        "first-party graphical cultures and literal leader names must be accepted: {results:?}"
    );
    let missing_color = results
        .iter()
        .filter(|item| item.code == DiagnosticCode::Cardinality && item.message.contains("`color`"))
        .count();
    assert_eq!(
        missing_color, 0,
        "the present file-level color field must satisfy cardinality: {results:?}"
    );
    let missing_monarch_names = results
        .iter()
        .filter(|item| {
            item.code == DiagnosticCode::Cardinality && item.message.contains("`monarch_names`")
        })
        .count();
    assert_eq!(
        missing_monarch_names, 1,
        "missing file-level fields must be reported once: {results:?}"
    );
}

#[test]
fn evicted_frontend_diagnostics_match_retained() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-evict-debug-{nonce}"));
    std::fs::create_dir_all(root.join("events")).unwrap();
    std::fs::write(root.join("events/invalid.txt"), "scope = nowhere\n").unwrap();
    let root = root.canonicalize().unwrap();
    let cleanup = root.clone();
    let mut host = crate::tests::support::eu4_host(pdx_game::eu4::first_party_rules().unwrap());
    host.apply_change(pdx_engine::WorkspaceChange::SetSourceRoots(vec![
        pdx_engine::SourceRoot::new(
            pdx_engine::SourceRootId::new(0),
            pdx_engine::SourceRootKind::CurrentMod,
            root,
        ),
    ]));
    host.refresh_source_roots().unwrap();
    let snapshot = host.snapshot();
    let file_id = *snapshot.source_files().keys().next().unwrap();
    let retained = crate::source_file_diagnostics_with_cancellation(
        &snapshot,
        file_id,
        &crate::CancellationToken::new(),
    )
    .unwrap();
    eprintln!("retained diagnostics: {retained:?}");
    drop(snapshot);
    let evicted = host.evict_source_frontends(&|_| false);
    eprintln!("evicted: {evicted}");
    let snapshot2 = host.snapshot();
    let state = snapshot2.file_state(file_id).unwrap();
    eprintln!(
        "post-evict parsed: {}, source len: {}",
        state.parsed().is_some(),
        state.source().len()
    );
    let after = crate::source_file_diagnostics_with_cancellation(
        &snapshot2,
        file_id,
        &crate::CancellationToken::new(),
    )
    .unwrap();
    eprintln!("evicted diagnostics: {after:?}");
    assert_eq!(
        retained.len(),
        after.len(),
        "evicted diagnostics must match"
    );
    std::fs::remove_dir_all(cleanup).unwrap();
}

#[test]
fn logic_container_lints_fire_on_degenerate_shapes() {
    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(1),
        SourceRootKind::CurrentMod,
        std::path::PathBuf::from("/tmp"),
    )]));
    let id = DocumentId::new("file:///tmp/events/lint_probe.txt");
    host.open_document(
        id.clone(),
        1,
        concat!(
            "country_event = { id = lint.1\n",
            "  trigger = {\n",
            "    NOT = { always = yes is_year = 1500 }\n",
            "    OR = { has_country_flag = lint_flag }\n",
            "    AND = { }\n",
            "  }\n",
            "  option = { name = lint.1.a\n",
            "    if = { limit = { always = yes } add_prestige = 1\n",
            "      else = { add_prestige = 4 }\n",
            "    }\n",
            "    else = { add_prestige = 2 }\n",
            "    ai_chance = { factor = 0 }\n",
            "  }\n",
            "  option = { name = lint.1.b\n",
            "    else = { add_prestige = 3 }\n",
            "    has_country_flag = lint_flag\n",
            "  }\n",
            "}\n",
            "country_event = { id = lint.2 trigger = { add_prestige = 1 } option = { name = lint.2.a } }\n",
        )
        .to_owned(),
        Some(std::path::PathBuf::from("/tmp/events/lint_probe.txt")),
    )
    .expect("open lint probe");
    let all = diagnostics(&host.snapshot(), &id);
    let codes: Vec<(DiagnosticCode, String)> = all
        .iter()
        .map(|diagnostic| (diagnostic.code, diagnostic.message.clone()))
        .collect();

    assert!(
        codes.iter().any(|(code, message)| {
            *code == DiagnosticCode::LogicalContainer && message.contains("AND of NOTs")
        }),
        "NOT with multiple conditions must explain the NOR reading: {codes:?}"
    );
    assert!(
        codes.iter().any(|(code, message)| {
            *code == DiagnosticCode::LogicalContainer && message.contains("single condition")
        }),
        "single-condition OR must be flagged: {codes:?}"
    );
    assert!(
        codes
            .iter()
            .any(|(code, message)| *code == DiagnosticCode::LogicalContainer
                && message.contains("empty `AND`")),
        "empty AND container must be flagged: {codes:?}"
    );
    assert!(
        codes
            .iter()
            .any(|(code, message)| *code == DiagnosticCode::ConstantCondition
                && message.contains("always true")),
        "constant always-yes limit must be flagged: {codes:?}"
    );
    assert_eq!(
        codes
            .iter()
            .filter(|(code, _)| *code == DiagnosticCode::OrphanElse)
            .count(),
        1,
        "exactly the option-level else is orphaned; sibling and nested forms after an `if` are valid: {codes:?}"
    );
    assert!(
        codes
            .iter()
            .any(|(code, message)| *code == DiagnosticCode::UnknownKey
                && message.contains("is an effect and cannot be used inside a trigger block")),
        "effect-in-trigger must sharpen the unknown-key message: {codes:?}"
    );
    assert!(
        codes
            .iter()
            .any(|(code, message)| *code == DiagnosticCode::UnknownKey
                && message.contains("is a trigger and cannot be used inside an effect block")),
        "trigger-in-effect must sharpen the unknown-key message: {codes:?}"
    );
    assert!(
        !all.iter()
            .any(|diagnostic| diagnostic.message.contains("unexpected key `add_prestige`")),
        "the sharpened effect-in-trigger message replaces the generic wording: {codes:?}"
    );
    // A deliberate `ai_chance = { factor = 0 }` (AI never picks this option)
    // is a standard EU4 idiom and must stay silent.
    assert!(
        !all.iter()
            .any(|diagnostic| diagnostic.message.contains("never chooses")),
        "zero AI weight is intentional authoring and is not diagnosed: {codes:?}"
    );
}

#[test]
fn scripted_macro_cycles_are_reported_at_definition_sites() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-analysis-macro-cycles-{nonce}"));
    let effects = root.join("common/scripted_effects");
    std::fs::create_dir_all(&effects).expect("scripted effects directory");
    std::fs::write(
        effects.join("00_cycles.txt"),
        concat!(
            "ping = { pong = yes }\n",
            "pong = { ping = yes }\n",
            "loop_self = { loop_self = yes }\n",
            "honest = { add_prestige = 1 }\n",
            "dispatch = { $action$ = yes }\n",
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

    // The dynamic dispatch cycle only closes through a call-site binding:
    // `dispatch` renders its key to `relay`, and `relay` calls `dispatch`.
    std::fs::write(
        effects.join("01_dispatch.txt"),
        "relay = { dispatch = { action = relay } }\n",
    )
    .expect("dispatch definition");
    host.refresh_source_roots().expect("rescan dispatch");

    let definitions = DocumentId::new("file:///tmp/common/scripted_effects/00_cycles.txt");
    let definition_text = std::fs::read_to_string(effects.join("00_cycles.txt")).expect("text");
    host.open_document(
        definitions.clone(),
        1,
        definition_text.clone(),
        Some(effects.join("00_cycles.txt")),
    )
    .expect("open definitions");
    let all = diagnostics(&host.snapshot(), &definitions);
    let cycles: Vec<&Diagnostic> = all
        .iter()
        .filter(|diagnostic| diagnostic.code == DiagnosticCode::MacroExpansionCycle)
        .collect();

    let ping_offset = u32::try_from(definition_text.find("ping = ").expect("ping")).expect("u32");
    assert!(
        cycles.iter().any(|diagnostic| {
            diagnostic.message.contains("`ping`")
                && diagnostic.message.contains("ping -> pong -> ping")
                && diagnostic.range.start() == ping_offset
        }),
        "mutual recursion must be reported at the definition site: {cycles:?}"
    );
    let pong_offset = u32::try_from(
        definition_text
            .find("\npong = ")
            .map(|offset| offset + 1)
            .expect("pong"),
    )
    .expect("u32");
    assert!(
        cycles
            .iter()
            .any(|diagnostic| diagnostic.range.start() == pong_offset),
        "both cycle participants are reported at their definitions: {cycles:?}"
    );
    let self_offset =
        u32::try_from(definition_text.find("loop_self = ").expect("self")).expect("u32");
    assert!(
        cycles.iter().any(|diagnostic| {
            diagnostic.message.contains("loop_self -> loop_self")
                && diagnostic.range.start() == self_offset
        }),
        "self-recursion must be reported at the definition site: {cycles:?}"
    );
    let dispatch_offset =
        u32::try_from(definition_text.find("dispatch = ").expect("dispatch")).expect("u32");
    assert!(
        cycles.iter().any(|diagnostic| {
            diagnostic.message.contains("`dispatch`") && diagnostic.range.start() == dispatch_offset
        }),
        "the parameter-bound dispatch cycle must close through call-site bindings: {cycles:?}"
    );
    assert!(
        !cycles
            .iter()
            .any(|diagnostic| diagnostic.message.contains("`honest`")),
        "acyclic macros stay unreported: {cycles:?}"
    );

    // The runtime expansion guard stays silent for statically known cycles;
    // call sites must not re-report what the definitions already say.
    let event = DocumentId::new("file:///tmp/events/cycle_call.txt");
    host.open_document(
        event.clone(),
        1,
        "country_event = { id = cycle.1 immediate = { ping = yes } }\n".to_owned(),
        Some(std::path::PathBuf::from("/tmp/events/cycle_call.txt")),
    )
    .expect("open call site");
    let event_diagnostics = diagnostics(&host.snapshot(), &event);
    assert!(
        !event_diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == DiagnosticCode::MacroExpansionCycle),
        "call sites no longer duplicate definition-site cycle reports: {event_diagnostics:?}"
    );
    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn scripted_macro_scope_contracts_infer_and_reject_empty_intersections() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-analysis-macro-contracts-{nonce}"));
    let effects = root.join("common/scripted_effects");
    std::fs::create_dir_all(&effects).expect("scripted effects directory");
    let body = concat!(
        // Direct clash: add_prestige wants country, add_province_modifier wants province.
        "clash = { add_prestige = 1 add_province_modifier = { name = clash_mod duration = 1 } }\n",
        // The clash also closes through a callee whose own contract is province-only.
        "via_callee = { helper_province = yes add_prestige = 1 }\n",
        "helper_province = { change_province_name = \"X\" }\n",
        // Compatible single-scope bodies must not error.
        "fine = { add_prestige = 1 add_manpower = 1000 }\n",
        // add_core has alternative province and country rows: either accepts.
        "dual_ok = { add_prestige = 1 add_core = FRA }\n",
        // A scope-switching container constrains only through its own row.
        "scoped_ok = { any_country = { change_province_name = \"Y\" } }\n",
        // Same-scope containers (if/limit) are descended, not treated as switches.
        "nested_ok = { if = { limit = { always = yes } add_prestige = 1 } }\n",
        // Dynamic $param$ dispatch is not narrowed.
        "dynamic_dispatch = { $action$ = yes }\n",
        // Dynamic scope links re-target runtime scopes; their bodies must not
        // constrain the entry (ROOT is the event root, not the macro entry).
        "root_opaque = { add_prestige = 1 ROOT = { change_province_name = \"Z\" } }\n",
        // A THIS block only evaluates the entry scope in trigger context;
        // as an effect container it must not constrain the entry either.
        "this_opaque = { add_prestige = 1 THIS = { change_province_name = \"W\" } }\n",
        // OR branches union: one satisfiable branch is enough, so the
        // province-only and country-only limits of the two branches combine
        // instead of clashing.
        "or_union = { if = { limit = { OR = { is_capital = yes has_estate_privilege = some_priv } } add_prestige = 1 } }\n",
        // An OR branch with no scope knowledge keeps the whole OR unconstrained.
        "or_open = { if = { limit = { OR = { unknown_branch_key = yes is_capital = yes } } add_prestige = 1 } }\n",
    );
    std::fs::write(effects.join("00_contracts.txt"), body).expect("macro definitions");
    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(1),
        SourceRootKind::CurrentMod,
        root.clone(),
    )]));
    host.refresh_source_roots().expect("scan definitions");

    let definitions = DocumentId::new("file:///tmp/common/scripted_effects/00_contracts.txt");
    host.open_document(
        definitions.clone(),
        1,
        body.to_owned(),
        Some(effects.join("00_contracts.txt")),
    )
    .expect("open definitions");
    let snapshot = host.snapshot();
    let all = diagnostics(&snapshot, &definitions);
    let empty: Vec<&Diagnostic> = all
        .iter()
        .filter(|diagnostic| diagnostic.code == DiagnosticCode::EmptyScopeContract)
        .collect();

    let clash_offset = u32::try_from(body.find("clash = ").expect("clash")).expect("u32");
    assert!(
        empty.iter().any(|diagnostic| {
            diagnostic.message.contains("`clash`")
                && diagnostic.message.contains("empty inferred entry scope")
                && diagnostic.range.start() == clash_offset
        }),
        "the country/province clash must be an error at the definition site: {empty:?}"
    );
    let via_offset = u32::try_from(body.find("via_callee = ").expect("via_callee")).expect("u32");
    assert!(
        empty
            .iter()
            .any(|diagnostic| diagnostic.range.start() == via_offset),
        "an empty contract propagates through a same-kind callee: {empty:?}"
    );
    let empty_names: Vec<&str> = empty
        .iter()
        .map(|diagnostic| {
            let start = usize::try_from(diagnostic.range.start()).expect("usize");
            let end = usize::try_from(diagnostic.range.end()).expect("usize");
            body[start..end].trim_end_matches(" =")
        })
        .collect();
    assert_eq!(
        empty_names,
        vec!["clash", "via_callee"],
        "only the two empty-contract definitions are reported: {all:?}"
    );

    // The inferred contracts behind those diagnostics, via the hover view.
    use crate::macro_contracts::{ScopeContract, contract_hover_line, macro_contract};
    assert_eq!(
        macro_contract(&snapshot, "scripted_effect", "clash"),
        Some(ScopeContract::Empty)
    );
    assert_eq!(
        macro_contract(&snapshot, "scripted_effect", "fine"),
        Some(ScopeContract::Scopes(vec!["country".to_owned()]))
    );
    assert_eq!(
        macro_contract(&snapshot, "scripted_effect", "helper_province"),
        Some(ScopeContract::Scopes(vec!["province".to_owned()]))
    );
    assert_eq!(
        macro_contract(&snapshot, "scripted_effect", "root_opaque"),
        Some(ScopeContract::Scopes(vec!["country".to_owned()])),
        "ROOT blocks re-target the event root and must not narrow the entry"
    );
    assert_eq!(
        macro_contract(&snapshot, "scripted_effect", "this_opaque"),
        Some(ScopeContract::Scopes(vec!["country".to_owned()])),
        "THIS effect blocks do not run in the entry scope"
    );
    assert_eq!(
        macro_contract(&snapshot, "scripted_effect", "or_union"),
        Some(ScopeContract::Scopes(vec!["country".to_owned()])),
        "OR branches union, so the province branch does not clash with add_prestige"
    );
    assert_eq!(
        macro_contract(&snapshot, "scripted_effect", "or_open"),
        Some(ScopeContract::Scopes(vec!["country".to_owned()])),
        "an unconstrained OR branch keeps the OR unconstrained"
    );
    let fine_hover = contract_hover_line(&snapshot, "scripted_effect", "fine");
    assert!(
        fine_hover.contains("Inferred entry scope: country"),
        "hover states the narrowed contract: {fine_hover}"
    );
    let dispatch_hover = contract_hover_line(&snapshot, "scripted_effect", "dynamic_dispatch");
    assert!(
        dispatch_hover.contains("dynamic `$param$` dispatch"),
        "hover flags dynamic dispatch: {dispatch_hover}"
    );
    assert!(
        contract_hover_line(&snapshot, "scripted_effect", "clash")
            .contains("definition can never run"),
        "hover explains the empty contract"
    );
    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn modifier_scope_mismatch_reports_cross_scope_modifier_applications() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-analysis-modifier-scope-{nonce}"));
    let modifiers_dir = root.join("common/event_modifiers");
    let events_dir = root.join("events");
    std::fs::create_dir_all(&modifiers_dir).expect("modifier directory");
    std::fs::create_dir_all(&events_dir).expect("events directory");
    let modifiers = concat!(
        "country_mod = { global_tax_modifier = 0.1 discipline = 0.05 }\n",
        "province_mod = { local_unrest = -1 local_defensiveness = 0.2 }\n",
        "mixed_mod = { global_tax_modifier = 0.1 local_unrest = -1 }\n",
        "unscoped_mod = { monthly_militarized_society = 0.1 }\n",
        "unit_mod = { land_morale_constant = 0.5 }\n",
    );
    std::fs::write(modifiers_dir.join("00_mods.txt"), modifiers).expect("modifier definitions");
    let events = concat!(
        "country_event = {\n",
        "    id = test_event.1\n",
        "    title = test_event.1.t\n",
        "    option = { name = opt_bad_country\n",
        "        add_province_modifier = { name = country_mod duration = 100 }\n",
        "    }\n",
        "    option = { name = opt_bad_province\n",
        "        add_country_modifier = { name = province_mod duration = 100 }\n",
        "    }\n",
        "    option = { name = opt_ok\n",
        "        add_country_modifier = { name = country_mod duration = 100 }\n",
        "    }\n",
        "    option = { name = opt_mixed\n",
        "        add_permanent_province_modifier = { name = mixed_mod duration = -1 }\n",
        "    }\n",
        "    option = { name = opt_unknown\n",
        "        add_country_modifier = { name = no_such_modifier duration = 100 }\n",
        "    }\n",
        "    option = { name = opt_unscoped\n",
        "        add_country_modifier = { name = unscoped_mod duration = 100 }\n",
        "    }\n",
        "    option = { name = opt_unit_country\n",
        "        add_country_modifier = { name = unit_mod duration = 100 }\n",
        "    }\n",
        "    option = { name = opt_unit_province\n",
        "        add_province_modifier = { name = unit_mod duration = 100 }\n",
        "    }\n",
        "}\n",
    );
    std::fs::write(events_dir.join("test_events.txt"), events).expect("event document");

    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(1),
        SourceRootKind::CurrentMod,
        root.clone(),
    )]));
    host.refresh_source_roots().expect("scan definitions");

    // The call-site document is open as an overlay; the modifier definitions
    // resolve through the scanned index.
    let document = DocumentId::new("file:///tmp/events/test_events.txt");
    host.open_document(
        document.clone(),
        1,
        events.to_owned(),
        Some(events_dir.join("test_events.txt")),
    )
    .expect("open events");
    let snapshot = host.snapshot();
    let all = diagnostics(&snapshot, &document);
    let mismatches: Vec<&Diagnostic> = all
        .iter()
        .filter(|diagnostic| diagnostic.code == DiagnosticCode::ModifierScopeMismatch)
        .collect();
    assert_eq!(
        mismatches.len(),
        4,
        "exactly the four cross-scope applications are reported: {all:?}"
    );
    // The country application of a unit-class modifier stays compatible and
    // contributes no diagnostic.

    // Locate the `name` value range; searching for the bare name would first
    // hit the `province_mod` prefix inside `add_province_modifier`.
    let offset = |name: &str| -> TextRange {
        let needle = format!("= {name} ");
        let position = events.find(&needle).unwrap_or_else(|| panic!("{name}"));
        let start = u32::try_from(position + 2).expect("u32");
        TextRange::new(start, start + u32::try_from(name.len()).expect("u32")).expect("range")
    };
    // Province-scoped effect applying country-class attributes: the game loads
    // them, so the application is recorded at information severity.
    let country_range = offset("country_mod");
    let country_case = mismatches
        .iter()
        .find(|diagnostic| diagnostic.range == country_range)
        .expect("info on the country_mod application");
    assert_eq!(country_case.severity, Severity::Information);
    assert_eq!(
        country_case.message,
        "modifier `country_mod` applies country-class attributes (global_tax_modifier, discipline) \
         in province scope"
    );
    // Country-scoped effect applying province-class attributes: information.
    let province_range = offset("province_mod");
    let province_case = mismatches
        .iter()
        .find(|diagnostic| diagnostic.range == province_range)
        .expect("info on the province_mod application");
    assert_eq!(province_case.severity, Severity::Information);
    assert_eq!(
        province_case.message,
        "modifier `province_mod` applies unexpected province-class attributes (local_unrest, \
         local_defensiveness) in country scope"
    );
    // Mixed definition under a province effect still reports the country part.
    let mixed_range = offset("mixed_mod");
    let mixed_case = mismatches
        .iter()
        .find(|diagnostic| diagnostic.range == mixed_range)
        .expect("info on the mixed_mod application");
    assert_eq!(mixed_case.severity, Severity::Information);
    assert!(mixed_case.message.contains("global_tax_modifier"));
    assert!(!mixed_case.message.contains("local_unrest"));

    // Unit-class attributes in a province application are unexpected and also
    // reported at information severity; only the second `unit_mod` application
    // (opt_unit_province) reports.
    let first_unit = events.find("= unit_mod ").expect("first unit_mod");
    let second_unit = events[first_unit + 1..]
        .find("= unit_mod ")
        .map(|position| first_unit + 1 + position)
        .expect("second unit_mod");
    let start = u32::try_from(second_unit + 2).expect("u32");
    let unit_range = TextRange::new(start, start + u32::try_from("unit_mod".len()).expect("u32"))
        .expect("range");
    let unit_case = mismatches
        .iter()
        .find(|diagnostic| diagnostic.range == unit_range)
        .expect("info on the province-side unit_mod application");
    assert_eq!(unit_case.severity, Severity::Information);
    assert_eq!(
        unit_case.message,
        "modifier `unit_mod` applies unexpected unit-class attributes (land_morale_constant) \
         in province scope"
    );

    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn macro_call_sites_are_validated_against_entry_contracts() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-analysis-call-sites-{nonce}"));
    let effects_dir = root.join("common/scripted_effects");
    let events_dir = root.join("events");
    std::fs::create_dir_all(&effects_dir).expect("effects directory");
    std::fs::create_dir_all(&events_dir).expect("events directory");
    let effects = concat!(
        "country_helper = { add_prestige = 1 }\n",
        "province_helper = { change_province_name = \"X\" }\n",
        "unconstrained_helper = { custom_tooltip = some_tip }\n",
    );
    std::fs::write(effects_dir.join("00_effects.txt"), effects).expect("macro definitions");
    let events = concat!(
        "country_event = {\n",
        "    id = call_site_test.1\n",
        "    title = call_site_test.1.t\n",
        "    option = { name = ok_country\n",
        "        country_helper = yes\n",
        "    }\n",
        "}\n",
        "country_event = {\n",
        "    id = call_site_test.2\n",
        "    title = call_site_test.2.t\n",
        "    option = { name = bad_province_macro\n",
        "        province_helper = yes\n",
        "    }\n",
        "}\n",
        "country_event = {\n",
        "    id = call_site_test.3\n",
        "    title = call_site_test.3.t\n",
        "    option = { name = ok_unconstrained\n",
        "        unconstrained_helper = yes\n",
        "    }\n",
        "}\n",
        "province_event = {\n",
        "    id = call_site_test.4\n",
        "    title = call_site_test.4.t\n",
        "    option = { name = ok_province\n",
        "        province_helper = yes\n",
        "    }\n",
        "}\n",
        "province_event = {\n",
        "    id = call_site_test.5\n",
        "    title = call_site_test.5.t\n",
        "    option = { name = bad_country_macro\n",
        "        country_helper = yes\n",
        "    }\n",
        "}\n",
    );
    std::fs::write(events_dir.join("call_sites.txt"), events).expect("event document");

    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(1),
        SourceRootKind::CurrentMod,
        root.clone(),
    )]));
    host.refresh_source_roots().expect("scan definitions");

    let document = DocumentId::new("file:///tmp/events/call_sites.txt");
    host.open_document(
        document.clone(),
        1,
        events.to_owned(),
        Some(events_dir.join("call_sites.txt")),
    )
    .expect("open events");
    let snapshot = host.snapshot();
    let all = diagnostics(&snapshot, &document);
    let mismatches: Vec<&Diagnostic> = all
        .iter()
        .filter(|diagnostic| diagnostic.code == DiagnosticCode::MacroCallScopeMismatch)
        .collect();
    assert_eq!(
        mismatches.len(),
        2,
        "only the two cross-scope macro calls are reported: {all:?}"
    );

    let key_after = |anchor: &str, key: &str| -> TextRange {
        let anchor_at = events.find(anchor).unwrap_or_else(|| panic!("{anchor}"));
        let key_at = anchor_at
            + events[anchor_at..]
                .find(key)
                .unwrap_or_else(|| panic!("{key}"));
        let start = u32::try_from(key_at).expect("u32");
        TextRange::new(start, start + u32::try_from(key.len()).expect("u32")).expect("range")
    };
    let bad_country = key_after("bad_country_macro", "country_helper");
    let bad_province = key_after("bad_province_macro", "province_helper");
    let country_case = mismatches
        .iter()
        .find(|diagnostic| diagnostic.range == bad_country)
        .expect("country_helper mismatch reported at its key");
    assert_eq!(country_case.severity, Severity::Error);
    assert_eq!(
        country_case.message,
        "scripted macro `country_helper` requires entry scope country but is called in `province` scope"
    );
    let province_case = mismatches
        .iter()
        .find(|diagnostic| diagnostic.range == bad_province)
        .expect("province_helper mismatch reported at its key");
    assert_eq!(
        province_case.message,
        "scripted macro `province_helper` requires entry scope province but is called in `country` scope"
    );

    std::fs::remove_dir_all(root).expect("cleanup");
}
