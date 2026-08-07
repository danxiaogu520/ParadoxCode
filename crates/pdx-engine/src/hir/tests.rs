use super::{
    HirParameterReferenceKind, HirReferenceOrigin, ScopeState, ScopeValue, lower,
    lower_with_profile, property_children, resolve_scope_expression,
};
use pdx_game::eu4::{bootstrap_rules, first_party_rules, profile};
use pdx_parser::{FileFormat, parse};
use pdx_rules::{GameProfile, RuleSet, RuleShape};
use pdx_text::LogicalPath;

#[test]
fn lowering_retains_property_paths_scalars_and_top_level_identity() {
    let parsed = parse(
        FileFormat::Script,
        "root = { child = \"value\" nested = { leaf = yes } }\n",
    );
    let hir = lower(parsed, &RuleSet::empty());

    assert_eq!(hir.properties().len(), 4);
    assert!(hir.properties()[0].top_level);
    assert_eq!(hir.properties()[0].path, ["root"]);
    assert_eq!(hir.properties()[1].path, ["root", "child"]);
    assert!(hir.properties()[1].value_range.is_some());
    assert_eq!(
        hir.properties()[1]
            .scalar
            .as_ref()
            .expect("child scalar")
            .value,
        "value"
    );
    assert_eq!(hir.properties()[3].path, ["root", "nested", "leaf"]);
    assert_eq!(
        hir.bare_values()
            .iter()
            .map(|value| value.value.as_str())
            .collect::<Vec<_>>(),
        ["yes"]
    );
}

#[test]
fn property_adjacency_preserves_duplicate_siblings_and_nested_children() {
    let hir = lower(
        parse(
            FileFormat::Script,
            concat!(
                "root = { ",
                "duplicate = { child = one } ",
                "duplicate = { child = two nested = { leaf = three } } ",
                "tail = yes",
                " }\n",
            ),
        ),
        &RuleSet::empty(),
    );
    let children = property_children(hir.properties());
    let root = hir
        .properties()
        .iter()
        .position(|property| property.top_level && property.key == "root")
        .expect("root property");
    let root_keys = children[root]
        .iter()
        .map(|index| hir.properties()[*index].key.as_str())
        .collect::<Vec<_>>();
    assert_eq!(root_keys, ["duplicate", "duplicate", "tail"]);

    let second_duplicate = children[root][1];
    let duplicate_keys = children[second_duplicate]
        .iter()
        .map(|index| hir.properties()[*index].key.as_str())
        .collect::<Vec<_>>();
    assert_eq!(duplicate_keys, ["child", "nested"]);
    let nested = children[second_duplicate][1];
    assert_eq!(children[nested].len(), 1);
    assert_eq!(hir.properties()[children[nested][0]].key, "leaf");
}

#[test]
fn lowering_retains_localisation_definition_ranges() {
    let parsed = parse(
        FileFormat::Localisation,
        "l_english:\n example_key:0 \"Example\"\n",
    );
    let hir = lower(parsed, &RuleSet::empty());

    assert_eq!(hir.localisation_entries().len(), 1);
    assert_eq!(hir.localisation_entries()[0].name, "example_key");
    let entry = &hir.localisation_entries()[0];
    assert!(entry.range.start() <= entry.name_range.start());
    assert!(entry.name_range.end() <= entry.range.end());
}

#[test]
fn required_type_localisation_templates_expand_from_dynamic_members() {
    let path = LogicalPath::parse("missions/test.txt").expect("logical path");
    let hir = lower_with_profile(
        parse(
            FileFormat::Script,
            "series = { mission_one = { potential = { always = yes } } }\n",
        ),
        &path,
        &first_party_rules().expect("first-party rules"),
        &profile(),
    );
    let derived = hir
        .references()
        .iter()
        .filter(|reference| reference.origin == HirReferenceOrigin::DerivedLocalisation)
        .map(|reference| reference.name.as_str())
        .collect::<Vec<_>>();
    assert!(derived.contains(&"mission_one_title"));
    assert!(derived.contains(&"mission_one_desc"));
}

