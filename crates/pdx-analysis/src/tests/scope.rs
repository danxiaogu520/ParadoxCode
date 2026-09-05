use super::support::*;

#[test]
fn eu4_scope_links_switch_effect_context_and_scope() {
    let rules = pdx_game::eu4::first_party_rules().expect("load first-party rules");
    let mut host = eu4_host(rules);
    let id = DocumentId::new("file:///tmp/events/scope.txt");
    host.open_document(
        id.clone(),
        1,
        "country_event = { immediate = { capital_scope = { add_base_tax = nope } } }\n".to_owned(),
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
fn unknown_tooltip_and_named_event_target_scopes_stay_conservative() {
    let rules = pdx_game::eu4::first_party_rules().expect("load first-party rules");
    let mut host = eu4_host(rules);
    let id = DocumentId::new("file:///tmp/events/conservative-scopes.txt");
    let text = concat!(
        "country_event = { immediate = { ",
        "tooltip = { add_base_tax = 1 } ",
        "event_target:runtime_province = { spawn_rebels = { type = catholic_rebels size = 1 } } ",
        "225 = { ROOT = { set_capital = PREV } } ",
        "} }\n",
    );
    host.open_document(id.clone(), 1, text.to_owned(), None)
        .expect("open");
    let results = diagnostics(&host.snapshot(), &id);
    assert!(
        results.iter().all(|item| {
            item.code != DiagnosticCode::WrongScope
                && !(item.code == DiagnosticCode::InvalidValue
                    && item.message.contains("set_capital"))
        }),
        "runtime/tooltip scopes must not be disproved statically: {results:?}"
    );
}

#[test]
fn eu4_scope_link_chains_are_resolved_segment_by_segment() {
    let rules = pdx_game::eu4::first_party_rules().expect("load first-party rules");
    let host = eu4_host(rules);
    let snapshot = host.snapshot();
    let mut context = crate::ScopeContext::new(std::sync::Arc::new(pdx_game::eu4::profile()));
    context.root = std::sync::Arc::from("province");
    context.current = std::sync::Arc::from("province");

    assert_eq!(
        crate::resolve_scope_expression_context(&snapshot, &context, "owner.capital_scope")
            .as_ref(),
        "province"
    );
    assert_eq!(
        crate::resolve_scope_expression_context(&snapshot, &context, "owner.missing_link").as_ref(),
        "any"
    );

    let mut invalid_register_rule = snapshot.rules().model().semantic.rules[0].clone();
    invalid_register_rule.push_scope = None;
    invalid_register_rule.replace_scope = vec![
        ("from_owner".to_owned(), "country".to_owned()),
        ("previous_owner".to_owned(), "country".to_owned()),
    ];
    let unchanged = crate::semantic_child_scope(&snapshot, &context, &invalid_register_rule);
    assert!(unchanged.from.is_empty());
    assert!(unchanged.previous.is_empty());
}

#[test]
fn game_age_abilities_defined_in_the_current_file_validate_their_effects() {
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
    assert!(
        diagnostics.iter().all(|item| {
            item.code != DiagnosticCode::UnknownKey || !item.message.contains("`MISSING`")
        }),
        "an ability declared below `abilities` is a workspace definition: {diagnostics:?}"
    );
    let missing_symbols = diagnostics.iter().filter(|item| {
        item.code == DiagnosticCode::UnknownLocalisationKey
            && item.message.contains("`missing_loc`")
    });
    assert_eq!(
        missing_symbols.count(),
        2,
        "both indexed and newly declared abilities must validate their nested effects"
    );

    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn game_age_ability_in_an_initially_empty_index_is_a_definition() {
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
        diagnostics.iter().all(|item| {
            item.code != DiagnosticCode::UnknownKey || !item.message.contains("`MISSING`")
        }),
        "the current file must contribute its game-age ability definition: {diagnostics:?}"
    );
    assert!(
        diagnostics.iter().any(|item| {
            item.code == DiagnosticCode::UnknownLocalisationKey
                && item.range.start() == missing_loc_start
        }),
        "a newly defined ability must validate its nested effect: {diagnostics:?}"
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
        crate::repeated_scope_register_depth("prevprev", "prev"),
        Some(1)
    );
    assert_eq!(
        crate::repeated_scope_register_depth("previous_owner", "previous"),
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
            .any(|item| item.code == DiagnosticCode::WrongScope)
    );
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn dynamic_scope_mismatch_surfaces_at_the_call_site() {
    use pdx_engine::{SourceRoot, SourceRootId, SourceRootKind, WorkspaceChange};
    use std::fs;

    let mut model = pdx_game::eu4::first_party_rules()
        .expect("first-party rules")
        .model()
        .clone();
    model.semantic.rules.push(SemanticRule {
        id: "fixture:effect:enter-province".to_owned(),
        context: "effect".to_owned(),
        parent_path: Vec::new(),
        key: KeyMatcher::Exact("fixture_enter_province".to_owned()),
        operator: None,
        value: ValueMatcher::AnyScalar,
        shape: RuleShape::Node,
        child_context: Some("effect".to_owned()),
        alternative_id: None,
        severity: None,
        required: false,
        deprecated: false,
        documentation: Vec::new(),
        allowed_scopes: Vec::new(),
        push_scope: Some("province".to_owned()),
        replace_scope: Vec::new(),
        min_occurs: None,
        strict_min: true,
        max_occurs: None,
        source_file: "fixture.semantic".to_owned(),
        line: 1,
    });
    model.semantic.rules.push(SemanticRule {
        id: "fixture:effect:country-only".to_owned(),
        context: "effect".to_owned(),
        parent_path: Vec::new(),
        key: KeyMatcher::Exact("fixture_country_only".to_owned()),
        operator: None,
        value: ValueMatcher::Bool,
        shape: RuleShape::Leaf,
        child_context: None,
        alternative_id: None,
        severity: None,
        required: false,
        deprecated: false,
        documentation: Vec::new(),
        allowed_scopes: vec!["country".to_owned()],
        push_scope: None,
        replace_scope: Vec::new(),
        min_occurs: None,
        strict_min: true,
        max_occurs: None,
        source_file: "fixture.semantic".to_owned(),
        line: 2,
    });

    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-analysis-dynamic-scope-{nonce}"));
    let definitions = root.join("common/scripted_effects");
    fs::create_dir_all(&definitions).expect("definition directory");
    fs::write(
        definitions.join("00_scope.txt"),
        "country_wrapper = { fixture_country_only = yes }\n",
    )
    .expect("dynamic definition");
    let mut host = eu4_host(RuleSet::from_model(model));
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(1),
        SourceRootKind::CurrentMod,
        root.clone(),
    )]));
    host.refresh_source_roots().expect("scan definition");
    let id = DocumentId::new("file:///tmp/events/dynamic-scope.txt");
    let source = concat!(
        "country_event = { immediate = { ",
        "country_wrapper = yes ",
        "fixture_enter_province = { country_wrapper = yes }",
        " } }\n",
    );
    host.open_document(id.clone(), 1, source.to_owned(), None)
        .expect("open calls");

    let results = diagnostics(&host.snapshot(), &id);
    // Scope authority for expansion trees sits with the dynamic-contract layer:
    // the nested call in province scope is reported once, at the call site,
    // instead of once per offending statement inside the expansion.
    let call_site = results
        .iter()
        .filter(|diagnostic| diagnostic.code == DiagnosticCode::DynamicCallScopeMismatch)
        .collect::<Vec<_>>();
    assert_eq!(call_site.len(), 1, "{results:?}");
    let nested_call = source
        .rfind("country_wrapper")
        .expect("nested dynamic call");
    assert_eq!(
        call_site[0].range.start(),
        u32::try_from(nested_call).expect("call range")
    );
    assert_eq!(
        call_site[0].message,
        "dynamic definition `country_wrapper` requires entry scope country but is called in `province` scope"
    );
    assert!(
        results
            .iter()
            .all(|diagnostic| !(diagnostic.code == DiagnosticCode::WrongScope
                && diagnostic.message.contains("fixture_country_only"))),
        "the expansion walk no longer reports scope findings: {results:?}"
    );
    fs::remove_dir_all(root).expect("cleanup");
}
