use super::scope::property_children;
use super::{
    HirParameterReferenceKind, HirReferenceOrigin, ScopeState, ScopeValue, TemplateFragment,
    TemplateItem, TemplateValue, is_ir_scope_link, lower, lower_shared_with_ir,
    transition_ir_scope,
};
use game::eu4::{bootstrap_rules, first_party_ir, profile, runtime_rules};
use parser::{FileFormat, parse};
use rules::{GameProfile, RuleSet};
use std::sync::Arc;
use text::{LogicalPath, TextRange};

#[test]
fn ir_symbol_fact_dependencies_include_missing_members_and_template_templates() {
    let ir = first_party_ir().unwrap();
    let rules = RuleSet::from_ir_catalog(&ir);
    for (path, format, source, dependency) in [
        (
            "localisation/test_l_english.yml",
            FileFormat::Localisation,
            "l_english:\n key:0 \"value\"\n",
            false,
        ),
        (
            "events/test.txt",
            FileFormat::Script,
            "country_event = { id = test.1 immediate = { add_prestige = 1 } }",
            false,
        ),
        (
            "events/test.txt",
            FileFormat::Script,
            "country_event = { id = test.1 trigger = { religion = missing_religion } }",
            true,
        ),
        (
            "events/test.txt",
            FileFormat::Script,
            "country_event = { id = test.1 immediate = { missing_effect = { payload = yes } } }",
            true,
        ),
        (
            "events/test.txt",
            FileFormat::Script,
            "country_event = { id = test.1 immediate = { event_target:missing_target = { add_prestige = 1 } } }",
            true,
        ),
    ] {
        let hir = lower_shared_with_ir(
            Arc::new(parse(format, source)),
            &LogicalPath::parse(path).unwrap(),
            &rules,
            &ir.game.profile,
            &ir,
        );
        assert_eq!(hir.depends_on_symbol_facts(), dependency, "{source}");
    }
}

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
        ["value", "yes"]
    );
}

#[test]
fn range_properties_match_full_scan_for_repeated_and_nested_blocks() {
    let source = "root = { child = yes root = { child = no } } root = { child = maybe } broken = {";
    let hir = lower(parse(FileFormat::Script, source), &RuleSet::empty());
    for property in hir.properties() {
        for range in [
            property.range,
            property.key_range,
            property.value_range.unwrap_or(property.range),
        ] {
            let expected = hir
                .properties()
                .iter()
                .filter(|candidate| {
                    range.start() <= candidate.range.start() && candidate.range.end() <= range.end()
                })
                .map(|candidate| candidate.key_range)
                .collect::<Vec<_>>();
            let actual = hir
                .properties_in_range(range)
                .map(|candidate| candidate.key_range)
                .collect::<Vec<_>>();
            assert_eq!(actual, expected, "{range:?}");
            assert_eq!(
                hir.property_at_key_range(range).map(std::ptr::from_ref),
                hir.properties()
                    .iter()
                    .find(|property| property.key_range == range)
                    .map(std::ptr::from_ref),
                "exact key {range:?}"
            );
        }
    }
}