#[test]
fn subtype_conditions_gate_type_localisation_templates() {
    let path = LogicalPath::parse("common/ideas/subtypes.txt").expect("logical path");
    let hir = lower_with_profile(
        parse(
            FileFormat::Script,
            "country_idea = { free = yes }\nother_idea = { free = no }\n",
        ),
        &path,
        &first_party_rules().expect("first-party rules"),
        &profile(),
    );
    let derived = hir
        .references()
        .iter()
        .filter(|reference| reference.origin == HirReferenceOrigin::DerivedLocalisation)
        .map(|reference| reference.name.as_str())
        .collect::<Vec<_>>();
    assert!(derived.contains(&"country_idea_start"));
    assert!(!derived.contains(&"other_idea_start"));
}

#[test]
fn lowering_retains_recovery_nodes_as_unknown_constructs() {
    let source = "root = { = broken good = yes }\n";
    let hir = lower(parse(FileFormat::Script, source), &RuleSet::empty());

    assert!(!hir.syntax().errors().is_empty());
    assert!(!hir.unknown_constructs().is_empty());
    assert!(
        hir.unknown_constructs().iter().all(|unknown| {
            unknown.range.end() <= u32::try_from(source.len()).unwrap_or(u32::MAX)
        })
    );
    assert!(
        hir.properties()
            .iter()
            .any(|property| property.key == "good")
    );
}

#[test]
fn lowering_retains_parameter_conditionals_with_polarity() {
    let source = "[[enabled] value = yes ]\n[[!disabled] other = no ]\n";
    let hir = lower(parse(FileFormat::Script, source), &RuleSet::empty());

    assert_eq!(hir.parameter_conditionals().len(), 2);
    assert_eq!(hir.parameter_conditionals()[0].name, "enabled");
    assert!(!hir.parameter_conditionals()[0].negated);
    assert_eq!(hir.parameter_conditionals()[1].name, "disabled");
    assert!(hir.parameter_conditionals()[1].negated);
    assert!(hir.parameter_conditionals().iter().all(|conditional| {
        conditional.range.start() <= conditional.condition_range.start()
            && conditional.condition_range.end() <= conditional.range.end()
    }));
}

#[test]
fn profile_lowering_associates_local_parameter_definitions_and_uses() {
    let source = concat!(
        "first = { value = $amount$ again = $amount$ ",
        "[[optional] enabled = yes ] }\n",
        "second = { value = $amount$ }\n",
    );
    let path = LogicalPath::parse("common/scripted_effects/parameters.txt").expect("logical path");
    let hir = lower_with_profile(
        parse(FileFormat::Script, source),
        &path,
        &bootstrap_rules(),
        &profile(),
    );

    assert_eq!(hir.parameter_definitions().len(), 3);
    assert_eq!(hir.parameter_references().len(), 4);
    assert_eq!(
        hir.parameter_definitions()
            .iter()
            .filter(|definition| definition.name == "amount")
            .count(),
        2
    );
    assert!(hir.parameter_definitions().iter().all(|definition| {
        hir.syntax().text(definition.name_range) == Some(definition.name.as_str())
            && definition.range.end() <= definition.owner_range.end()
    }));
    assert!(hir.parameter_references().iter().any(|reference| {
        reference.name == "optional" && reference.kind == HirParameterReferenceKind::Conditional
    }));
    assert_eq!(
        hir.parameter_references()
            .iter()
            .filter(|reference| reference.kind == HirParameterReferenceKind::Substitution)
            .count(),
        3
    );
    let first_owner = hir.parameter_definitions()[0].owner_range;
    assert_eq!(hir.parameter_definitions_for_owner(first_owner).count(), 2);
    assert_eq!(hir.parameter_references_for_owner(first_owner).count(), 3);
    let optional_position =
        u32::try_from(source.find("optional").expect("optional parameter")).expect("position");
    assert_eq!(
        hir.parameter_reference_at(optional_position)
            .map(|reference| reference.name.as_str()),
        Some("optional")
    );
    assert!(hir.parameter_reference_at(first_owner.end()).is_none());
}

