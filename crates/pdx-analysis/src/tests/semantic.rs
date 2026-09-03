use super::support::*;

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
fn quoted_transition_beats_any_scalar_leaf_fallback() {
    let (host, _) = quoted_script_snapshot(
        "country_event = { id = test.1 trigger = { embedded = \"foo = yes\" } }\n",
    );
    let snapshot = host.snapshot();
    let quoted = snapshot
        .rules()
        .exact_semantic_rules("embedded")
        .find(|rule| matches!(rule.shape, RuleShape::QuotedScript))
        .expect("quoted fixture rule");
    let mut fallback = quoted.clone();
    fallback.id = "fixture:trigger:any-scalar-fallback".to_owned();
    fallback.key = KeyMatcher::AnyScalar;
    fallback.shape = RuleShape::Leaf;
    fallback.child_context = None;
    let property = crate::ScriptProperty {
        key: std::sync::Arc::from("embedded"),
        key_range: TextRange::empty(0),
        range: TextRange::empty(0),
        operator: Some(std::sync::Arc::from("=")),
        scalar: Some((std::sync::Arc::from("foo = yes"), TextRange::empty(0))),
        quoted: true,
        quoted_source: None,
        block_range: None,
        block: Vec::new(),
        bare_values: Vec::new(),
    };
    let scope = crate::ScopeContext::new(snapshot.game_profile_handle());

    let selected = crate::semantic_selected_transition(crate::SemanticTransitionInput {
        snapshot: &snapshot,
        matching: &[&fallback, quoted],
        selected_alternative: None,
        context: "trigger",
        parent_path: &[],
        property: &property,
        scope: &scope,
        transparent_wrapper: false,
    })
    .expect("specific quoted transition");

    assert_eq!(selected.id, quoted.id);
    assert_eq!(selected.child_context.as_deref(), Some("trigger"));
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
        root: pdx_engine::hir::ScopeValue::known(vec!["country".to_owned(), "province".to_owned()]),
        current: vec![pdx_engine::hir::ScopeValue::known(vec![
            "country".to_owned(),
            "province".to_owned(),
        ])],
        from: vec![pdx_engine::hir::ScopeValue::known(vec![
            "country".to_owned(),
        ])],
        previous: Vec::new(),
    };
    let context =
        crate::scope_context_from_hir(std::sync::Arc::new(pdx_game::eu4::profile()), &state);
    assert_eq!(context.root.as_ref(), "any");
    assert_eq!(context.current.as_ref(), "any");
    assert_eq!(
        context.from.iter().map(|s| s.as_ref()).collect::<Vec<_>>(),
        vec!["country"]
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
        retain_attributes: false,
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
            item.code == DiagnosticCode::UnknownKey && item.message.contains("`tripolitania_area`")
        }),
        "area name was not accepted as a dynamic area key: {results:?}"
    );
    assert!(
        !results.iter().any(|item| {
            item.code == DiagnosticCode::RuleWrongScope
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
            item.code == DiagnosticCode::UnknownBareValue && item.message.contains("potential")
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
    let base = pdx_game::eu4::first_party_rules().expect("first-party rules");
    let mut left = base.model().semantic.rules[0].clone();
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
    // Rebuild the runtime index so the keyed container lookups can see the fixture rules.
    let mut model = base.model().clone();
    model.semantic.rules = vec![left.clone(), right.clone()];
    let host = eu4_host(pdx_rules::RuleSet::from_model(model));
    let snapshot = host.snapshot();
    let rules = snapshot
        .rules()
        .model()
        .semantic
        .rules
        .iter()
        .collect::<Vec<_>>();
    let scope = crate::ScopeContext::new(std::sync::Arc::new(pdx_game::eu4::profile()));
    assert_eq!(
        crate::semantic_selected_alternative(&snapshot, &rules, "fixture", &[], &[], &[], &scope),
        None
    );

    let property = crate::ScriptProperty {
        key: std::sync::Arc::from("left"),
        key_range: TextRange::empty(0),
        range: TextRange::empty(0),
        operator: None,
        scalar: Some((std::sync::Arc::from("yes"), TextRange::empty(0))),
        quoted: false,
        quoted_source: None,
        block_range: None,
        block: Vec::new(),
        bare_values: Vec::new(),
    };
    assert_eq!(
        crate::semantic_selected_alternative(
            &snapshot,
            &rules,
            "fixture",
            &[],
            &[&property],
            &[],
            &scope,
        )
        .as_deref(),
        Some("left-alternative")
    );
}

#[test]
fn first_party_alternatives_select_value_shape_by_current_scope() {
    let host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    let snapshot = host.snapshot();
    let accepts =
        |context: &str, key: &str, value: Option<&str>, block: bool, current_scope: &str| {
            let property = crate::ScriptProperty {
                key: std::sync::Arc::from(key),
                key_range: TextRange::empty(0),
                range: TextRange::empty(0),
                operator: Some(std::sync::Arc::from("=")),
                scalar: value.map(|value| {
                    (
                        std::sync::Arc::from(value),
                        TextRange::empty(u32::try_from(key.len() + 3).expect("offset")),
                    )
                }),
                quoted: false,
                quoted_source: None,
                block_range: block.then(|| TextRange::empty(0)),
                block: Vec::new(),
                bare_values: Vec::new(),
            };
            let mut scope = crate::ScopeContext::new(snapshot.game_profile_handle());
            scope.root = std::sync::Arc::from(current_scope);
            scope.current = std::sync::Arc::from(current_scope);
            let candidates = snapshot
                .rules()
                .semantic_rules_for_context_key(context, key)
                .filter(|rule| {
                    rule.parent_path.is_empty()
                        && crate::semantic::semantic_rule_key_matches(&snapshot, rule, &[], key)
                })
                .collect::<Vec<_>>();
            candidates.into_iter().any(|rule| {
                crate::semantic::semantic_scope_allows(rule, &scope)
                    && crate::semantic::semantic_property_matches(
                        &snapshot, rule, &property, &scope,
                    )
            })
        };

    // kill_leader is a country+province dual (cwtools-compare arbitration):
    // both the block shape and the scalar spelling resolve in either scope.
    assert!(accepts("effect", "kill_leader", None, true, "country"));
    assert!(accepts(
        "effect",
        "kill_leader",
        Some("general"),
        false,
        "country"
    ));
    assert!(accepts(
        "effect",
        "kill_leader",
        Some("general"),
        false,
        "province"
    ));
    assert!(accepts("effect", "kill_leader", None, true, "province"));

    // Absolute scope switches (emperor, enum[country_tags], global iterators,
    // type addresses) are unrestricted: usable from any scope.
    assert!(accepts("effect", "emperor", None, true, "country"));
    assert!(accepts("effect", "emperor", None, true, "province"));
    assert!(accepts("effect", "emperor", None, true, "unit"));
    assert!(accepts(
        "effect",
        "emperor",
        None,
        true,
        "mercenary_company"
    ));
    assert!(accepts("trigger", "emperor", None, true, "province"));
    assert!(accepts(
        "trigger",
        "emperor",
        None,
        true,
        "mercenary_company"
    ));
    assert!(accepts("effect", "every_country", None, true, "unit"));
    assert!(accepts("trigger", "any_province", None, true, "country"));
    // every_owned_province is a country+province dual (cwtools-compare
    // arbitration): unit scope no longer accepts it.
    assert!(accepts(
        "effect",
        "every_owned_province",
        None,
        true,
        "country"
    ));
    assert!(accepts(
        "effect",
        "every_owned_province",
        None,
        true,
        "province"
    ));
    assert!(!accepts(
        "effect",
        "every_owned_province",
        None,
        true,
        "unit"
    ));

    // All estate_loyalty variants are country-scope only.
    assert!(accepts("trigger", "estate_loyalty", None, true, "country"));
    assert!(!accepts(
        "trigger",
        "estate_loyalty",
        None,
        true,
        "province"
    ));

    // change_national_focus accepts the `none` spelling in country scope only.
    assert!(accepts(
        "effect",
        "change_national_focus",
        Some("none"),
        false,
        "country"
    ));
    assert!(!accepts(
        "effect",
        "change_national_focus",
        Some("none"),
        false,
        "province"
    ));

    // trade_range is province-class; trade-node contexts accept it through the
    // one-way trade_node→province compatibility, not by declaration.
    assert!(accepts(
        "trigger",
        "trade_range",
        Some("owner"),
        false,
        "province"
    ));
    assert!(accepts(
        "trigger",
        "trade_range",
        Some("owner"),
        false,
        "trade_node"
    ));
    assert!(!accepts(
        "trigger",
        "trade_range",
        Some("owner"),
        false,
        "country"
    ));
    assert!(accepts(
        "trigger",
        "same_continent",
        Some("owner"),
        false,
        "country"
    ));
    assert!(accepts(
        "trigger",
        "same_continent",
        Some("capital_scope"),
        false,
        "province"
    ));

    assert!(accepts(
        "trigger",
        "has_discovered",
        Some("capital_scope"),
        false,
        "country"
    ));
    // Value-shape selection no longer gates on current scope for the flattened
    // country+province groups: ROOT resolves to a country and now matches the
    // former province-side alternative in both scopes.
    assert!(accepts(
        "trigger",
        "has_discovered",
        Some("ROOT"),
        false,
        "country"
    ));
    assert!(accepts(
        "trigger",
        "has_discovered",
        Some("owner"),
        false,
        "province"
    ));
    assert!(accepts(
        "trigger",
        "has_discovered",
        Some("ROOT"),
        false,
        "province"
    ));
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
    let scope = crate::ScopeContext::new(std::sync::Arc::new(pdx_game::eu4::profile()));
    let mut property = crate::ScriptProperty {
        key: std::sync::Arc::from("choose"),
        key_range: TextRange::empty(0),
        range: TextRange::empty(0),
        operator: Some(std::sync::Arc::from("=")),
        scalar: None,
        quoted: false,
        quoted_source: None,
        block_range: Some(TextRange::empty(0)),
        block: vec![crate::ScriptProperty {
            key: std::sync::Arc::from("FRA"),
            key_range: TextRange::empty(0),
            range: TextRange::empty(0),
            operator: Some(std::sync::Arc::from("=")),
            scalar: Some((std::sync::Arc::from("yes"), TextRange::empty(0))),
            quoted: false,
            quoted_source: None,
            block_range: None,
            block: Vec::new(),
            bare_values: Vec::new(),
        }],
        bare_values: Vec::new(),
    };
    let selected = crate::semantic_selected_transition(crate::SemanticTransitionInput {
        snapshot: &snapshot,
        matching: &[&country_transition, &other_transition],
        selected_alternative: None,
        context: "fixture",
        parent_path: &[],
        property: &property,
        scope: &scope,
        transparent_wrapper: false,
    })
    .expect("workspace-backed child key selects a transition");
    assert_eq!(
        selected.child_context.as_deref(),
        Some("country-destination")
    );

    property.block[0].key = std::sync::Arc::from("MISSING");
    assert!(
        crate::semantic_selected_transition(crate::SemanticTransitionInput {
            snapshot: &snapshot,
            matching: &[&country_transition, &other_transition],
            selected_alternative: None,
            context: "fixture",
            parent_path: &[],
            property: &property,
            scope: &scope,
            transparent_wrapper: false,
        })
        .is_none(),
        "an unresolved child key must not fall back to rule order"
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

    let root = std::env::temp_dir().join(format!("pdx-analysis-cwt-tags-{}", std::process::id()));
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

    let root = std::env::temp_dir().join(format!("pdx-analysis-cwt-flags-{}", std::process::id()));
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

    let root = std::env::temp_dir().join(format!("pdx-analysis-cwt-params-{}", std::process::id()));
    fs::create_dir_all(root.join("common/scripted_effects")).expect("scripted effect directory");
    let definition_path = root.join("common/scripted_effects/00_test.txt");
    fs::write(
        &definition_path,
        concat!(
            "apply = { add_prestige = $amount$ [[optional] add_stability = 1 ] }\n",
            "other_effect = { add_prestige = $other$ }\n",
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
    let invocation = "country_event = { immediate = { apply = { amount = 1 optional = yes } } }\n";
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
        crate::parameter_names_for_owner(&snapshot, "scripted_effect", "apply")
            .expect("resolved owner parameters"),
        ["amount", "optional"]
    );
    assert!(diagnostics(&snapshot, &id).iter().all(|item| !matches!(
        item.code,
        DiagnosticCode::InvalidValue | DiagnosticCode::UnknownKey
    )));
    let completion_position =
        u32::try_from(invocation.find("apply = { ").expect("invocation") + "apply = { ".len() - 1)
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
        "apply = { add_prestige = $overlay_only$ }\n".to_owned(),
        Some(definition_path),
    )
    .expect("open scripted effect overlay");
    let overlay_call = DocumentId::new("file:///tmp/events/overlay-params.txt");
    host.open_document(
        overlay_call.clone(),
        1,
        "country_event = { immediate = { apply = { overlay_only = 1 amount = 2 } } }\n".to_owned(),
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
fn unresolved_macro_signature_keeps_parameter_blocks_open_world() {
    use pdx_engine::{SourceRoot, SourceRootId, SourceRootKind, WorkspaceChange};
    use std::fs;

    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-analysis-open-macro-params-{nonce}"));
    let definitions = root.join("common/scripted_effects");
    fs::create_dir_all(&definitions).expect("scripted effect directory");
    fs::write(
        definitions.join("00_first.txt"),
        "ambiguous_effect = { value = $first$ }\n",
    )
    .expect("first definition");
    fs::write(
        definitions.join("01_second.txt"),
        "ambiguous_effect = { value = $second$ }\n",
    )
    .expect("second definition");

    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(1),
        SourceRootKind::CurrentMod,
        root.clone(),
    )]));
    host.refresh_source_roots().expect("scan ambiguous macros");
    let id = DocumentId::new("file:///tmp/events/open-macro-params.txt");
    let invocation =
        "country_event = { immediate = { ambiguous_effect = { foreign_parameter = 1 } } }\n";
    host.open_document(id.clone(), 1, invocation.to_owned(), None)
        .expect("open invocation");
    let snapshot = host.snapshot();
    assert!(
        crate::parameter_names_for_owner(&snapshot, "scripted_effect", "ambiguous_effect")
            .is_none()
    );
    let results = diagnostics(&snapshot, &id);
    assert!(!results.iter().any(|item| {
        item.code == DiagnosticCode::UnknownKey && item.message.contains("`foreign_parameter`")
    }));
    assert!(!results.iter().any(|item| {
        item.code == DiagnosticCode::Cardinality
            && item.message.contains("missing required parameter")
    }));

    let completion_position = u32::try_from(
        invocation
            .find("ambiguous_effect = { ")
            .expect("parameter block")
            + "ambiguous_effect = { ".len()
            - 1,
    )
    .expect("completion position");
    let labels = complete(&snapshot, &id, completion_position)
        .items
        .into_iter()
        .map(|item| item.label)
        .collect::<Vec<_>>();
    assert!(
        !labels
            .iter()
            .any(|label| label.eq_ignore_ascii_case("scaled_skill")),
        "an unresolved owner must not fall back to the static parameter enum: {labels:?}"
    );

    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn eu4_legacy_governments_use_eu4_reform_semantics() {
    use pdx_engine::{SourceRoot, SourceRootId, SourceRootKind, WorkspaceChange};
    use std::fs;

    let root = std::env::temp_dir().join(format!("pdx-analysis-cwt-legacy-{}", std::process::id()));
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