#[test]
fn ir_lowering_tracks_schema_fields_for_events_on_actions_decisions_and_dynamic_defs() {
    let rules = runtime_rules().expect("first-party rules");
    let ir = first_party_ir().expect("first-party IR");
    let cases = [
        (
            "events/test.txt",
            "country_event = { id = test.1 title = test_title option = { name = option_title } trigger = { always = yes } }",
            "country_event",
        ),
        (
            "common/on_actions/test.txt",
            "on_yearly_pulse = { events = { 1 = test.1 } random_events = { 1 = 0 2 = test.2 } }",
            "on_yearly_pulse",
        ),
        (
            "decisions/test.txt",
            "country_decisions = { test_decision = { potential = { always = yes } effect = { add_prestige = 1 } } }",
            "country_decisions",
        ),
        (
            "common/scripted_effects/test.txt",
            "test_effect = { add_prestige = $AMOUNT$ }",
            "test_effect",
        ),
    ];
    for (path, source, first_key) in cases {
        let path = LogicalPath::parse(path).expect("logical path");
        let hir = lower_shared_with_ir(
            Arc::new(parse(FileFormat::Script, source)),
            &path,
            &rules,
            &profile(),
            &ir,
        );
        assert!(hir.uses_ir());
        assert!(!hir.schema_facts().is_empty(), "{path}");
        let property = hir
            .properties()
            .iter()
            .find(|property| property.key == first_key)
            .expect("root property");
        assert!(
            hir.field_fact_at(property.key_range).is_some(),
            "{path} {first_key}"
        );
        if path.as_str() == "events/test.txt" {
            assert!(
                hir.definitions()
                    .iter()
                    .any(|definition| definition.kind.eq_ignore_ascii_case("event"))
            );
            let event_body = property.value_range.expect("event body range");
            assert!(
                hir.schema_at(event_body.start())
                    .is_some_and(|fact| !fact.subtypes.is_empty())
            );
            assert!(hir.definition_attributes().iter().any(|attributes| {
                attributes.kind.eq_ignore_ascii_case("event")
                    && attributes
                        .subtypes
                        .iter()
                        .any(|subtype| subtype.eq_ignore_ascii_case("country"))
            }));
            assert!(hir.references().iter().any(|reference| {
                reference.kind.eq_ignore_ascii_case("localisation")
                    && reference.name == "test_title"
            }));
            assert!(hir.references().iter().any(|reference| {
                reference.kind.eq_ignore_ascii_case("localisation")
                    && reference.name == "option_title"
            }));
        }
        if path.as_str() == "common/on_actions/test.txt" {
            assert!(
                hir.references()
                    .iter()
                    .any(|reference| reference.kind.eq_ignore_ascii_case("event")),
                "{:?}",
                hir.references()
            );
            assert!(
                !hir.references()
                    .iter()
                    .any(|reference| reference.name == "0"
                        && reference.kind.eq_ignore_ascii_case("event"))
            );
            assert!(
                hir.references()
                    .iter()
                    .any(|reference| reference.name == "test.2"
                        && reference.subtype.as_deref() == Some("country"))
            );
        }
    }
    let path = LogicalPath::parse("common/scripted_effects/test.txt").expect("logical path");
    let hir = lower_shared_with_ir(
        Arc::new(parse(FileFormat::Script, cases[3].1)),
        &path,
        &rules,
        &profile(),
        &ir,
    );
    assert!(
        hir.dynamic_templates()
            .iter()
            .any(|template| template.name == "test_effect")
    );
    let path = LogicalPath::parse("localisation/events_l_english.yml").expect("logical path");
    let hir = lower_shared_with_ir(
        Arc::new(parse(
            FileFormat::Localisation,
            "l_english:\n test_key:0 \"Text\"\n",
        )),
        &path,
        &rules,
        &profile(),
        &ir,
    );
    assert!(hir.uses_ir());
    assert!(
        hir.definitions()
            .iter()
            .any(|definition| definition.kind.as_ref() == "localisation"
                && definition.name == "test_key")
    );
}

#[test]
fn ir_scope_transitions_update_root_this_registers_and_respect_link_from_scopes() {
    let ir = first_party_ir().expect("first-party IR");
    let sym = |name: &str| {
        ir.strings()
            .lookup_folded(name)
            .unwrap_or_else(|| panic!("missing IR symbol {name}"))
    };
    let mut state = ScopeState::initial(ScopeValue::known_single("country"));
    let effect = rules::ir::ScopeEffect {
        set: vec![
            (sym("root"), sym("province")),
            (sym("this"), sym("province")),
            (sym("from"), sym("unit")),
            (sym("prev"), sym("province")),
        ]
        .into_boxed_slice(),
        ..rules::ir::ScopeEffect::default()
    };
    state = super::ir_lowering::transition_state(&ir, state, Some(&effect), "noop");
    assert_eq!(state.root, ScopeValue::known_single("province"));
    assert_eq!(state.current[0], ScopeValue::known_single("province"));
    assert_eq!(state.from[0], ScopeValue::known_single("unit"));
    assert_eq!(state.previous[0], ScopeValue::known_single("province"));

    let starting_country = ScopeState::initial(ScopeValue::known_single("country"));
    let traversed =
        super::ir_lowering::transition_state(&ir, starting_country, None, "capital_scope");
    assert_eq!(traversed.current[0], ScopeValue::known_single("province"));
    assert_eq!(traversed.previous[0], ScopeValue::known_single("country"));

    let province = ScopeState::initial(ScopeValue::known_single("province"));
    let owner_link = super::ir_lowering::transition_state(&ir, province, None, "owner");
    assert_eq!(owner_link.current[0], ScopeValue::known_single("country"));
    assert!(ir.scope_matches(Some(sym("province")), "province"));
    assert!(!ir.scope_matches(Some(sym("province")), "trade_node"));

    let starting_province = ScopeState::initial(ScopeValue::known_single("province"));
    let rejected =
        super::ir_lowering::transition_state(&ir, starting_province.clone(), None, "capital_scope");
    assert_eq!(rejected, starting_province);
    assert!(super::ir_lowering::link_or_register_matches(
        &ir,
        "fromfromfrom"
    ));
    assert!(!super::ir_lowering::link_or_register_matches(
        &ir,
        "fromgarbage"
    ));
}