#[test]
fn profile_aware_lowering_produces_shared_typed_definitions_and_references() {
    let rules = bootstrap_rules();
    let path = LogicalPath::parse("events/profile_hir.txt").expect("logical path");
    let source =
        "country_event = { id = profile.1 title = profile_title set_country_flag = seen }\n";

    let hir = lower_with_profile(parse(FileFormat::Script, source), &path, &rules, &profile());

    assert!(hir.definitions().iter().any(|definition| {
        definition.kind == "event"
            && definition.name == "profile.1"
            && definition.selection_range != definition.range
    }));
    assert!(
        hir.definitions()
            .iter()
            .any(|definition| definition.kind == "country_flag" && definition.name == "seen")
    );
    assert!(hir.references().iter().any(|reference| {
        reference.kind == "localisation" && reference.name == "profile_title"
    }));
}

#[test]
fn first_party_semantic_localisation_rules_produce_references_without_profile_shorthand() {
    let rules = first_party_rules().expect("first-party rules");
    let path = LogicalPath::parse("events/semantic_hir.txt").expect("logical path");
    let hir = lower_with_profile(
        parse(
            FileFormat::Script,
            "country_event = { id = semantic.1 title = semantic_title }\n",
        ),
        &path,
        &rules,
        &GameProfile::empty(rules.game_id()),
    );

    assert!(hir.references().iter().any(|reference| {
        reference.origin == HirReferenceOrigin::Semantic
            && reference.kind == "localisation"
            && reference.name == "semantic_title"
    }));
}

#[test]
fn scope_facts_descend_through_dynamic_mission_blocks() {
    let rules = first_party_rules().expect("embedded rules");
    let path = LogicalPath::parse("missions/dynamic_scope_hir.txt").expect("logical path");
    let hir = lower_with_profile(
        parse(
            FileFormat::Script,
            "series = { mission_one = { effect = { custom_tooltip = tt_key } } }\n",
        ),
        &path,
        &rules,
        &profile(),
    );

    let tooltip = hir
        .properties()
        .iter()
        .find(|property| property.key == "custom_tooltip")
        .expect("custom tooltip property");
    let fact = hir
        .scope_facts()
        .iter()
        .find(|fact| fact.range == tooltip.key_range)
        .expect("mission effect scope fact");
    assert_eq!(fact.context, "effect");
    assert!(
        fact.parent_path.is_empty(),
        "the effect child context resets the semantic path"
    );
}

#[test]
fn identity_only_profile_does_not_create_game_specific_typed_facts() {
    let rules = bootstrap_rules();
    let path = LogicalPath::parse("events/profile_hir.txt").expect("logical path");
    let source = "country_event = { id = profile.1 title = profile_title }\n";
    let profile = GameProfile::empty(rules.game_id());

    let hir = lower_with_profile(parse(FileFormat::Script, source), &path, &rules, &profile);

    assert!(hir.definitions().is_empty());
    assert!(
        !hir.references()
            .iter()
            .any(|reference| reference.kind == "localisation")
    );
}

