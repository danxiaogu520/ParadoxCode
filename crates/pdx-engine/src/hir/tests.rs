use super::{
    HirParameterReferenceKind, HirReferenceOrigin, MacroTemplateFragment, MacroTemplateItem,
    MacroTemplateValue, ScopeState, ScopeValue, lower, lower_with_profile, property_children,
    resolve_scope_expression, semantic_root_context,
};
use pdx_game::eu4::{bootstrap_rules, first_party_rules, profile};
use pdx_parser::{FileFormat, parse};
use pdx_rules::{GameProfile, KeyMatcher, RuleSet, RuleShape, ValueMatcher};
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
fn shared_type_paths_use_the_top_level_key_filter() {
    let rules = first_party_rules().expect("first-party rules");
    let path = LogicalPath::parse("common/estate_crown_land/00_interactions.txt").expect("path");

    assert_eq!(
        semantic_root_context(&rules, Some(&path), "interaction").as_deref(),
        Some("type:estate_interaction")
    );
    assert_eq!(
        semantic_root_context(&rules, Some(&path), "bonus").as_deref(),
        Some("type:estate_crown_land_bonus")
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
fn scalar_records_whether_the_value_was_quoted() {
    let parsed = parse(FileFormat::Script, "root = { a = \"x\" b = y }\n");
    let hir = lower(parsed, &RuleSet::empty());

    let a = hir
        .properties()
        .iter()
        .find(|property| property.key == "a")
        .expect("a property");
    let b = hir
        .properties()
        .iter()
        .find(|property| property.key == "b")
        .expect("b property");
    assert!(a.scalar.as_ref().expect("a scalar").quoted);
    assert!(!b.scalar.as_ref().expect("b scalar").quoted);
}

#[test]
fn quoted_string_values_are_not_localisation_references() {
    let path = LogicalPath::parse("events/test.txt").expect("logical path");
    let hir = lower_with_profile(
        parse(
            FileFormat::Script,
            concat!(
                "country_event = { id = a.1 option = { ",
                "name = option_a ",
                "custom_tooltip = \" \" ",
                "custom_tooltip = tooltip_key ",
                "} }\n",
            ),
        ),
        &path,
        &first_party_rules().expect("first-party rules"),
        &profile(),
    );
    let localisation = hir
        .references()
        .iter()
        .filter(|reference| reference.kind == "localisation")
        .map(|reference| reference.name.as_str())
        .collect::<Vec<_>>();
    assert!(
        !localisation.contains(&" "),
        "quoted literals must not resolve as localisation symbols: {localisation:?}"
    );
    assert!(localisation.contains(&"tooltip_key"));
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
fn explicit_type_localisation_fields_are_associated_with_instances() {
    let path = LogicalPath::parse("events/test.txt").expect("logical path");
    let mut model = first_party_rules()
        .expect("first-party rules")
        .model()
        .clone();
    model.semantic.rules.retain(|rule| {
        !(rule.context.eq_ignore_ascii_case("root:event")
            && matches!(&rule.key, KeyMatcher::Exact(key) if key.eq_ignore_ascii_case("title"))
            && matches!(rule.value, ValueMatcher::Localisation))
    });
    let rules = RuleSet::from_model(model);
    let hir = lower_with_profile(
        parse(
            FileFormat::Script,
            "country_event = { id = event_one title = event_one_title }\n",
        ),
        &path,
        &rules,
        &GameProfile::empty(rules.game_id()),
    );
    let title_range = hir
        .properties()
        .iter()
        .find(|property| property.key == "title")
        .and_then(|property| property.scalar.as_ref())
        .map(|scalar| scalar.range)
        .expect("event title range");
    let hover_references = super::derived_localisation_references_for_hover(&hir, &path, &rules);
    assert!(hover_references.iter().any(|reference| {
        reference.kind == "localisation"
            && reference.name == "event_one_title"
            && reference.range == title_range
    }));
    assert!(hover_references.iter().any(|reference| {
        reference.origin == HirReferenceOrigin::DerivedLocalisation
            && reference.name == "event_one_title"
            && reference.range == title_range
    }));
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
        "[[optional] enabled = yes ] [[amount] guarded = $amount$ ] }\n",
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
    assert_eq!(hir.parameter_references().len(), 6);
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
        4
    );
    let first_owner = hir.parameter_definitions()[0].owner_range;
    assert_eq!(hir.parameter_definitions_for_owner(first_owner).count(), 2);
    assert_eq!(hir.parameter_references_for_owner(first_owner).count(), 5);
    assert!(hir.parameter_is_required(first_owner, "amount"));
    assert!(!hir.parameter_is_required(first_owner, "optional"));
    let optional_position =
        u32::try_from(source.find("optional").expect("optional parameter")).expect("position");
    assert_eq!(
        hir.parameter_reference_at(optional_position)
            .map(|reference| reference.name.as_str()),
        Some("optional")
    );
    assert!(hir.parameter_reference_at(first_owner.end()).is_none());

    let concatenated_source =
        "tooltip_name = { value = PREFIX_$optional$_SUFFIX required_value = $required$ }\n";
    let concatenated_hir = lower_with_profile(
        parse(FileFormat::Script, concatenated_source),
        &path,
        &bootstrap_rules(),
        &profile(),
    );
    let concatenated_owner = concatenated_hir.parameter_definitions()[0].owner_range;
    assert!(!concatenated_hir.parameter_is_required(concatenated_owner, "optional"));
    assert!(concatenated_hir.parameter_is_required(concatenated_owner, "required"));

    let conditional_source = "conditional = { [[feature] value = $amount$ ] }\n";
    let conditional_hir = lower_with_profile(
        parse(FileFormat::Script, conditional_source),
        &path,
        &bootstrap_rules(),
        &profile(),
    );
    let conditional_owner = conditional_hir.parameter_definitions()[0].owner_range;
    assert!(!conditional_hir.parameter_is_required(conditional_owner, "feature"));
    assert!(
        !conditional_hir.parameter_is_required(conditional_owner, "amount"),
        "a compact boolean signature must not treat a cross-conditional dependency as unconditional"
    );

    let runtime_branch_source = concat!(
        "branch = { if = { limit = { government = $limit_government$ } ",
        "add_republican_tradition = $republican_tradition$ } ",
        "else = { add_legitimacy = $amount$ } }\n",
    );
    let runtime_branch_hir = lower_with_profile(
        parse(FileFormat::Script, runtime_branch_source),
        &path,
        &bootstrap_rules(),
        &profile(),
    );
    let runtime_branch_owner = runtime_branch_hir.parameter_definitions()[0].owner_range;
    assert!(
        !runtime_branch_hir.parameter_is_required(runtime_branch_owner, "amount")
            && !runtime_branch_hir
                .parameter_is_required(runtime_branch_owner, "republican_tradition"),
        "mutually exclusive runtime branch values are optional at the macro call site"
    );
    assert!(
        runtime_branch_hir.parameter_is_required(runtime_branch_owner, "limit_government"),
        "a branch limit must still receive its parameter"
    );
}

#[test]
fn scripted_macro_lowering_keeps_body_context_calls_and_local_parameter_uses() {
    let rules = first_party_rules().expect("first-party rules");
    let path = LogicalPath::parse("common/scripted_effects/rewrite.txt").expect("logical path");
    let source = concat!(
        "apply_effect = { value = yes }\n",
        "wrapper = { ",
        "apply_effect = yes ",
        "$TARGET$ = { value = $AMOUNT$ } ",
        "effect = \"$PROVINCE$ = { add = $AMOUNT$ }\" ",
        "[[optional] value = $AMOUNT$ ] }\n",
    );
    let hir = lower_with_profile(parse(FileFormat::Script, source), &path, &rules, &profile());

    assert!(hir.references().iter().any(|reference| {
        reference.origin == HirReferenceOrigin::ScriptedMacro
            && reference.kind == "scripted_effect"
            && reference.name == "apply_effect"
    }));
    let body_property = hir
        .properties()
        .iter()
        .find(|property| {
            property.key == "apply_effect"
                && property.path.first().is_some_and(|root| root == "wrapper")
        })
        .expect("macro body property");
    let body_fact = hir
        .scope_facts()
        .iter()
        .find(|fact| fact.range == body_property.key_range)
        .expect("macro body scope fact");
    assert_eq!(body_fact.context, "effect");

    assert!(
        hir.parameter_definitions().iter().any(|definition| {
            definition.name == "TARGET" && definition.owner_range == body_property.range
        }) || hir
            .parameter_definitions()
            .iter()
            .any(|definition| definition.name == "TARGET")
    );
    assert!(hir.parameter_references().iter().any(|reference| {
        reference.name == "TARGET" && reference.kind == HirParameterReferenceKind::KeySubstitution
    }));
    assert!(hir.parameter_references().iter().any(|reference| {
        reference.name == "PROVINCE"
            && reference.kind == HirParameterReferenceKind::OpaqueTextSubstitution
    }));
    assert!(
        hir.parameter_references()
            .iter()
            .any(|reference| reference.kind == HirParameterReferenceKind::Conditional)
    );

    let trigger_path =
        LogicalPath::parse("common/scripted_triggers/rewrite.txt").expect("trigger path");
    let trigger_hir = lower_with_profile(
        parse(
            FileFormat::Script,
            "apply_trigger = { always = yes }\nwrapper_trigger = { apply_trigger = yes }\n",
        ),
        &trigger_path,
        &rules,
        &profile(),
    );
    let trigger_root = trigger_hir
        .scope_facts()
        .iter()
        .find(|fact| fact.parent_path.is_empty())
        .expect("trigger root fact");
    assert_eq!(trigger_root.context, "trigger");
    assert!(trigger_hir.references().iter().any(|reference| {
        reference.origin == HirReferenceOrigin::ScriptedMacro
            && reference.kind == "scripted_trigger"
            && reference.name == "apply_trigger"
    }));
}

#[test]
fn scripted_macro_templates_preserve_order_conditionals_and_token_fragments() {
    let rules = first_party_rules().expect("first-party rules");
    let path = LogicalPath::parse("common/scripted_effects/template.txt").expect("logical path");
    let source = concat!(
        "wrapper = { ",
        "prefix_$TARGET$ = $VALUE$ ",
        "[[OPTION] add_prestige = $VALUE$ ] ",
        "[[!SKIP] FRA GER ] ",
        "nested = { $KEY$ = yes }",
        " }\n",
    );
    let hir = lower_with_profile(parse(FileFormat::Script, source), &path, &rules, &profile());

    let template = hir
        .macro_template(
            "scripted_effect",
            "wrapper",
            hir.definitions()
                .iter()
                .find(|definition| definition.name == "wrapper")
                .expect("wrapper definition")
                .range,
        )
        .expect("macro template");
    assert_eq!(template.items.len(), 4);

    let MacroTemplateItem::Property(first) = &template.items[0] else {
        panic!("first item must be a property");
    };
    assert_eq!(
        first.key.fragments,
        [
            MacroTemplateFragment::Literal("prefix_".to_owned()),
            MacroTemplateFragment::Parameter {
                name: "TARGET".to_owned(),
                range: hir
                    .parameter_references()
                    .iter()
                    .find(|reference| reference.name == "TARGET")
                    .expect("target reference")
                    .range,
            },
        ]
    );
    let MacroTemplateValue::Scalar(value) = &first.value else {
        panic!("first value must be scalar");
    };
    assert!(matches!(
        value.fragments.as_slice(),
        [MacroTemplateFragment::Parameter { name, .. }] if name == "VALUE"
    ));

    let MacroTemplateItem::Conditional(optional) = &template.items[1] else {
        panic!("second item must be conditional");
    };
    assert_eq!(optional.name, "OPTION");
    assert!(!optional.negated);
    assert!(matches!(
        optional.items.as_slice(),
        [MacroTemplateItem::Property(_)]
    ));

    let MacroTemplateItem::Conditional(skipped) = &template.items[2] else {
        panic!("third item must be conditional");
    };
    assert_eq!(skipped.name, "SKIP");
    assert!(skipped.negated);
    assert!(matches!(
        skipped.items.as_slice(),
        [
            MacroTemplateItem::BareValue(_),
            MacroTemplateItem::BareValue(_)
        ]
    ));

    let MacroTemplateItem::Property(nested) = &template.items[3] else {
        panic!("fourth item must be nested property");
    };
    assert!(matches!(nested.value, MacroTemplateValue::Block { .. }));
}

#[test]
fn scripted_macro_templates_skip_syntax_damaged_owners() {
    let rules = first_party_rules().expect("first-party rules");
    let path = LogicalPath::parse("common/scripted_effects/broken.txt").expect("logical path");
    let hir = lower_with_profile(
        parse(FileFormat::Script, "broken = { add_prestige = $VALUE$\n"),
        &path,
        &rules,
        &profile(),
    );
    assert!(hir.macro_templates().is_empty());
}

#[test]
fn scripted_macro_lowering_retains_scalar_candidates_for_signature_resolution() {
    let rules = first_party_rules().expect("first-party rules");
    let path =
        LogicalPath::parse("common/scripted_effects/value_matchers.txt").expect("logical path");
    let source = "wrapper = { apply_effect = no apply_effect = yes }\n";
    let hir = lower_with_profile(parse(FileFormat::Script, source), &path, &rules, &profile());

    let apply_effect_properties = hir
        .properties()
        .iter()
        .filter(|property| property.key == "apply_effect")
        .collect::<Vec<_>>();
    assert_eq!(apply_effect_properties.len(), 2);
    let yes_property = apply_effect_properties
        .iter()
        .find(|property| {
            property
                .scalar
                .as_ref()
                .is_some_and(|scalar| scalar.value == "yes")
        })
        .expect("yes macro call");
    let no_property = apply_effect_properties
        .iter()
        .find(|property| {
            property
                .scalar
                .as_ref()
                .is_some_and(|scalar| scalar.value == "no")
        })
        .expect("no macro call");

    let references = hir
        .references()
        .iter()
        .filter(|reference| {
            reference.origin == HirReferenceOrigin::ScriptedMacro
                && reference.kind == "scripted_effect"
                && reference.name == "apply_effect"
        })
        .collect::<Vec<_>>();
    assert_eq!(references.len(), 2);
    assert!(
        references
            .iter()
            .any(|reference| reference.range == yes_property.key_range)
    );
    assert!(
        references
            .iter()
            .any(|reference| reference.range == no_property.key_range)
    );
}

#[test]
fn scripted_macro_lowering_rejects_non_scalar_block_matchers() {
    let original_rules = first_party_rules().expect("first-party rules");
    let mut model = original_rules.model().clone();
    for rule in &mut model.semantic.rules {
        if matches!(&rule.key, KeyMatcher::Type(type_name) if type_name == "scripted_effect")
            && matches!(rule.shape, RuleShape::Node | RuleShape::ValueClause)
        {
            rule.value = ValueMatcher::Exact("not-a-block-value".to_owned());
        }
    }
    let rules = RuleSet::from_model(model);
    let path =
        LogicalPath::parse("common/scripted_effects/block_matcher.txt").expect("logical path");
    let hir = lower_with_profile(
        parse(FileFormat::Script, "wrapper = { apply_effect = { } }\n"),
        &path,
        &rules,
        &profile(),
    );

    assert!(!hir.references().iter().any(|reference| {
        reference.origin == HirReferenceOrigin::ScriptedMacro
            && reference.kind == "scripted_effect"
            && reference.name == "apply_effect"
    }));
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
fn profile_aware_lowering_indexes_definitions_inside_quoted_effect_arguments() {
    let rules = bootstrap_rules();
    let path = LogicalPath::parse("missions/quoted_effect.txt").expect("logical path");
    let source = "mission = { first_effect = \"set_country_flag = embedded_flag\" }\n";

    let hir = lower_with_profile(parse(FileFormat::Script, source), &path, &rules, &profile());
    let definition = hir
        .definitions()
        .iter()
        .find(|definition| definition.kind == "country_flag" && definition.name == "embedded_flag")
        .expect("embedded flag definition");
    assert_eq!(
        &source[usize::try_from(definition.selection_range.start()).expect("start")
            ..usize::try_from(definition.selection_range.end()).expect("end")],
        "embedded_flag"
    );
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
fn first_party_typed_values_produce_workspace_symbol_references() {
    let rules = first_party_rules().expect("first-party rules");
    let path = LogicalPath::parse("events/typed_reference.txt").expect("logical path");
    let source = concat!(
        "country_event = { id = declared.1 }\n",
        "country_event = { id = caller.1 immediate = { ",
        "country_event = { id = declared.1 } } }\n",
    );
    let hir = lower_with_profile(parse(FileFormat::Script, source), &path, &rules, &profile());

    let typed = hir
        .references()
        .iter()
        .filter(|reference| reference.origin == HirReferenceOrigin::SemanticTyped)
        .collect::<Vec<_>>();
    assert_eq!(typed.len(), 1, "typed references: {typed:?}");
    assert_eq!(typed[0].kind, "event");
    assert_eq!(typed[0].name, "declared.1");
    let reference_start =
        u32::try_from(source.rfind("declared.1").expect("call id")).expect("reference offset");
    assert_eq!(typed[0].range.start(), reference_start);
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
    assert_eq!(fact.state.from, vec![ScopeValue::Unknown]);
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
    let capital = hir
        .properties()
        .iter()
        .find(|property| property.key == "capital_scope")
        .expect("capital scope transition");
    let capital_fact = hir
        .scope_facts()
        .iter()
        .find(|fact| fact.range == capital.key_range)
        .expect("capital scope fact");
    assert_eq!(
        capital_fact
            .transition
            .as_ref()
            .and_then(|state| state.current.first()),
        Some(&ScopeValue::Known(vec!["province".to_owned()]))
    );
}

#[test]
fn on_action_lowering_seeds_distinct_root_this_and_from_registers() {
    let rules = first_party_rules().expect("embedded rules");
    let path = LogicalPath::parse("common/on_actions/mercenary.txt").expect("logical path");
    let hir = lower_with_profile(
        parse(
            FileFormat::Script,
            "on_mercenary_recruited = { on_mercenary_recruited_effect = yes }\n",
        ),
        &path,
        &rules,
        &profile(),
    );

    let fact = hir.scope_facts().first().expect("on_action scope fact");
    assert_eq!(fact.context, "type:on_action");
    assert_eq!(
        fact.state.root,
        ScopeValue::Known(vec!["mercenary_company".to_owned()])
    );
    assert_eq!(
        fact.state.current,
        vec![ScopeValue::Known(vec!["province".to_owned()])]
    );
    assert_eq!(
        fact.state.from,
        vec![ScopeValue::Known(vec!["country".to_owned()])]
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
        super::statically_selected_transition(super::StaticTransitionInput {
            matching: &candidates,
            properties: empty.properties(),
            property_children: &children,
            property_index: empty_index,
            rules: &rules,
            context: "type:event",
            parent_path: &[],
            transparent: false,
        })
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