#[test]
fn ir_scope_register_roles_work_with_arbitrary_declared_spellings() {
    let mut ir = (*first_party_ir().unwrap()).clone();
    for register in &mut ir.scopes.registers {
        let name = match register.role {
            rules::source::RegisterRole::Root => "origin",
            rules::source::RegisterRole::Current => "here",
            rules::source::RegisterRole::Previous => "prior_scope",
            rules::source::RegisterRole::From => "caller",
        };
        register.name = ir.strings.intern_folded(name);
    }
    let mut state = ScopeState::initial(ScopeValue::known_single("country"));
    state.root = ScopeValue::known_single("province");
    state.previous = vec![
        ScopeValue::known_single("country"),
        ScopeValue::known_single("unit"),
    ];
    state.from = vec![
        ScopeValue::known_single("province"),
        ScopeValue::known_single("advisor"),
    ];
    assert_eq!(transition_ir_scope(&ir, state.clone(), None, "HERE"), state);
    for (name, expected) in [
        ("ORIGIN", "province"),
        ("prior_scope_prior_scope", "unit"),
        ("CALLERCALLER", "advisor"),
    ] {
        assert_eq!(
            transition_ir_scope(&ir, state.clone(), None, name).current[0],
            ScopeValue::known_single(expected)
        );
    }
    assert!(!is_ir_scope_link(&ir, "ROOT"));
    assert!(!is_ir_scope_link(&ir, "FROMFROM"));
    assert!(!is_ir_scope_link(&ir, "caller_bad"));
    let effect = rules::ir::ScopeEffect {
        set: vec![(
            ir.strings.lookup_folded("origin").unwrap(),
            ir.strings.lookup_folded("advisor").unwrap(),
        )]
        .into_boxed_slice(),
        ..Default::default()
    };
    assert_eq!(
        transition_ir_scope(&ir, state, Some(&effect), "noop").root,
        ScopeValue::known_single("advisor")
    );
}

#[test]
fn ir_schema_fragment_keeps_its_root_schema_fact() {
    let ir = first_party_ir().expect("first-party IR");
    let schema = ir.schema_by_name("trigger").expect("trigger schema");
    let parsed = Arc::new(parse(FileFormat::Script, "always = yes"));
    let range = parsed.root().range();
    let hir = super::lower_ir_schema(
        Arc::clone(&parsed),
        &ir,
        schema,
        rules::ir::SubtypeSet::default(),
        ScopeState::initial(ScopeValue::Unknown),
        &rules::ir::NoSymbolFacts,
    );
    assert!(
        hir.schema_facts()
            .iter()
            .any(|fact| { fact.range == range && fact.schema == schema })
    );
    assert!(
        hir.schema_at(range.start())
            .is_some_and(|fact| fact.schema == schema)
    );
}

#[test]
fn ir_file_instance_seeds_root_scope_before_lowering_history_effects() {
    let ir = first_party_ir().expect("first-party IR");
    let rules = RuleSet::from_ir_catalog(&ir);
    for (path, expected) in [
        ("history/countries/ZZZ - Test.txt", "country"),
        ("history/provinces/1 - Test.txt", "province"),
    ] {
        let hir = lower_shared_with_ir(
            Arc::new(parse(
                FileFormat::Script,
                "1444.11.11 = { add_prestige = 1 }",
            )),
            &LogicalPath::parse(path).unwrap(),
            &rules,
            &ir.game.profile,
            &ir,
        );
        let root = hir
            .schema_facts()
            .iter()
            .find(|fact| fact.range.start() == 0)
            .expect("file root fact");
        assert_eq!(
            root.state.root,
            ScopeValue::known_single(expected),
            "{path}"
        );
        assert_eq!(
            root.state.current[0],
            ScopeValue::known_single(expected),
            "{path}"
        );
        assert!(
            hir.scope_facts()
                .iter()
                .all(|fact| { fact.state.root == ScopeValue::known_single(expected) }),
            "{path}"
        );
    }
}