#[test]
fn profile_lowering_caches_semantic_root_context_and_initial_scope() {
    let rules = first_party_rules().expect("embedded rules");
    let path = LogicalPath::parse("events/scope_hir.txt").expect("logical path");
    let hir = lower_with_profile(
        parse(
            FileFormat::Script,
            "country_event = { id = scope.1 immediate = { capital_scope = { add_base_tax = 1 } } }\n",
        ),
        &path,
        &rules,
        &profile(),
    );

    let fact = hir.scope_facts().first().expect("semantic root scope fact");
    assert_eq!(fact.context, "type:event");
    assert_eq!(
        fact.state.root,
        ScopeValue::Known(vec!["country".to_owned()])
    );
    assert_eq!(
        fact.state.current,
        vec![ScopeValue::Known(vec!["country".to_owned()])]
    );
    assert_eq!(hir.scope_fact(fact.range, "TYPE:EVENT"), Some(fact));
    let tax = hir
        .properties()
        .iter()
        .find(|property| property.key == "add_base_tax")
        .expect("nested province command");
    let tax_scope = hir
        .scope_facts()
        .iter()
        .find(|fact| fact.range == tax.key_range)
        .expect("nested transition fact");
    assert_eq!(
        tax_scope.state.current.first(),
        Some(&ScopeValue::Known(vec!["province".to_owned()]))
    );
    assert!(
        tax_scope.parent_path.is_empty(),
        "an explicit same-name effect child context resets the semantic path"
    );
}

#[test]
fn equivalent_rule_alternatives_share_one_cached_transition() {
    let rules = first_party_rules().expect("embedded rules");
    let path = LogicalPath::parse("events/alternative_scope_hir.txt").expect("logical path");
    let hir = lower_with_profile(
        parse(
            FileFormat::Script,
            concat!(
                "country_event = { immediate = { ",
                "multiply_variable = { which = amount value = 2 }",
                " } }\n",
            ),
        ),
        &path,
        &rules,
        &profile(),
    );

    let which = hir
        .properties()
        .iter()
        .find(|property| property.key == "which")
        .expect("alternative child");
    let fact = hir
        .scope_facts()
        .iter()
        .find(|fact| fact.range == which.key_range)
        .expect("equivalent alternatives should continue lowering");
    assert_eq!(
        fact.state.current.first(),
        Some(&ScopeValue::Known(vec!["country".to_owned()]))
    );

    let conflicting = lower_with_profile(
        parse(
            FileFormat::Script,
            "country_event = { mean_time_to_happen = { days = 30 } }\n",
        ),
        &path,
        &rules,
        &profile(),
    );
    let days = conflicting
        .properties()
        .iter()
        .find(|property| property.key == "days")
        .expect("conflicting-transition child");
    let days_fact = conflicting
        .scope_facts()
        .iter()
        .find(|fact| fact.range == days.key_range)
        .expect("the child key statically eliminates the modifier-rule transition");
    assert_eq!(
        days_fact.context, "type:event",
        "the rules for `days` keep the event mean-time path"
    );
    assert_eq!(days_fact.parent_path, ["mean_time_to_happen"]);

    let modifier = lower_with_profile(
        parse(
            FileFormat::Script,
            concat!(
                "country_event = { mean_time_to_happen = { ",
                "modifier = { factor = 0.5 always = yes }",
                " } }\n",
            ),
        ),
        &path,
        &rules,
        &profile(),
    );
    let modifier_property = modifier
        .properties()
        .iter()
        .find(|property| property.key == "modifier")
        .expect("modifier-rule child");
    let modifier_fact = modifier
        .scope_facts()
        .iter()
        .find(|fact| fact.range == modifier_property.key_range)
        .expect("the child key statically eliminates the event mean-time transition");
    assert_eq!(modifier_fact.context, "modifier_rule");
    assert!(modifier_fact.parent_path.is_empty());

    let empty = lower_with_profile(
        parse(
            FileFormat::Script,
            "country_event = { mean_time_to_happen = { } }\n",
        ),
        &path,
        &rules,
        &profile(),
    );
    let empty_index = empty
        .properties()
        .iter()
        .position(|property| property.key == "mean_time_to_happen")
        .expect("empty ambiguous block");
    let children = super::property_children(empty.properties());
    let candidates = rules
        .exact_semantic_rules("mean_time_to_happen")
        .filter(|rule| {
            rule.context == "root:event"
                && rule.parent_path.is_empty()
                && matches!(rule.shape, RuleShape::Node)
        })
        .collect::<Vec<_>>();
    assert!(
        super::statically_selected_transition(
            &candidates,
            empty.properties(),
            &children,
            empty_index,
            &rules,
            "type:event",
            &[],
            false,
        )
        .is_none(),
        "an empty block must not guess between conflicting transitions"
    );
}