#[test]
fn shared_type_paths_use_the_top_level_key_filter() {
    let ir = first_party_ir().unwrap();
    let catalog = RuleSet::from_ir_catalog(&ir);
    let path = LogicalPath::parse("common/estate_crown_land/00_interactions.txt").unwrap();
    let hir = lower_shared_with_ir(
        Arc::new(parse(
            FileFormat::Script,
            "interaction = { test_interaction = {} } bonus = { test_bonus = {} }",
        )),
        &path,
        &catalog,
        &ir.game.profile,
        &ir,
    );
    let names: Vec<_> = hir
        .schema_facts()
        .iter()
        .map(|fact| ir.strings().resolve(ir.schema(fact.schema).name))
        .collect();
    assert!(
        names.iter().any(|name| name.contains("interaction")),
        "{names:?}"
    );
    assert!(names.iter().any(|name| name.contains("bonus")), "{names:?}");
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
                "custom_tooltip = \"\" ",
                "custom_tooltip = tooltip_key ",
                "} }\n",
            ),
        ),
        &path,
        &runtime_rules().expect("first-party rules"),
        &profile(),
    );
    let localisation = hir
        .references()
        .iter()
        .filter(|reference| reference.kind.as_ref() == "localisation")
        .map(|reference| reference.name.as_ref())
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
        &runtime_rules().expect("first-party rules"),
        &profile(),
    );
    let derived = hir
        .references()
        .iter()
        .filter(|reference| reference.origin == HirReferenceOrigin::DerivedLocalisation)
        .map(|reference| reference.name.as_ref())
        .collect::<Vec<_>>();
    assert!(derived.contains(&"mission_one_title"));
    assert!(derived.contains(&"mission_one_desc"));
}

#[test]
fn ir_display_bindings_do_not_depend_on_structural_flags() {
    let path = LogicalPath::parse("common/ideas/all.txt").unwrap();
    let ir = first_party_ir().unwrap();
    let rules = RuleSet::from_ir_catalog(&ir);
    let hir = lower_shared_with_ir(
        Arc::new(parse(
            FileFormat::Script,
            "country_idea = { free = yes } other_idea = { free = no }",
        )),
        &path,
        &rules,
        &profile(),
        &ir,
    );
    let display = hir
        .binding_references_for_hover()
        .iter()
        .map(|reference| reference.name.as_str())
        .collect::<Vec<_>>();
    let required = hir
        .references()
        .iter()
        .map(|reference| reference.name.as_str())
        .collect::<Vec<_>>();
    for name in ["country_idea_start", "other_idea_start"] {
        assert!(display.contains(&name), "{display:?}");
        assert!(
            !required.contains(&name),
            "optional display bindings must not become mandatory: {required:?}"
        );
    }
}

#[test]
fn required_self_bindings_reach_the_diagnostics_set() {
    // Cultures and their groups carry required self bindings: the instance
    // name is the localisation key the game shows, so a missing key is a
    // diagnostic, not just a silent hover miss.
    let path = LogicalPath::parse("common/cultures/cultures.txt").expect("logical path");
    let hir = lower_with_profile(
        parse(
            FileFormat::Script,
            "nordic = { male_names = { } swedish = { } }\n",
        ),
        &path,
        &runtime_rules().expect("first-party rules"),
        &profile(),
    );
    let required = hir
        .references()
        .iter()
        .filter(|reference| reference.origin == HirReferenceOrigin::DerivedLocalisation)
        .map(|reference| reference.name.as_ref())
        .collect::<Vec<_>>();
    assert!(
        required.contains(&"swedish") && required.contains(&"nordic"),
        "culture and culture_group self bindings reach diagnostics: {required:?}"
    );
    assert!(!required.contains(&"male_names"));
}

#[test]
fn unbound_types_carry_no_implicit_localisation() {
    // The same-name convention exists only where a type declares it: an
    // unbound type has neither hover previews nor diagnostics from its
    // instance names.
    let path = LogicalPath::parse("common/on_action/a.txt").expect("logical path");
    let hir = lower_with_profile(
        parse(FileFormat::Script, "on_startup = { effect = { } }\n"),
        &path,
        &runtime_rules().expect("first-party rules"),
        &profile(),
    );
    let derived = hir
        .references()
        .iter()
        .filter(|reference| reference.origin == HirReferenceOrigin::DerivedLocalisation)
        .map(|reference| reference.name.as_ref())
        .collect::<Vec<_>>();
    assert!(
        !derived.contains(&"on_startup"),
        "no implicit same-name reference for the unbound on_action type: {derived:?}"
    );
}

#[test]
fn sprite_bindings_expand_template_and_semantic_fields() {
    let rules = runtime_rules().expect("first-party rules");

    // aspects_and_blessings: the GFX_$ template binding generates the sprite
    // name from the aspect's own name (hover path; the binding is not
    // required, so it stays out of the lowered reference set).
    let path = LogicalPath::parse("common/church_aspects/test.txt").expect("logical path");
    let hir = lower_with_profile(
        parse(FileFormat::Script, "my_aspect = { enable = yes }\n"),
        &path,
        &rules,
        &profile(),
    );
    let hover = super::derived_sprite_references_for_hover(&hir, &path, &rules);
    assert!(
        hover.iter().any(|reference| {
            reference.kind.as_ref() == "sprite"
                && reference.name == "GFX_my_aspect"
                && reference.origin == HirReferenceOrigin::DerivedSprite
        }),
        "the GFX_$ template must expand to the aspect name"
    );
    assert!(
        !hir.references()
            .iter()
            .any(|reference| reference.origin == HirReferenceOrigin::DerivedSprite),
        "non-required icon bindings stay out of the diagnostics reference set"
    );

    // building: the required GFX_$ template enters the lowered reference
    // set, so a missing building sprite is a diagnostic.
    let path = LogicalPath::parse("common/buildings/test.txt").expect("logical path");
    let hir = lower_with_profile(
        parse(FileFormat::Script, "my_building = { cost = 1 }\n"),
        &path,
        &rules,
        &profile(),
    );
    assert!(
        hir.references().iter().any(|reference| {
            reference.origin == HirReferenceOrigin::DerivedSprite
                && reference.kind.as_ref() == "sprite"
                && reference.name == "GFX_my_building"
        }),
        "the required GFX_$ template must reach the diagnostics reference set"
    );
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
        "mutually exclusive runtime branch values are optional at the dynamic call site"
    );
    assert!(
        runtime_branch_hir.parameter_is_required(runtime_branch_owner, "limit_government"),
        "a branch limit must still receive its parameter"
    );
}

#[test]
fn dynamic_lowering_keeps_body_context_calls_and_local_parameter_uses() {
    let rules = runtime_rules().expect("first-party rules");
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
        reference.origin == HirReferenceOrigin::SemanticTyped
            && reference.kind.as_ref() == "scripted_effect"
            && reference.name == "apply_effect"
    }));
    let body_property = hir
        .properties()
        .iter()
        .find(|property| {
            property.key == "apply_effect"
                && property.path.first().is_some_and(|root| root == "wrapper")
        })
        .expect("definition body property");
    let body_fact = hir
        .scope_facts()
        .iter()
        .find(|fact| fact.range == body_property.key_range)
        .expect("definition body scope fact");
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
        .find(|fact| fact.context == "trigger")
        .expect("trigger body fact");
    assert_eq!(trigger_root.context, "trigger");
    assert!(trigger_hir.references().iter().any(|reference| {
        reference.origin == HirReferenceOrigin::SemanticTyped
            && reference.kind.as_ref() == "scripted_trigger"
            && reference.name == "apply_trigger"
    }));
}

#[test]
fn dynamic_templates_preserve_order_conditionals_and_token_fragments() {
    let rules = runtime_rules().expect("first-party rules");
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
        .dynamic_template(
            "scripted_effect",
            "wrapper",
            hir.definitions()
                .iter()
                .find(|definition| definition.name == "wrapper")
                .expect("wrapper definition")
                .range,
        )
        .expect("dynamic template");
    assert_eq!(template.items.len(), 4);

    let TemplateItem::Property(first) = &template.items[0] else {
        panic!("first item must be a property");
    };
    assert_eq!(
        first.key.fragments,
        [
            TemplateFragment::Literal("prefix_".into()),
            TemplateFragment::Parameter {
                name: "TARGET".into(),
                range: hir
                    .parameter_references()
                    .iter()
                    .find(|reference| reference.name == "TARGET")
                    .expect("target reference")
                    .range,
            },
        ]
    );
    let TemplateValue::Scalar(value) = &first.value else {
        panic!("first value must be scalar");
    };
    assert!(matches!(
        value.fragments.as_slice(),
        [TemplateFragment::Parameter { name, .. }] if name.as_ref() == "VALUE"
    ));

    let TemplateItem::Conditional(optional) = &template.items[1] else {
        panic!("second item must be conditional");
    };
    assert_eq!(optional.name.as_ref(), "OPTION");
    assert!(!optional.negated);
    assert!(matches!(
        optional.items.as_slice(),
        [TemplateItem::Property(_)]
    ));

    let TemplateItem::Conditional(skipped) = &template.items[2] else {
        panic!("third item must be conditional");
    };
    assert_eq!(skipped.name.as_ref(), "SKIP");
    assert!(skipped.negated);
    assert!(matches!(
        skipped.items.as_slice(),
        [TemplateItem::BareValue(_), TemplateItem::BareValue(_)]
    ));

    let TemplateItem::Property(nested) = &template.items[3] else {
        panic!("fourth item must be nested property");
    };
    assert!(matches!(nested.value, TemplateValue::Block { .. }));
}