#[test]
fn workspace_backed_child_keys_never_eliminate_a_transition_during_lowering() {
    let rules = first_party_rules().expect("embedded rules");
    assert!(
        super::child_key_may_match(
            &rules,
            "root:game_age",
            &["abilities".to_owned()],
            "workspace_defined_ability",
        ),
        "a type matcher can be satisfied by a later workspace definition"
    );
    assert!(
        super::child_key_may_match(
            &rules,
            "root:government_reform",
            &["custom_attributes".to_owned()],
            "workspace_defined_attribute",
        ),
        "a dynamic matcher can be satisfied by a later workspace value set"
    );
    assert!(
        !super::child_key_may_match(&rules, "modifier_rule", &[], "workspace_defined_ability",),
        "a context with only exact alternatives can still be ruled out"
    );
}

#[test]
fn skipped_type_roots_still_cache_descendant_scope_facts() {
    let rules = first_party_rules().expect("embedded rules");
    let path = LogicalPath::parse("common/on_actions/scope_hir.txt").expect("logical path");
    let hir = lower_with_profile(
        parse(
            FileFormat::Script,
            concat!(
                "on_harmonized_religiongroup = { ",
                "random_events = { int = province_event.1 }",
                " }\n",
            ),
        ),
        &path,
        &rules,
        &profile(),
    );

    let event = hir
        .properties()
        .iter()
        .find(|property| property.key == "int")
        .expect("random event entry");
    assert!(
        hir.scope_facts()
            .iter()
            .any(|fact| fact.range == event.key_range),
        "skip-root lowering must recurse below the selected semantic root"
    );
}

#[test]
fn replace_scope_resolves_static_links_into_register_values() {
    assert_eq!(
        super::repeated_scope_register_depth("fromfrom", "from"),
        Some(1)
    );
    assert_eq!(
        super::repeated_scope_register_depth("previous_owner", "previous"),
        None
    );

    let rules = first_party_rules().expect("embedded rules");
    let path = LogicalPath::parse("common/buildings/scope_hir.txt").expect("logical path");
    let hir = lower_with_profile(
        parse(
            FileFormat::Script,
            concat!(
                "test_building = { ",
                "on_built = { cossack_infantry = FROM }",
                " }\n",
            ),
        ),
        &path,
        &rules,
        &profile(),
    );

    let command = hir
        .properties()
        .iter()
        .find(|property| property.key == "cossack_infantry")
        .expect("nested effect");
    let fact = hir
        .scope_fact(command.key_range, "effect")
        .expect("effect scope fact");
    assert_eq!(
        fact.state.current.first(),
        Some(&ScopeValue::Known(vec!["province".to_owned()]))
    );
    assert_eq!(
        fact.state.from.first(),
        Some(&ScopeValue::Known(vec!["country".to_owned()]))
    );

    let province = ScopeValue::Known(vec!["province".to_owned()]);
    let state = ScopeState::initial(province.clone());
    assert_eq!(
        resolve_scope_expression(&state, "OwNeR.CAPITAL_SCOPE", &rules, &profile()),
        province
    );
    assert_eq!(
        resolve_scope_expression(&state, "owner.missing_link", &rules, &profile()),
        ScopeValue::Unknown
    );

    let mut invalid_register_rule = rules.model().semantic.rules[0].clone();
    invalid_register_rule.push_scope = None;
    invalid_register_rule.replace_scope = vec![
        ("from_owner".to_owned(), "country".to_owned()),
        ("previous_owner".to_owned(), "country".to_owned()),
    ];
    let unchanged = super::child_scope_state(&state, &invalid_register_rule, &rules, &profile());
    assert!(unchanged.from.is_empty());
    assert!(unchanged.previous.is_empty());
}