#[test]
fn dynamic_templates_retain_syntax_damaged_owners() {
    let rules = runtime_rules().expect("first-party rules");
    let path = LogicalPath::parse("common/scripted_effects/broken.txt").expect("logical path");
    let hir = lower_with_profile(
        parse(FileFormat::Script, "broken = { add_prestige = $VALUE$\n"),
        &path,
        &rules,
        &profile(),
    );
    assert_eq!(hir.dynamic_templates().len(), 1);
    assert!(!hir.syntax().errors().is_empty());
    assert!(
        hir.dynamic_templates()[0]
            .program
            .parameters
            .iter()
            .any(|name| name.eq_ignore_ascii_case("value"))
    );
}

#[test]
fn dynamic_lowering_retains_scalar_candidates_for_signature_resolution() {
    let rules = runtime_rules().expect("first-party rules");
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
        .expect("yes dynamic call");
    let no_property = apply_effect_properties
        .iter()
        .find(|property| {
            property
                .scalar
                .as_ref()
                .is_some_and(|scalar| scalar.value == "no")
        })
        .expect("no dynamic call");

    let references = hir
        .references()
        .iter()
        .filter(|reference| {
            reference.origin == HirReferenceOrigin::SemanticTyped
                && reference.kind.as_ref() == "scripted_effect"
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
fn profile_aware_lowering_produces_shared_typed_definitions_and_references() {
    let rules = bootstrap_rules();
    let path = LogicalPath::parse("events/profile_hir.txt").expect("logical path");
    let source = "country_event = { id = profile.1 title = profile_title immediate = { set_country_flag = seen } }\n";

    let hir = lower_with_profile(parse(FileFormat::Script, source), &path, &rules, &profile());

    assert!(hir.definitions().iter().any(|definition| {
        definition.kind.as_ref() == "event"
            && definition.name == "profile.1"
            && definition.selection_range != definition.range
    }));
    assert!(
        hir.definitions()
            .iter()
            .any(|definition| definition.kind.as_ref() == "country_flag"
                && definition.name == "seen")
    );
    assert!(hir.references().iter().any(|reference| {
        reference.kind.as_ref() == "localisation" && reference.name == "profile_title"
    }));
}

#[test]
fn ordinary_quoted_scalars_do_not_index_script_definitions() {
    let file=serde_json::from_value(serde_json::json!({"types":{"flag":{}},"files":{"fixture":{"path":"missions","ext":"txt","root":"root"}},"schemas":{"root":{"fields":{"execute":{"value":"scalar","card":"1"}}},"effect":{"fields":{"set_flag":{"value":"def<flag>","card":"0..*"}}}}})).unwrap();
    let ir = rules::lower::lower(
        &[("fixture.json".to_owned(), file)],
        rules::ir::GameConfig {
            profile: GameProfile::empty("fixture"),
        },
    )
    .unwrap();
    let source = "execute = \"set_flag = embedded_flag\"";
    let hir = lower_shared_with_ir(
        Arc::new(parse(FileFormat::Script, source)),
        &LogicalPath::parse("missions/quoted.txt").unwrap(),
        &RuleSet::from_ir_catalog(&ir),
        &ir.game.profile,
        &ir,
    );
    assert!(
        !hir.definitions()
            .iter()
            .any(|definition| definition.kind.as_ref() == "flag")
    );
}

#[test]
fn first_party_semantic_localisation_rules_produce_references_without_profile_shorthand() {
    let rules = runtime_rules().expect("first-party rules");
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
        reference.origin == HirReferenceOrigin::SemanticTyped
            && reference.kind.as_ref() == "localisation"
            && reference.name == "semantic_title"
    }));
}

#[test]
fn first_party_typed_values_produce_workspace_symbol_references() {
    let rules = runtime_rules().expect("first-party rules");
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
    assert_eq!(typed[0].kind.as_ref(), "event");
    assert_eq!(typed[0].name, "declared.1");
    let reference_start =
        u32::try_from(source.rfind("declared.1").expect("call id")).expect("reference offset");
    assert_eq!(typed[0].range.start(), reference_start);
}

#[test]
fn scope_facts_descend_through_dynamic_mission_blocks() {
    let rules = runtime_rules().expect("embedded rules");
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

    let hir = super::lower_with_profile(parse(FileFormat::Script, source), &path, &rules, &profile);

    assert!(hir.definitions().is_empty());
    assert!(
        !hir.references()
            .iter()
            .any(|reference| reference.kind.as_ref() == "localisation")
    );
}

#[test]
fn profile_lowering_caches_semantic_root_context_and_initial_scope() {
    let rules = runtime_rules().expect("embedded rules");
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

    let fact = hir
        .scope_facts()
        .iter()
        .find(|fact| fact.context == "event_body")
        .expect("event scope fact");
    assert_eq!(fact.context, "event_body");
    assert_eq!(
        fact.state.root,
        ScopeValue::known(vec!["country".to_owned()])
    );
    assert_eq!(
        fact.state.current,
        vec![ScopeValue::known(vec!["country".to_owned()])]
    );
    assert!(
        fact.state.from.is_empty(),
        "event FROM is not declared until a caller seeds it"
    );
    assert_eq!(hir.scope_fact(fact.range, "EVENT_BODY"), Some(fact));
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
        Some(&ScopeValue::known(vec!["province".to_owned()]))
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
        Some(&ScopeValue::known(vec!["province".to_owned()]))
    );
}

#[test]
fn on_action_lowering_seeds_distinct_root_this_and_from_registers() {
    let rules = runtime_rules().expect("embedded rules");
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

    let fact = hir
        .scope_facts()
        .iter()
        .find(|fact| fact.state.root == ScopeValue::known_single("mercenary_company"))
        .expect("on_action scope fact");
    assert!(fact.context.starts_with("on_action"));
    assert_eq!(
        fact.state.root,
        ScopeValue::known(vec!["mercenary_company".to_owned()])
    );
    assert_eq!(
        fact.state.current,
        vec![ScopeValue::known(vec!["province".to_owned()])]
    );
    assert_eq!(
        fact.state.from,
        vec![ScopeValue::known(vec!["country".to_owned()])]
    );
}

#[test]
fn equivalent_rule_alternatives_share_one_cached_transition() {
    let rules = runtime_rules().expect("embedded rules");
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
        Some(&ScopeValue::known(vec!["country".to_owned()]))
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
        days_fact.state.current.first(),
        Some(&ScopeValue::known(vec!["country".to_owned()]))
    );

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
    assert_eq!(
        modifier_fact.state.current.first(),
        Some(&ScopeValue::known(vec!["country".to_owned()]))
    );

    assert!(
        modifier
            .schema_facts()
            .iter()
            .any(|fact| ir_schema_name(fact.schema).contains("mean_time_to_happen"))
    );
}

#[test]
fn skipped_type_roots_still_cache_descendant_scope_facts() {
    let rules = runtime_rules().expect("embedded rules");
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
fn mission_trigger_boolean_containers_still_extract_scripted_references() {
    let rules = runtime_rules().expect("embedded rules");
    let path = LogicalPath::parse("missions/probe_scratch.txt").expect("logical path");
    // Real-world shape from a mod missions file: the scripted trigger sits under
    // trigger > OR > num_of_owned_provinces_with. Boolean containers (AND/OR/NOT)
    // must keep the trigger-context descent alive so the reference is extracted.
    let source = "series = { mission_one = { trigger = { OR = { num_of_owned_provinces_with = { value = 3 has_fort_building_trigger = yes } } } } }\n";
    let hir = lower_with_profile(parse(FileFormat::Script, source), &path, &rules, &profile());
    assert!(
        hir.references()
            .iter()
            .any(|reference| reference.kind.as_ref() == "scripted_trigger"
                && reference.name == "has_fort_building_trigger"),
        "scripted trigger reference must be extracted from mission trigger blocks"
    );

    let and_not = "series = { mission_one = { trigger = { AND = { has_fort_building_trigger = yes } NOT = { has_fort_building_trigger = yes } } } } }\n";
    let hir = lower_with_profile(
        parse(FileFormat::Script, and_not),
        &path,
        &rules,
        &profile(),
    );
    assert_eq!(
        hir.references()
            .iter()
            .filter(|reference| reference.kind.as_ref() == "scripted_trigger"
                && reference.name == "has_fort_building_trigger")
            .count(),
        2,
        "AND and NOT containers must keep extracting scripted trigger references"
    );
}

#[test]
fn profile_lowering_skips_substitutions_inside_condition_ranges() {
    let source = "block = { [[!$guard$] inner = $guard$ ] }\n";
    let path = LogicalPath::parse("common/scripted_effects/parameters.txt").expect("logical path");
    let hir = lower_with_profile(
        parse(FileFormat::Script, source),
        &path,
        &bootstrap_rules(),
        &profile(),
    );

    // The `!$guard$` head keeps only its conditional reference; the nested substitution is
    // represented by it, so references stay ordered and non-overlapping.
    let references = hir.parameter_references();
    assert_eq!(references.len(), 2);
    assert_eq!(references[0].kind, HirParameterReferenceKind::Conditional);
    assert_eq!(references[1].name, "guard");
    assert_eq!(references[1].range, TextRange::new(30, 37).unwrap());
    assert!(
        references
            .windows(2)
            .all(|items| items[0].range.end() <= items[1].range.start())
    );

    // Inside the `!$guard$` head: the conditional reference must resolve instead of the
    // nested-substitution gap that overlapping ranges previously produced.
    assert!(matches!(
        hir.parameter_reference_at(13),
        Some(reference) if reference.kind == HirParameterReferenceKind::Conditional
    ));
}

#[test]
fn empty_and_whitespace_sources_lower_to_construct_free_trees() {
    for format in [FileFormat::Script, FileFormat::Localisation] {
        let hir = lower(parse(format, " \n\t\n"), &RuleSet::empty());
        assert!(
            hir.syntax().errors().is_empty(),
            "blank input must stay error-free"
        );
        assert!(hir.properties().is_empty());
        assert!(hir.definitions().is_empty());
        assert!(hir.unknown_constructs().is_empty());
    }
}

#[test]
fn unterminated_block_cascades_keep_nested_properties_and_bounded_errors() {
    let source = "intact = yes\nouter = {\n inner = {\n deep = yes\n";
    let hir = lower(parse(FileFormat::Script, source), &RuleSet::empty());

    // Both unterminated blocks are reported exactly once each.
    assert_eq!(hir.syntax().errors().len(), 2);
    let keys = hir
        .properties()
        .iter()
        .map(|property| property.key.as_str())
        .collect::<Vec<_>>();
    for key in ["intact", "outer", "inner", "deep"] {
        assert!(
            keys.contains(&key),
            "property `{key}` must survive the cascade: {keys:?}"
        );
    }
    assert!(
        hir.unknown_constructs()
            .iter()
            .all(|unknown| unknown.range.end() <= u32::try_from(source.len()).unwrap_or(u32::MAX)),
        "no recovery construct may point past the source"
    );
}

#[test]
fn ir_template_presence_uses_declared_branch_and_guard_keys() {
    let mut ir = (*first_party_ir().unwrap()).clone();
    for (old, new) in [("if", "choose"), ("limit", "guard")] {
        let old = ir.strings.lookup_folded(old).unwrap();
        let new = ir.strings.intern_folded(new);
        for matcher in &mut ir.matchers {
            if matches!(matcher, rules::ir::Matcher::Literal(name) if *name == old) {
                *matcher = rules::ir::Matcher::Literal(new);
            }
        }
        for schema in &mut ir.schemas {
            if let Some(fields) = schema.exact.remove(&old) {
                schema.exact.insert(new, fields);
            }
        }
        for field in &mut ir.fields {
            if let Some(control) = &mut field.control
                && control.guard == Some(old)
            {
                control.guard = Some(new);
            }
        }
    }
    let source =
        "test_effect = { choose = { guard = { always = $GATE$ } add_prestige = $AMOUNT$ } }";
    let path = LogicalPath::parse("common/scripted_effects/presence.txt").unwrap();
    let hir = lower_shared_with_ir(
        Arc::new(parse(FileFormat::Script, source)),
        &path,
        &RuleSet::from_ir_catalog(&ir),
        &ir.game.profile,
        &ir,
    );
    let owner = hir
        .definitions()
        .iter()
        .find(|definition| definition.name == "test_effect")
        .unwrap()
        .range;
    assert!(hir.parameter_is_required(owner, "GATE"));
    assert!(!hir.parameter_is_required(owner, "AMOUNT"));
}

fn lower_with_profile(
    parsed: parser::ParsedFile,
    path: &LogicalPath,
    catalog: &RuleSet,
    profile: &GameProfile,
) -> super::HirFile {
    if profile.game_id == "eu4" {
        let ir = first_party_ir().unwrap();
        super::lower_shared_with_ir_and_facts(
            Arc::new(parsed),
            path,
            catalog,
            profile,
            &ir,
            &FixtureMembers { ir: &ir },
        )
    } else {
        super::lower_with_profile(parsed, path, catalog, profile)
    }
}

fn ir_schema_name(schema: rules::ir::SchemaId) -> String {
    let ir = first_party_ir().unwrap();
    ir.strings().resolve(ir.schema(schema).name).to_owned()
}

struct FixtureMembers<'a> {
    ir: &'a rules::ir::RulesIr,
}
impl rules::ir::SymbolFacts for FixtureMembers<'_> {
    fn type_member(&self, ty: rules::ir::TypeId, name: &str) -> bool {
        match self.ir.strings().resolve(self.ir.type_info(ty).name) {
            "scripted_effect" => ["apply_effect", "on_mercenary_recruited_effect"].contains(&name),
            "scripted_trigger" => matches!(name, "has_fort_building_trigger" | "apply_trigger"),
            _ => false,
        }
    }
}
