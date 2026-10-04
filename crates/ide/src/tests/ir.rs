//! Production Rules IR integration checks.
use crate::{DiagnosticCode, complete, definition, diagnostics, hover, semantic_tokens};
use engine::{AnalysisHost, DocumentId};
use std::sync::Arc;

fn host() -> AnalysisHost {
    let ir = game::eu4::first_party_ir().expect("embedded IR");
    let catalog = rules::RuleSet::from_ir_catalog(&ir);
    assert_eq!(catalog.rule_hash(), ir.rule_hash());
    assert_eq!(
        catalog.dynamic_definition_context("scripted_effect"),
        Some("effect")
    );
    AnalysisHost::with_ir(catalog, ir.game.profile.clone(), ir)
}

#[test]
fn ir_template_long_chain_is_complete_in_diagnostics_completion_and_hover() {
    let mut host = host();
    let mut definitions = String::new();
    for depth in 0..40 {
        definitions.push_str(&format!(
            "limit_chain{depth} = {{ limit_chain{} = {{ N = $N$ }} }}\n",
            depth + 1
        ));
    }
    definitions
        .push_str("limit_chain40 = { add_prestige = $N$ }\nshort_limit = { add_prestige = $N$ }\n");
    open(
        &mut host,
        "common/scripted_effects/limits.txt",
        &definitions,
    );
    let source = "country_event = { id = limits.1 immediate = { limit_chain0 = { N = wrong } short_limit = { N = wrong } } }";
    let id = open(&mut host, "events/limits.txt", source);
    let results = diagnostics(&host.snapshot(), &id);
    assert!(
        results
            .iter()
            .all(|d| d.code != DiagnosticCode::AnalysisIncomplete),
        "{results:?}"
    );
    assert!(
        results
            .iter()
            .any(|d| d.code == DiagnosticCode::InvalidValue && d.message.contains("limit_chain0")),
        "a known rejection survives incomplete work: {results:?}"
    );
    let position = source.find("N = wrong").unwrap() as u32;
    let items = complete(&host.snapshot(), &id, position + 9);
    assert!(items.coverage.is_complete());
    let hover = hover(&host.snapshot(), &id, position).expect("parameter hover");
    assert!(hover.coverage.is_complete());
    assert!(hover.contents.contains("number"), "{}", hover.contents);
}

#[test]
fn ir_template_node_limit_retains_an_early_rejection() {
    let mut host = host();
    open(
        &mut host,
        "common/scripted_effects/node-limit.txt",
        "large_limit = { set_emperor = $P$ log = $P$ log = $P$ log = $P$ }",
    );
    let snapshot = host.snapshot();
    let sites = crate::ir_template::parameter_sites_with_budget(
        &snapshot,
        "scripted_effect",
        "large_limit",
        "P",
        &std::collections::BTreeMap::from([("P".into(), "wrong".into())]),
        hir::ScopeState::initial(hir::ScopeValue::known_single("country")),
        2,
        &crate::CancellationToken::new(),
    )
    .unwrap();
    assert!(!sites.coverage.is_complete());
    assert!(
        sites
            .iter()
            .any(|site| !site.accepts(&snapshot, "P", "wrong")),
        "known rejection survives: {sites:?}"
    );
}

#[test]
fn ir_template_key_completion_checks_the_selected_body_scope() {
    let mut host = host();
    open(
        &mut host,
        "common/scripted_effects/body-scope.txt",
        "body_scope = { $TARGET$ = { add_base_tax = 1 } }",
    );
    let source =
        "country_event = { id = structure.1 immediate = { body_scope = { TARGET = cap } } }";
    let id = open(&mut host, "events/body-scope.txt", source);
    let position = source.find("TARGET = cap").unwrap() as u32 + 12;
    let items = complete(&host.snapshot(), &id, position).items;
    assert!(
        items.iter().any(|item| item.label == "capital"),
        "{items:?}"
    );
    assert!(
        !items
            .iter()
            .any(|item| item.label == "capital.owner" || item.label == "capital.controller"),
        "country-target branches reject province effects: {items:?}"
    );
}

#[test]
fn ir_template_splice_validates_the_whole_parent_container() {
    let mut host = host();
    open(
        &mut host,
        "common/scripted_effects/parent-container.txt",
        "parent_container = { if = { limit = { always = yes } $BODY$ } }",
    );
    let source = "country_event = { id = structure.2 immediate = { parent_container = { BODY = \"limit = { always = yes } add_prestige = 1\" } } }";
    let id = open(&mut host, "events/parent-container.txt", source);
    let issues = diagnostics(&host.snapshot(), &id);
    assert!(
        issues
            .iter()
            .any(|issue| issue.code == DiagnosticCode::Cardinality
                && issue.message.contains("Template `parent_container`")),
        "fixed and inserted limit count together: {issues:?}"
    );
}

#[test]
fn ir_disk_diagnostics_release_temporary_frontends_and_reuse_interactive_ones() {
    use engine::{SourceRoot, SourceRootId, SourceRootKind, WorkspaceChange};
    use text::{AbsPath, LogicalPath};
    let root = super::support::temp_root("ir-diagnostic-frontends");
    std::fs::create_dir_all(root.join("events")).unwrap();
    std::fs::write(
        root.join("events/frontends.txt"),
        "country_event = { id = cache.1 immediate = { add_prestige = 1 } }",
    )
    .unwrap();
    let mut host = host();
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(1),
        SourceRootKind::Project,
        AbsPath::normalize(&root),
    )]));
    host.refresh_source_roots().unwrap();
    let snapshot = host.snapshot();
    let path = LogicalPath::parse("events/frontends.txt").unwrap();
    let file = snapshot
        .source_files()
        .values()
        .find(|file| file.logical_path == path)
        .unwrap()
        .id;
    let key = format!("ir-hir:file:{}", file.get());
    let input = crate::support::diagnostic_input_for_source_file(&snapshot, file).unwrap();
    let temporary = Arc::downgrade(input.hir.as_ref().unwrap());
    drop(input);
    assert!(
        temporary.upgrade().is_none(),
        "temporary HIR is released after its query"
    );
    let cancellation = crate::CancellationToken::new();
    let transient =
        crate::source_file_diagnostics_with_cancellation(&snapshot, file, &cancellation).unwrap();
    assert!(
        snapshot
            .query_cache()
            .get::<hir::HirFile>(snapshot.revision(), &key)
            .is_none()
    );

    let interactive = crate::support::input_for_source_file(&snapshot, file).unwrap();
    let retained = Arc::downgrade(interactive.hir.as_ref().unwrap());
    let reused = crate::support::diagnostic_input_for_source_file(&snapshot, file).unwrap();
    assert!(Arc::ptr_eq(
        interactive.hir.as_ref().unwrap(),
        reused.hir.as_ref().unwrap()
    ));
    drop(reused);
    drop(interactive);
    let cached =
        crate::source_file_diagnostics_with_cancellation(&snapshot, file, &cancellation).unwrap();
    assert_eq!(cached, transient);
    assert!(
        retained.upgrade().is_some(),
        "interactive frontend stays cached"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn ir_file_instances_name_configuration_symbols_from_filename() {
    let mut host = host();
    for (path, kind, name) in [
        (
            "common/units_display/phase5_display.txt",
            "units_display",
            "phase5_display",
        ),
        ("map/climate.txt", "mapclimate", "climate"),
    ] {
        let id = open(&mut host, path, "");
        let snapshot = host.snapshot();
        let definitions = snapshot.document(&id).unwrap().hir().unwrap().definitions();
        assert!(
            definitions
                .iter()
                .any(|definition| definition.kind.as_ref() == kind && definition.name == name),
            "{path}: {definitions:?}"
        );
    }
}

#[test]
fn ir_counter_descriptions_and_federation_tooltips_navigate_to_localisation() {
    let mut host = host();
    let localisation = open(
        &mut host,
        "localisation/phase5_tooltips_l_english.yml",
        "l_english:\n phase5_counter_desc:0 \"Counter\"\n phase5_federation_tooltip:0 \"Federation\"\n",
    );
    for (path, source, name) in [
        (
            "events/counter-description.txt",
            "country_event = { id = phase5.counter trigger = { calc_true_if = { amount = 1 desc = phase5_counter_desc always = yes } } }",
            "phase5_counter_desc",
        ),
        (
            "common/federation_advancements/phase5_tooltip.txt",
            "phase5_advancement = { effect = { custom_tooltip = phase5_federation_tooltip } }",
            "phase5_federation_tooltip",
        ),
    ] {
        let id = open(&mut host, path, source);
        let targets = definition(&host.snapshot(), &id, source.find(name).unwrap() as u32 + 1);
        assert_eq!(targets.len(), 1, "{path}: {targets:?}");
        assert_eq!(targets[0].document.as_ref(), Some(&localisation));
        assert!(diagnostics(&host.snapshot(), &id).iter().all(|d| !matches!(
            d.code,
            DiagnosticCode::InvalidValue | DiagnosticCode::UnknownKey
        )));
    }
}

#[test]
fn ir_empty_custom_tooltips_are_valid_separators_without_relaxing_other_loc_fields() {
    let mut host = host();
    open(
        &mut host,
        "localisation/tooltip-separator_l_english.yml",
        "l_english:\n phase5.first:0 \"First\"\n phase5.second:0 \"Second\"\n",
    );
    let id = open(
        &mut host,
        "events/tooltip-separator.txt",
        "country_event = { id = phase5.separator immediate = { \
         custom_tooltip = phase5.first custom_tooltip = \"\" custom_tooltip = \" \" custom_tooltip = phase5.second } \
         option = { name = phase5.first } }",
    );
    let items = diagnostics(&host.snapshot(), &id);
    assert!(items.is_empty(), "{items:#?}");
    assert!(
        host.snapshot()
            .document(&id)
            .unwrap()
            .hir()
            .unwrap()
            .references()
            .iter()
            .filter(|reference| reference.kind.as_ref() == "localisation")
            .all(|reference| !reference.name.trim().is_empty())
    );
    let invalid = open(
        &mut host,
        "events/tooltip-separator-invalid.txt",
        "country_event = { id = phase5.separator.invalid trigger = { \
         calc_true_if = { amount = 1 desc = \"\" always = yes } } \
         immediate = { custom_tooltip = { text = phase5.first } } \
         option = { name = phase5.first } }",
    );
    let items = diagnostics(&host.snapshot(), &invalid);
    assert!(
        items
            .iter()
            .any(|item| item.code == DiagnosticCode::InvalidValue && item.message.contains("desc")),
        "{items:#?}"
    );
    assert!(
        items
            .iter()
            .any(|item| item.code == DiagnosticCode::InvalidValue
                && item.message.contains("custom_tooltip")),
        "{items:#?}"
    );
}

#[test]
fn ir_all_localisation_bindings_are_visible_without_name_or_field_gates() {
    let mut host = host();
    let personalities = open(
        &mut host,
        "common/ai_personalities/phase5_bindings.txt",
        "ai_phase5 = { chance = { factor = 1 } icon = 2 } human = { chance = { factor = 0 } icon = 1 }",
    );
    let reform = open(
        &mut host,
        "common/imperial_reforms/phase5_bindings.txt",
        "phase5_reform = { empire = hre emperor = { } member = { } }",
    );
    let snapshot = host.snapshot();
    let names = |id: &DocumentId| {
        snapshot
            .document(id)
            .unwrap()
            .hir()
            .unwrap()
            .binding_references_for_hover()
            .iter()
            .filter(|r| r.kind.as_ref() == "localisation")
            .map(|r| r.name.to_string())
            .collect::<Vec<_>>()
    };
    let personality_names = names(&personalities);
    for name in [
        "ai_phase5",
        "ai_phase5_desc",
        "an_ai_phase5",
        "human",
        "human_desc",
        "an_human",
    ] {
        assert_eq!(
            personality_names.iter().filter(|n| *n == name).count(),
            1,
            "{personality_names:?}"
        );
    }
    let reform_names = names(&reform);
    for name in [
        "phase5_reform_emperor",
        "phase5_reform_member",
        "phase5_reform_elector",
        "phase5_reform_province",
    ] {
        assert_eq!(
            reform_names.iter().filter(|n| *n == name).count(),
            1,
            "{reform_names:?}"
        );
    }
}

#[test]
fn ir_hover_preserves_every_binding_label_for_identical_localisation_text() {
    let mut host = host();
    open(
        &mut host,
        "localisation/all-bindings.yml",
        "l_english:\nregion_one:0 \"Region One\"\nshort_region_one:0 \"Region One\"\n",
    );
    let id = open(
        &mut host,
        "common/colonial_regions/all-bindings.txt",
        "region_one = { }",
    );
    let result = hover(&host.snapshot(), &id, 1).expect("region hover");
    for label in ["name", "short"] {
        assert!(
            result
                .contents
                .contains(&format!("| {label} | Region One |")),
            "{}",
            result.contents
        );
    }
    assert!(
        !diagnostics(&host.snapshot(), &id)
            .iter()
            .any(|d| d.code == DiagnosticCode::UnknownLocalisationKey)
    );
}

#[test]
fn ir_known_scope_overloads_keep_register_and_target_value_alternatives() {
    let mut host = host();
    let source = "country_event = { id = phase5.religion immediate = { save_event_target_as = phase5_religion_origin every_owned_province = { change_religion = ROOT change_religion = event_target:phase5_religion_origin } } }";
    let id = open(&mut host, "events/scoped-religion-values.txt", source);
    let snapshot = host.snapshot();
    assert!(diagnostics(&snapshot, &id).iter().all(|d| !matches!(
        d.code,
        DiagnosticCode::InvalidValue | DiagnosticCode::WrongScope
    )));
    assert!(
        snapshot
            .document(&id)
            .unwrap()
            .hir()
            .unwrap()
            .references()
            .iter()
            .all(|r| r.kind.as_ref() != "country_tag"
                || (r.name != "ROOT" && !r.name.starts_with("event_target:")))
    );
    let invalid = open(
        &mut host,
        "events/scoped-religion-invalid.txt",
        "country_event = { id = phase5.bad_religion immediate = { every_owned_province = { change_religion = phase5_unknown_religion } } }",
    );
    assert!(
        diagnostics(&host.snapshot(), &invalid)
            .iter()
            .any(|d| d.code == DiagnosticCode::InvalidValue)
    );
}

#[test]
fn ir_province_name_lists_and_scalars_share_the_province_key_domain() {
    let mut host = host();
    open(
        &mut host,
        "map/positions.txt",
        "1177 = { position = { 0 0 } } 1178 = { position = { 1 1 } }",
    );
    let id = open(
        &mut host,
        "common/province_names/lists.txt",
        "1177 = { \"First name\" \"Second name\" } 1178 = \"Single name\"",
    );
    let items = diagnostics(&host.snapshot(), &id);
    assert!(
        !items.iter().any(|d| matches!(
            d.code,
            DiagnosticCode::InvalidValue | DiagnosticCode::UnknownKey
        )),
        "{items:#?}"
    );
    let id = open(
        &mut host,
        "common/province_names/lists-invalid.txt",
        "1177 = { nested = { } }",
    );
    assert!(
        diagnostics(&host.snapshot(), &id)
            .iter()
            .any(|d| d.code == DiagnosticCode::UnknownKey && d.message.contains("nested"))
    );
}

#[test]
fn ir_trade_node_iterator_and_exported_identity_values_follow_confirmed_rules() {
    let mut host = host();
    let valid = open(
        &mut host,
        "events/exported-identities.txt",
        "country_event = { id = phase5.identities trigger = { religion = variable:new_ruler_religion religion_group = variable:new_ruler_religion culture = variable:FROM:new_ruler_culture primary_culture = variable:From:new_ruler_culture accepted_culture = variable:advisor_culture culture_group = variable:advisor_culture } immediate = { change_religion = variable:new_ruler_religion set_ruler_religion = variable:new_ruler_religion set_heir_culture = variable:FROM:new_ruler_culture define_ruler = { religion = variable:new_ruler_religion culture = variable:new_ruler_culture } define_consort = { religion = variable:FROM:new_ruler_religion culture = variable:FROM:new_ruler_culture } every_owned_province = { every_trade_node_member_country = { set_country_flag = phase5_trade_member } } } option = { name = phase5.identities } }",
    );
    let items = diagnostics(&host.snapshot(), &valid);
    assert!(
        !items.iter().any(|d| matches!(
            d.code,
            DiagnosticCode::UnknownKey
                | DiagnosticCode::InvalidValue
                | DiagnosticCode::WrongScope
                | DiagnosticCode::Cardinality
        )),
        "{items:?}"
    );
    let invalid = open(
        &mut host,
        "events/exported-identities-invalid.txt",
        "country_event = { id = phase5.invalid trigger = { culture = phase5_unknown_culture } immediate = { every_trade_node_member_country = { add_treasury = 1 } } }",
    );
    let items = diagnostics(&host.snapshot(), &invalid);
    assert!(
        items.iter().any(|d| d.code == DiagnosticCode::InvalidValue
            && d.message.contains("phase5_unknown_culture")),
        "{items:?}"
    );
    assert!(
        items.iter().any(|d| d.code == DiagnosticCode::WrongScope
            && d.message.contains("every_trade_node_member_country")),
        "{items:?}"
    );
}

#[test]
fn ir_forward_subject_declarations_and_repeated_history_effects_are_valid() {
    let mut host = host();
    let subject = open(
        &mut host,
        "common/subject_types/forward.txt",
        "phase5_subject = { } phase5_subject = { can_fight_independence_war = yes }",
    );
    let country = open(
        &mut host,
        "history/countries/F00 - Forward.txt",
        "if = { limit = { always = yes } set_country_flag = first } if = { limit = { always = yes } set_country_flag = second } set_country_flag = third set_country_flag = fourth",
    );
    let values = diagnostics(&host.snapshot(), &subject);
    assert!(
        !values.iter().any(|d| matches!(
            d.code,
            DiagnosticCode::Cardinality | DiagnosticCode::UnknownKey | DiagnosticCode::InvalidValue
        )),
        "{values:?}"
    );
    let values = diagnostics(&host.snapshot(), &country);
    assert!(
        !values.iter().any(|d| matches!(
            d.code,
            DiagnosticCode::Cardinality | DiagnosticCode::UnknownKey | DiagnosticCode::InvalidValue
        )),
        "{values:?}"
    );
    let invalid = open(
        &mut host,
        "history/countries/F01 - Invalid.txt",
        "primary_culture = first primary_culture = second",
    );
    assert!(diagnostics(&host.snapshot(), &invalid).iter().any(|d| d.code == DiagnosticCode::Cardinality && d.message.contains("primary_culture")));
}

#[test]
fn ir_vanilla_scaled_rebel_interface_supports_saved_and_explicit_leader_names() {
    let mut host = host();
    open(
        &mut host,
        "common/scripted_effects/rebels.txt",
        "spawn_large_scaled_rebels = { spawn_rebels = { type = $type$ size = 3 [[saved_name] leader = $saved_name$ ] [[leader] leader = $leader$ leader_dynasty = $leader_dynasty$ ] } }",
    );
    open(
        &mut host,
        "common/rebel_types/phase5.txt",
        "phase5_rebels = { }",
    );
    let id = open(
        &mut host,
        "events/rebel-names.txt",
        "country_event = { id = phase5.rebels immediate = { every_owned_province = { spawn_large_scaled_rebels = { type = phase5_rebels saved_name = phase5_saved } spawn_large_scaled_rebels = { type = phase5_rebels leader = phase5_leader leader_dynasty = phase5_dynasty } } } option = { name = phase5.rebels } }",
    );
    let values = diagnostics(&host.snapshot(), &id);
    assert!(
        !values.iter().any(|d| matches!(
            d.code,
            DiagnosticCode::UnknownKey
                | DiagnosticCode::InvalidValue
                | DiagnosticCode::Cardinality
                | DiagnosticCode::WrongScope
        )),
        "{values:?}"
    );
    let id = open(
        &mut host,
        "events/rebel-names-invalid.txt",
        "country_event = { id = phase5.bad_rebels immediate = { every_owned_province = { spawn_large_scaled_rebels = { unsupported_argument = yes } } } }",
    );
    assert!(diagnostics(&host.snapshot(), &id).iter().any(|d| d.code
        == DiagnosticCode::UnknownKey
        && d.message.contains("unsupported_argument")));
}

#[test]
fn ir_runtime_advisor_ids_and_trade_node_province_values_are_typed() {
    let mut host = host();
    open(
        &mut host,
        "map/positions.txt",
        "1177 = { position = { 0 0 } }",
    );
    let id = open(
        &mut host,
        "events/runtime-ids.txt",
        "country_event = { id = phase5.ids trigger = { advisor_exists = 1097 is_advisor_employed = 562 any_owned_province = { same_trade_node_as = 1177 } } }",
    );
    let values = diagnostics(&host.snapshot(), &id);
    assert!(
        !values.iter().any(|d| matches!(
            d.code,
            DiagnosticCode::InvalidValue | DiagnosticCode::WrongScope
        )),
        "{values:?}"
    );
    let id = open(
        &mut host,
        "events/runtime-ids-invalid.txt",
        "country_event = { id = phase5.ids_invalid trigger = { advisor_exists = wrong is_advisor_employed = -1 any_owned_province = { same_trade_node_as = ROOT } } }",
    );
    let values = diagnostics(&host.snapshot(), &id);
    assert_eq!(
        values
            .iter()
            .filter(|d| d.code == DiagnosticCode::InvalidValue)
            .count(),
        2,
        "{values:?}"
    );
    assert!(
        values
            .iter()
            .any(|d| d.code == DiagnosticCode::WrongScope && d.message.contains("ROOT")),
        "{values:?}"
    );
}

#[test]
fn ir_canonical_ancestor_and_sprite_pack_types_keep_localisation_bindings() {
    let mut host = host();
    open(
        &mut host,
        "localisation/aliases_l_english.yml",
        "l_english:\n ancestor_sage_personality:0 \"Sage\"\n \
         desc_ancestor_sage_personality:0 \"Sage description\"\n \
         ancestor_missing_personality:0 \"Missing description\"\n phase5_pack:0 \"Pack\"\n",
    );
    let ancestor = open(
        &mut host,
        "common/ancestor_personalities/phase5.txt",
        "ancestor_sage_personality = { }",
    );
    let pack = open(
        &mut host,
        "gfx/sprite_packs/phase5.txt",
        "phase5_pack = { }",
    );
    let missing = open(
        &mut host,
        "common/ancestor_personalities/phase5_missing.txt",
        "ancestor_missing_personality = { }",
    );
    let snapshot = host.snapshot();
    for (id, key) in [
        (&ancestor, "ancestor_sage_personality"),
        (&pack, "phase5_pack"),
    ] {
        let hir = snapshot.document(id).unwrap().hir().unwrap();
        assert!(
            hir.binding_references_for_hover()
                .iter()
                .any(|r| r.kind.as_ref() == "localisation" && r.name == key),
            "{key}: {:?}",
            hir.binding_references_for_hover()
        );
        assert!(
            !diagnostics(&snapshot, id)
                .iter()
                .any(|d| d.code == DiagnosticCode::InvalidValue && d.message.contains(key))
        );
    }
    let ir = snapshot.ir();
    let ancestor_type = ir.type_info(ir.type_by_name("ancestor_personality").unwrap());
    assert!(
        ancestor_type
            .trait_impls
            .iter()
            .any(|t| Some(t.trait_id) == ir.trait_by_name("ModifierSource"))
    );
    let hir = snapshot.document(&ancestor).unwrap().hir().unwrap();
    assert!(hir.binding_references_for_hover().iter().any(|reference| {
        reference.kind.as_ref() == "localisation"
            && reference.name == "desc_ancestor_sage_personality"
    }));
    assert!(!hir.references().iter().any(|reference| {
        reference.kind.as_ref() == "localisation"
            && reference.name == "desc_ancestor_sage_personality"
    }));
    let result = hover(&snapshot, &ancestor, 10).expect("ancestor hover");
    assert!(
        result.contents.contains("| desc | Sage description |"),
        "{}",
        result.contents
    );
    assert!(diagnostics(&snapshot, &missing).is_empty());
}

#[test]
fn ir_rebel_demand_bindings_require_both_vanilla_localisation_keys() {
    let mut host = host();
    let loc = open(
        &mut host,
        "localisation/rebel_demands_l_english.yml",
        "l_english:\n audit_demand:0 \"Demand title\"\n audit_demand_desc:0 \"Demand description\"\n \
         missing_desc:0 \"Title only\"\n missing_name_desc:0 \"Description only\"\n",
    );
    let source = "audit_rebels = { demands_description = audit_demand }";
    let rebel = open(&mut host, "common/rebel_types/audit.txt", source);
    let snapshot = host.snapshot();
    let hir = snapshot.document(&rebel).unwrap().hir().unwrap();
    assert!(hir.definitions().iter().any(|definition| {
        definition.kind.as_ref() == "rebel_demand_loc" && definition.name == "audit_demand"
    }));
    for name in ["audit_demand", "audit_demand_desc"] {
        assert_eq!(
            hir.references()
                .iter()
                .filter(
                    |reference| reference.kind.as_ref() == "localisation" && reference.name == name
                )
                .count(),
            1,
            "{name}: {:?}",
            hir.references()
        );
    }
    let offset = source.rfind("audit_demand").unwrap() as u32 + 1;
    let targets = definition(&snapshot, &rebel, offset);
    assert_eq!(targets.len(), 1, "{targets:?}");
    assert!(
        targets
            .iter()
            .all(|target| target.document.as_ref() == Some(&loc))
    );
    let result = hover(&snapshot, &rebel, offset).expect("demand hover");
    for text in ["Demand title", "Demand description"] {
        assert!(result.contents.contains(text), "{}", result.contents);
    }
    for (key, missing) in [
        ("missing_desc", "missing_desc_desc"),
        ("missing_name", "missing_name"),
        ("", "_desc"),
    ] {
        let id = open(
            &mut host,
            &format!("common/rebel_types/missing_{key}.txt"),
            &format!("audit_rebels = {{ demands_description = \"{key}\" }}"),
        );
        let items = diagnostics(&host.snapshot(), &id);
        assert!(
            items.iter().any(|item| item.message.contains(missing)),
            "{missing}: {items:#?}"
        );
    }
}

#[test]
fn ir_papal_policy_bindings_exclude_structural_papacy_fields() {
    let mut host = host();
    open(
        &mut host,
        "localisation/papal_actions_l_english.yml",
        "l_english:\n bless_monarch:0 \"Bless the ruler\"\n",
    );
    let source = "audit_group = { audit_religion = { papacy = { \
                  bless_monarch = { } local_saint = { } harsh = { } \
                  neutral = { } concilatory = { } concessions = { } } } }";
    let id = open(&mut host, "common/religions/papal_actions.txt", source);
    let snapshot = host.snapshot();
    let hir = snapshot.document(&id).unwrap().hir().unwrap();
    let actions = hir
        .definitions()
        .iter()
        .filter(|definition| definition.kind.as_ref() == "papal_policy")
        .map(|definition| definition.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(actions, ["bless_monarch", "local_saint"]);
    let items = diagnostics(&snapshot, &id);
    assert!(
        items
            .iter()
            .any(|item| item.message.contains("local_saint")),
        "{items:#?}"
    );
    assert!(
        !items
            .iter()
            .any(|item| item.message.contains("bless_monarch")),
        "{items:#?}"
    );
    let offset = source.find("bless_monarch").unwrap() as u32 + 1;
    let result = hover(&snapshot, &id, offset).expect("papal action hover");
    assert!(
        result.contents.contains("Bless the ruler"),
        "{}",
        result.contents
    );
}

#[test]
fn ir_list_references_exclude_nested_field_values() {
    let mut host = host();
    let province = open(
        &mut host,
        "map/positions.txt",
        "144 = { position = { 0 0 } }",
    );
    let source = "audit_area = { color = { 144 0 32 } 144 9999 }";
    let id = open(&mut host, "map/area.txt", source);
    let snapshot = host.snapshot();
    let hir = snapshot.document(&id).unwrap().hir().unwrap();
    let references = hir
        .references()
        .iter()
        .filter(|reference| reference.kind.as_ref() == "province_id")
        .collect::<Vec<_>>();
    assert_eq!(references.len(), 2, "{references:#?}");
    assert_eq!(references[0].name, "144");
    assert_eq!(references[1].name, "9999");
    assert!(definition(&snapshot, &id, source.find("144").unwrap() as u32 + 1).is_empty());
    let targets = definition(&snapshot, &id, source.rfind("144").unwrap() as u32 + 1);
    assert_eq!(targets.len(), 1, "{targets:#?}");
    assert_eq!(targets[0].document.as_ref(), Some(&province));
    let errors = diagnostics(&snapshot, &id)
        .into_iter()
        .filter(|diagnostic| diagnostic.severity == crate::Severity::Error)
        .collect::<Vec<_>>();
    assert_eq!(errors.len(), 1, "{errors:#?}");
    assert!(errors[0].message.contains("9999"), "{errors:#?}");

    host.close_document(&id).unwrap();
    let invalid = open(
        &mut host,
        "map/area.txt",
        "audit_area = { color = { wrong 0 32 } 144 }",
    );
    let errors = diagnostics(&host.snapshot(), &invalid);
    assert!(
        errors.iter().any(|diagnostic| {
            diagnostic.code == DiagnosticCode::InvalidValue && diagnostic.message.contains("wrong")
        }),
        "{errors:#?}"
    );
    assert!(
        errors
            .iter()
            .all(|diagnostic| !diagnostic.message.contains("unknown province_id"))
    );
}

#[test]
fn ir_scope_union_values_keep_real_symbol_references_and_navigation() {
    let mut host = host();
    open(
        &mut host,
        "common/religions/phase5.txt",
        "phase5_group = { phase5_religion = { } }",
    );
    open(
        &mut host,
        "common/government_reforms/phase5.txt",
        "phase5_reform = { }",
    );
    open(
        &mut host,
        "common/governments/phase5.txt",
        "phase5_government = { basic_reform = phase5_reform }",
    );
    open(&mut host, "map/continent.txt", "phase5_continent = { 40 }");
    open(&mut host, "map/area.txt", "phase5_area = { 40 }");
    open(
        &mut host,
        "map/region.txt",
        "phase5_region = { areas = { phase5_area } }",
    );
    open(
        &mut host,
        "common/colonial_regions/phase5.txt",
        "phase5_colony = { provinces = { 40 } }",
    );
    let source = "country_event = { id = phase5.references trigger = { government = phase5_government religion = phase5_religion any_owned_province = { continent = phase5_continent region = phase5_region colonial_region = phase5_colony owned_by = ROOT } } }";
    let id = open(&mut host, "events/scope-union-references.txt", source);
    let snapshot = host.snapshot();
    let hir = snapshot.document(&id).unwrap().hir().unwrap();
    for (kind, name) in [
        ("government", "phase5_government"),
        ("religion", "phase5_religion"),
        ("continent", "phase5_continent"),
        ("region", "phase5_region"),
        ("colonial_region", "phase5_colony"),
    ] {
        assert!(
            hir.references()
                .iter()
                .any(|r| r.kind.as_ref() == kind && r.name == name),
            "{kind}: {:?}",
            hir.references()
        );
        assert_eq!(
            definition(&snapshot, &id, source.find(name).unwrap() as u32 + 1).len(),
            1,
            "{kind}"
        );
    }
    assert!(
        !hir.references().iter().any(|r| r.name == "ROOT"),
        "{:?}",
        hir.references()
    );
}

#[test]
fn ir_scalar_fallbacks_and_unknown_scope_overloads_do_not_create_unresolved_references() {
    let mut host = host();
    open(
        &mut host,
        "common/country_tags/fallback.txt",
        "FRA = \"countries/France.txt\"",
    );
    open(
        &mut host,
        "common/religions/fallback.txt",
        "phase5_group = { phase5_religion = { } }",
    );
    open(
        &mut host,
        "map/positions.txt",
        "40 = { position = { 0 0 } }",
    );
    let source = "country_event = { id = phase5.fallback trigger = { dynasty = \"von Habsburg\" dynasty = FRA } }";
    let id = open(&mut host, "events/scalar-fallback.txt", source);
    let snapshot = host.snapshot();
    assert!(
        !diagnostics(&snapshot, &id)
            .iter()
            .any(|d| d.code == DiagnosticCode::InvalidValue)
    );
    let hir = snapshot.document(&id).unwrap().hir().unwrap();
    assert!(!hir.references().iter().any(|r| r.name == "von Habsburg"));
    assert_eq!(
        definition(&snapshot, &id, source.find("FRA").unwrap() as u32 + 1).len(),
        1
    );

    let source = "helper = { change_religion = phase5_religion add_core = 40 }";
    let id = open(
        &mut host,
        "common/scripted_effects/scalar-overloads.txt",
        source,
    );
    let snapshot = host.snapshot();
    let items = diagnostics(&snapshot, &id);
    assert!(
        !items.iter().any(|d| d.code == DiagnosticCode::InvalidValue),
        "{items:?}"
    );
    let hir = snapshot.document(&id).unwrap().hir().unwrap();
    for (name, kind) in [("phase5_religion", "religion"), ("40", "province_id")] {
        let refs = hir
            .references()
            .iter()
            .filter(|r| r.name == name)
            .collect::<Vec<_>>();
        assert_eq!(refs.len(), 1, "{refs:?}");
        assert_eq!(refs[0].kind.as_ref(), kind);
        assert_eq!(
            definition(&snapshot, &id, source.find(name).unwrap() as u32 + 1).len(),
            1
        );
    }
    let id = open(
        &mut host,
        "events/no-fallback.txt",
        "country_event = { id = phase5.invalid trigger = { tag = phase5_unknown_country } }",
    );
    assert!(
        diagnostics(&host.snapshot(), &id)
            .iter()
            .any(|d| d.code == DiagnosticCode::InvalidValue)
    );
}

#[test]
fn ir_resolved_overlapping_reference_branches_retain_navigation() {
    let mut host = host();
    open(
        &mut host,
        "common/government_reforms/overlap.txt",
        "phase5_basic = { basic_reform = yes } constitutional_republic = { legacy_government = yes } phase5_new = { legacy_government = no }",
    );
    open(
        &mut host,
        "map/ambient_object.txt",
        "type = { type = eagle use_animation = yes object = { name = phase5_monument hidden_on_start = no position = { 1 2 3 } rotation = { 0 0 0 } } }",
    );
    let project_source = "phase5_monument = { type = monument on_built = { show_ambient_object = phase5_monument } on_destroyed = { hide_ambient_object = phase5_monument } }";
    let project = open(
        &mut host,
        "common/great_projects/overlap.txt",
        project_source,
    );
    let source = "country_event = { id = phase5.overlap trigger = { has_reform = phase5_basic has_reform = constitutional_republic has_reform = phase5_new } immediate = { add_government_reform = constitutional_republic } }";
    let event = open(&mut host, "events/reference-overlap.txt", source);
    let snapshot = host.snapshot();
    assert_eq!(
        definition(
            &snapshot,
            &event,
            source.find("phase5_basic").unwrap() as u32 + 1
        )
        .len(),
        1
    );
    for position in [
        source.find("constitutional_republic").unwrap(),
        source.rfind("constitutional_republic").unwrap(),
        source.find("phase5_new").unwrap(),
    ] {
        assert_eq!(definition(&snapshot, &event, position as u32 + 1).len(), 1);
    }
    assert!(
        !diagnostics(&snapshot, &event)
            .iter()
            .any(|d| d.code == DiagnosticCode::InvalidValue)
    );
    let refs = snapshot
        .document(&event)
        .unwrap()
        .hir()
        .unwrap()
        .references();
    assert_eq!(
        refs.iter()
            .filter(|r| r.kind.as_ref() == "government_reform" && r.name == "phase5_basic")
            .count(),
        1,
        "{refs:?}"
    );
    let refs = snapshot
        .document(&project)
        .unwrap()
        .hir()
        .unwrap()
        .references();
    for kind in ["ambient_object", "great_project"] {
        assert_eq!(
            refs.iter()
                .filter(|r| r.kind.as_ref() == kind && r.name == "phase5_monument")
                .count(),
            2,
            "{refs:?}"
        );
    }
    let position = project_source.rfind("phase5_monument").unwrap() as u32;
    // Navigation selects one namespace at a position; neither resolved branch
    // may disappear from the indexed reference set.
    assert_eq!(definition(&snapshot, &project, position + 1).len(), 1);
}

#[test]
fn ir_scripted_helper_calls_and_scalar_bindings_navigate_to_actual_definitions() {
    let mut host = host();
    let country = open(
        &mut host,
        "common/country_tags/helper.txt",
        "FRA = \"countries/France.txt\"",
    );
    let trigger = open(
        &mut host,
        "common/scripted_triggers/helper.txt",
        "was_never_end_game_tag_trigger = { always = yes } is_or_was_tag = { OR = { tag = $tag$ was_tag = $tag$ } }",
    );
    let effect = open(
        &mut host,
        "common/scripted_effects/helper.txt",
        "restore_country_name_effect = { add_prestige = 1 save_event_target_as = phase5_route } route = { $where$ = { add_prestige = 1 } }",
    );
    let source = "country_event = { id = phase5.helper trigger = { was_never_end_game_tag_trigger = yes is_or_was_tag = { tag = FRA } } immediate = { restore_country_name_effect = yes route = { where = event_target:phase5_route } } }";
    let id = open(&mut host, "events/helper.txt", source);
    let snapshot = host.snapshot();
    for (name, target) in [
        ("was_never_end_game_tag_trigger", &trigger),
        ("is_or_was_tag", &trigger),
        ("restore_country_name_effect", &effect),
        ("FRA", &country),
        ("phase5_route", &effect),
    ] {
        let targets = definition(&snapshot, &id, source.find(name).unwrap() as u32 + 1);
        assert_eq!(targets.len(), 1, "{name}: {targets:?}");
        assert_eq!(targets[0].document.as_ref(), Some(target));
    }
    let references = snapshot.document(&id).unwrap().hir().unwrap().references();
    assert_eq!(
        references
            .iter()
            .filter(|r| r.kind.as_ref() == "country_tag" && r.name == "FRA")
            .count(),
        1,
        "{references:?}"
    );
    assert!(diagnostics(&snapshot, &id).iter().all(|d| !matches!(
        d.code,
        DiagnosticCode::InvalidValue | DiagnosticCode::UnknownKey | DiagnosticCode::WrongScope
    )));
}

#[test]
fn ir_enum_sprite_fallbacks_keep_installed_symbol_navigation() {
    let mut host = host();
    open(
        &mut host,
        "interface/phase5.gfx",
        "spriteTypes = { spriteType = { name = FETISHIST_FIRE_eventPicture } }",
    );
    let source = "country_event = { id = phase5.sprite picture = FETISHIST_FIRE_eventPicture picture = { trigger = { always = yes } picture = FETISHIST_FIRE_eventPicture } option = { name = phase5.sprite } }";
    let id = open(&mut host, "events/enum-sprite-reference.txt", source);
    for position in [
        source.find("FETISHIST_FIRE_eventPicture").unwrap(),
        source.rfind("FETISHIST_FIRE_eventPicture").unwrap(),
    ] {
        assert_eq!(
            definition(&host.snapshot(), &id, position as u32 + 1).len(),
            1
        );
    }
}

#[test]
fn ir_quoted_preview_payload_symbols_match_direct_preview_symbols() {
    let mut host = host();
    open(
        &mut host,
        "common/scripted_effects/preview.txt",
        "preview = { tooltip = { $body$ } $body$ }",
    );
    let source = "country_event = { id = phase5.preview immediate = { tooltip = { set_country_flag = direct_preview } preview = { body = \"set_country_flag = quoted_preview clr_country_flag = quoted_preview\" } } option = { name = phase5.preview } }";
    let id = open(&mut host, "events/preview.txt", source);
    let snapshot = host.snapshot();
    let hir = snapshot.document(&id).unwrap().hir().unwrap();
    assert_eq!(
        hir.definition_attributes()
            .iter()
            .filter(|attrs| attrs.kind.as_ref() == "country_flag" && attrs.name == "quoted_preview")
            .count(),
        1
    );
    for name in ["direct_preview", "quoted_preview"] {
        assert!(
            hir.definitions()
                .iter()
                .any(|d| d.kind.as_ref() == "country_flag" && d.name == name),
            "{:?}",
            hir.definitions()
        );
    }
    let position = source.rfind("quoted_preview").unwrap() as u32;
    let targets = definition(&snapshot, &id, position + 1);
    assert_eq!(targets.len(), 1, "{targets:?}");
    assert_eq!(
        targets[0].range.start(),
        source.find("quoted_preview").unwrap() as u32
    );
}

#[test]
fn ir_development_growth_accepts_country_and_province_scopes() {
    let mut host = host();
    let id = open(
        &mut host,
        "events/growth.txt",
        "country_event = { id = phase5.growth trigger = { grown_by_development = 10 any_owned_province = { grown_by_development = 3 } } }",
    );
    assert!(!diagnostics(&host.snapshot(), &id).iter().any(|d| matches!(
        d.code,
        DiagnosticCode::WrongScope | DiagnosticCode::InvalidValue
    )));
    let id = open(
        &mut host,
        "events/growth-invalid.txt",
        "country_event = { id = phase5.bad_growth trigger = { any_owned_province = { grown_by_development = wrong } } }",
    );
    assert!(
        diagnostics(&host.snapshot(), &id)
            .iter()
            .any(|d| d.code == DiagnosticCode::InvalidValue && d.message.contains("wrong"))
    );
}

#[test]
fn ir_overloaded_core_checks_keep_unknown_target_scope_alternatives() {
    let mut host = host();
    open(
        &mut host,
        "map/positions.txt",
        "40 = { position = { 0 0 } }",
    );
    let id = open(
        &mut host,
        "events/core-overloads.txt",
        "country_event = { id = phase5.core immediate = { 40 = { tooltip = { if = { limit = { is_core = ROOT is_claim = ROOT } remove_core = ROOT } } } random_owned_province = { save_event_target_as = phase5_province } event_target:phase5_province = { if = { limit = { is_core = ROOT is_claim = ROOT } add_core = ROOT } } } option = { name = phase5.core } }",
    );
    let values = diagnostics(&host.snapshot(), &id);
    assert!(
        !values.iter().any(|d| matches!(
            d.code,
            DiagnosticCode::WrongScope | DiagnosticCode::InvalidValue
        )),
        "{values:#?}"
    );
    let invalid = open(
        &mut host,
        "events/core-overloads-invalid.txt",
        "country_event = { id = phase5.bad_core trigger = { is_core = ROOT is_claim = ROOT } }",
    );
    assert_eq!(
        diagnostics(&host.snapshot(), &invalid)
            .iter()
            .filter(|d| d.code == DiagnosticCode::WrongScope)
            .count(),
        2
    );
}

#[test]
fn ir_legacy_government_mappings_and_repeated_flag_textures_keep_typed_values() {
    let mut host = host();
    open(&mut host, "common/governments/phase5.txt", "tribal = { }");
    let mapping = open(
        &mut host,
        "common/governments/mapping.txt",
        "pre_dharma_mapping = { phase5_obsolete = { government = tribal legacy_government = tribal_federation_legacy } }",
    );
    let values = diagnostics(&host.snapshot(), &mapping);
    assert!(
        !values.iter().any(|d| matches!(
            d.code,
            DiagnosticCode::UnknownKey | DiagnosticCode::InvalidValue
        )),
        "{values:?}"
    );
    let invalid = open(
        &mut host,
        "common/governments/bad-mapping.txt",
        "pre_dharma_mapping = { phase5_obsolete = { government = unknown_government legacy_government = unknown_legacy } }",
    );
    assert_eq!(
        diagnostics(&host.snapshot(), &invalid)
            .iter()
            .filter(|d| d.code == DiagnosticCode::InvalidValue)
            .count(),
        2
    );
    let texture = open(
        &mut host,
        "common/custom_country_colors/00_custom_country_colors.txt",
        "textures = { texture = { size = { x = 10 y = 4 } noOfFrames = 34 color = 2 } texture = { size = { x = 2 y = 2 } noOfFrames = 4 color = 1 } }",
    );
    assert!(
        !diagnostics(&host.snapshot(), &texture)
            .iter()
            .any(|d| d.code == DiagnosticCode::Cardinality && d.message.contains("texture"))
    );
    open(
        &mut host,
        "interface/professionalism.gfx",
        "spriteTypes = { spriteType = { name = GFX_phase5_professionalism } }",
    );
    let modifier = open(
        &mut host,
        "common/professionalism/phase5.txt",
        "phase5_professionalism = { army_professionalism = 0.5 marker_sprite = GFX_phase5_professionalism unit_sprite_start = 0 trigger = { always = yes } reserves_organisation = 0.5 }",
    );
    assert!(
        !diagnostics(&host.snapshot(), &modifier)
            .iter()
            .any(|d| d.code == DiagnosticCode::InvalidValue)
    );
    let invalid = open(
        &mut host,
        "common/professionalism/phase5-bad.txt",
        "phase5_professionalism_bad = { reserves_organisation = wrong }",
    );
    assert!(
        diagnostics(&host.snapshot(), &invalid)
            .iter()
            .any(|d| d.code == DiagnosticCode::InvalidValue && d.message.contains("wrong"))
    );
}

#[test]
fn ir_age_technology_and_static_diplomacy_containers_validate_their_entries() {
    let mut host = host();
    open(
        &mut host,
        "interface/ages.gfx",
        "spriteTypes = { spriteType = { name = GFX_phase5_age } }",
    );
    open(
        &mut host,
        "localisation/containers_l_english.yml",
        "l_english:\n phase5_action:0 \"Action\"\n",
    );
    let age = open(
        &mut host,
        "common/ages/phase5.txt",
        "phase5_age = { start = 1400 can_start = { always = yes } objectives = { phase5_objective = { allow = { always = yes } total_development = 100 } } abilities = { phase5_ability = { ai_will_do = { factor = 1 } } } }",
    );
    let technology = open(
        &mut host,
        "common/technologies/phase5.txt",
        "monarch_power = ADM ahead_of_time = { production_efficiency = 0.2 monthly_russian_modernization = 0.05 ahead_of_time_benefit_adm = 1 } technology = { year = 1400 }",
    );
    let tables = open(
        &mut host,
        "common/technology.txt",
        "tables = { adm_tech = \"technologies/phase5.txt\" }",
    );
    let actions = open(
        &mut host,
        "common/new_diplomatic_actions/phase5.txt",
        "static_actions = { phase5_static = { alert_index = 1 alert_tooltip = phase5_action } }",
    );
    for id in [&age, &technology, &tables, &actions] {
        let values = diagnostics(&host.snapshot(), id);
        assert!(
            !values.iter().any(|d| matches!(
                d.code,
                DiagnosticCode::UnknownKey
                    | DiagnosticCode::InvalidValue
                    | DiagnosticCode::WrongScope
                    | DiagnosticCode::Cardinality
            )),
            "{id:?}: {values:?}"
        );
    }
    let snapshot = host.snapshot();
    let definitions = snapshot
        .document(&actions)
        .unwrap()
        .hir()
        .unwrap()
        .definitions();
    assert!(
        definitions
            .iter()
            .any(|d| d.kind.as_ref() == "diplomatic_action" && d.name == "phase5_static")
    );
    assert!(!definitions.iter().any(|d| d.name == "static_actions"));
    let invalid = open(
        &mut host,
        "common/technologies/phase5-invalid.txt",
        "monarch_power = WRONG ahead_of_time = { production_efficiency = wrong unknown_modifier = 1 } technology = { year = wrong }",
    );
    let values = diagnostics(&host.snapshot(), &invalid);
    assert!(
        values
            .iter()
            .filter(|d| d.code == DiagnosticCode::InvalidValue)
            .count()
            >= 3,
        "{values:?}"
    );
    assert!(values.iter().any(|d| d.code == DiagnosticCode::UnknownKey && d.message.contains("unknown_modifier")), "{values:?}");
    let invalid = open(
        &mut host,
        "common/ages/phase5-invalid.txt",
        "phase5_bad_age = { start = 1400 can_start = { always = yes } objectives = { invalid = { allow = { always = maybe } total_development = wrong } } abilities = { invalid = { ai_will_do = { factor = 1 } } } }",
    );
    assert!(
        diagnostics(&host.snapshot(), &invalid)
            .iter()
            .filter(|d| d.code == DiagnosticCode::InvalidValue)
            .count()
            >= 2
    );
}

#[test]
fn ir_gfx_chart_and_global_textcolor_wrappers_keep_structural_validation() {
    let mut host = host();
    let id = open(
        &mut host,
        "interface/charts.gfx",
        "spriteTypes = { PieChartType = { name = GFX_phase5_pie size = 17 } } spriteTypes = { LineChartType = { name = GFX_phase5_line size = { x = 100 y = 40 } linewidth = 2 } } bitmapfonts = { textcolors = { W = { 255 255 255 } B = { 0 0 255 } b = { 0 0 0 } G = { 0 255 0 } g = { 100 100 100 } } } bitmapfonts = { }",
    );
    let values = diagnostics(&host.snapshot(), &id);
    assert!(values.is_empty(), "{values:?}");
    let invalid = open(
        &mut host,
        "interface/charts-invalid.gfx",
        "spriteTypes = { PieChartType = { name = GFX_bad size = wrong } } bitmapfonts = { textcolors = { W = { 300 255 255 } } }",
    );
    let values = diagnostics(&host.snapshot(), &invalid);
    assert_eq!(
        values
            .iter()
            .filter(|d| d.code == DiagnosticCode::InvalidValue)
            .count(),
        2,
        "{values:?}"
    );
}

#[test]
fn ir_incident_weighting_links_and_event_day_counts_follow_nested_declarations() {
    let mut host = host();
    let event = open(
        &mut host,
        "events/incident.txt",
        "country_event = { id = phase5.incident mean_time_to_happen = { days = 7 } }",
    );
    assert!(
        !diagnostics(&host.snapshot(), &event)
            .iter()
            .any(|d| d.code == DiagnosticCode::InvalidValue)
    );
    let incident = open(
        &mut host,
        "common/imperial_incidents/weights.txt",
        "phase5_incident = { default_option = 0 event = phase5.incident 0 = { factor = 1 modifier = { factor = 0 F00 = { always = yes } emperor = { always = no } } } }",
    );
    let items = diagnostics(&host.snapshot(), &incident);
    assert!(
        !items.iter().any(|d| matches!(
            d.code,
            DiagnosticCode::UnknownKey
                | DiagnosticCode::InvalidValue
                | DiagnosticCode::WrongScope
                | DiagnosticCode::Cardinality
        )),
        "{items:#?}"
    );
    let bad = open(
        &mut host,
        "events/incident-invalid.txt",
        "country_event = { id = phase5.bad_day mean_time_to_happen = { days = 0 } }",
    );
    assert!(
        diagnostics(&host.snapshot(), &bad)
            .iter()
            .any(|d| d.code == DiagnosticCode::InvalidValue && d.message.contains("days"))
    );
}

#[test]
fn ir_weighted_entries_history_calls_and_gfx_sizes_keep_their_child_schemas() {
    let mut host = host();
    open(
        &mut host,
        "common/scripted_effects/history-call.txt",
        "history_writer = { set_province_flag = phase5_history_flag }",
    );
    let history = open(
        &mut host,
        "history/provinces/45 - fixture.txt",
        "history_writer = yes add_province_triggered_modifier = phase5_modifier add_province_triggered_modifier = phase5_other_modifier",
    );
    let items = diagnostics(&host.snapshot(), &history);
    assert!(
        !items.iter().any(|d| matches!(
            d.code,
            DiagnosticCode::UnknownKey | DiagnosticCode::Cardinality
        )),
        "{items:?}"
    );
    let event = open(
        &mut host,
        "events/weighted-links.txt",
        "country_event = { id = phase5.weighted immediate = { every_owned_province = { random_list = { 25 = { owner = { set_country_flag = phase5_weighted_flag } } 75 = { ROOT = { save_event_target_as = phase5_weighted_target } } } } } option = { name = phase5.weighted } }",
    );
    let snapshot = host.snapshot();
    let defs = snapshot
        .document(&event)
        .unwrap()
        .hir()
        .unwrap()
        .definitions();
    assert!(
        defs.iter()
            .any(|d| d.kind.as_ref() == "country_flag" && d.name == "phase5_weighted_flag"),
        "{defs:?}"
    );
    assert!(
        defs.iter()
            .any(|d| d.kind.as_ref() == "event_target" && d.name == "phase5_weighted_target"),
        "{defs:?}"
    );
    let items = diagnostics(&snapshot, &event);
    assert!(
        !items.iter().any(|d| matches!(
            d.code,
            DiagnosticCode::UnknownKey
                | DiagnosticCode::InvalidValue
                | DiagnosticCode::WrongScope
                | DiagnosticCode::Cardinality
        )),
        "{items:?}"
    );
    let gfx = open(
        &mut host,
        "interface/size.gfx",
        "objectTypes = { animatedmaptext = { name = phase5_text textblock = { size = { x = 256 y = 64 } position = { x = 0 y = 0 } color = { 1.0 1.0 0.0 } } } } spriteTypes = { spriteType = { name = GFX_phase5_animation animation = { animationrotationoffset = { x = 0.0 y = 0.0 } animationtexturescale = { x = 1.0 y = 1.0 } animationframes = { 1 2 3 } } } }",
    );
    let items = diagnostics(&host.snapshot(), &gfx);
    assert!(
        !items.iter().any(|d| matches!(
            d.code,
            DiagnosticCode::UnknownKey | DiagnosticCode::InvalidValue
        )),
        "{items:?}"
    );
    let gfx = open(
        &mut host,
        "interface/size-invalid.gfx",
        "objectTypes = { animatedmaptext = { name = phase5_bad_text textblock = { size = { x = wrong y = 64 } } } } spriteTypes = { spriteType = { name = GFX_phase5_bad_animation animation = { animationtexturescale = { x = wrong y = 1.0 } animationframes = { 1 bad } } } }",
    );
    assert!(
        diagnostics(&host.snapshot(), &gfx)
            .iter()
            .any(|d| d.code == DiagnosticCode::InvalidValue)
    );
}

fn open(host: &mut AnalysisHost, path: &str, source: &str) -> DocumentId {
    let id = DocumentId::new(format!("file:///fixture/{path}"));
    host.open_document(id.clone(), 1, source.to_owned(), None)
        .expect("open");
    id
}

#[test]
fn ir_template_quoted_payload_reports_parser_errors_at_argument() {
    let mut host = host();
    open(
        &mut host,
        "common/scripted_effects/quoted-syntax.txt",
        "splice = { $body$ $body$ }",
    );
    for (index, payload) in [
        "if = { limit = { always = yes } add_prestige = 1",
        "add_prestige =",
        "log = \"界\" if = { limit = { always = yes } add_prestige = 1",
    ]
    .into_iter()
    .enumerate()
    {
        let raw = format!("\"{}\"", parser::encode_quoted_script_text(payload));
        let source = format!(
            "country_event = {{ id = syntax.{index} immediate = {{ splice = {{ body = {raw} }} }} }}"
        );
        let id = open(
            &mut host,
            &format!("events/quoted-syntax-{index}.txt"),
            &source,
        );
        let script = parser::parse_quoted_script(&raw).expect("quoted payload");
        assert!(!script.parsed().errors().is_empty(), "malformed fixture");
        let start = source.find(&raw).unwrap() as u32;
        let expected = script
            .parsed()
            .errors()
            .iter()
            .map(|error| {
                let relative = script.source_map().decoded_range(error.range).unwrap();
                text::TextRange::new(start + relative.start(), start + relative.end()).unwrap()
            })
            .collect::<Vec<_>>();
        let values = diagnostics(&host.snapshot(), &id);
        let actual = values
            .iter()
            .filter(|diagnostic| diagnostic.code == DiagnosticCode::Syntax)
            .map(|diagnostic| diagnostic.range)
            .collect::<Vec<_>>();
        assert_eq!(
            actual, expected,
            "each parser error is projected once: {values:?}"
        );
    }
}

#[test]
fn ir_template_value_completion_is_independent_of_usage_order() {
    let mut host = host();
    open(
        &mut host,
        "common/scripted_effects/completion-order.txt",
        "scalar_first = { log = $p$ set_emperor = $p$ } \
         bool_first = { set_emperor = $p$ log = $p$ } \
         repeated = { log = $p$ set_emperor = $p$ set_emperor = $p$ } \
         incompatible = { set_emperor = $p$ add_prestige = $p$ }",
    );
    for name in ["scalar_first", "bool_first", "repeated", "incompatible"] {
        let source = format!(
            "country_event = {{ id = completion.{name} immediate = {{ {name} = {{ p = y }} }} }}"
        );
        let id = open(&mut host, &format!("events/completion-{name}.txt"), &source);
        let position = source.find("p = y").unwrap() as u32 + "p = y".len() as u32;
        let items = complete(&host.snapshot(), &id, position).items;
        let yes = items.iter().filter(|item| item.label == "yes").count();
        assert_eq!(
            yes,
            usize::from(name != "incompatible"),
            "all usage constraints still filter distinct candidates: {name}: {items:?}"
        );
    }
}

#[test]
fn ir_quoted_payload_must_be_valid_at_every_distinct_usage() {
    let mut host = host();
    open(
        &mut host,
        "common/scripted_effects/multiple-payload-usages.txt",
        "phase5_hybrid = { if = { limit = { $body$ } $body$ } }",
    );
    for (name, payload) in [("effect", "add_prestige = 1"), ("trigger", "always = yes")] {
        let source = format!(
            "country_event = {{ id = phase5.hybrid immediate = {{ phase5_hybrid = {{ body = \"{payload}\" }} }} }}"
        );
        let id = open(&mut host, &format!("events/hybrid-{name}.txt"), &source);
        let values = diagnostics(&host.snapshot(), &id);
        assert!(
            values.iter().any(|d| d.code == DiagnosticCode::UnknownKey
                && d.range.start() >= source.find(payload).unwrap() as u32),
            "{values:?}"
        );
    }
}

#[test]
fn ir_spliced_payloads_do_not_repeat_the_enclosing_guard_requirement() {
    let mut host = host();
    open(
        &mut host,
        "common/scripted_effects/splice.txt",
        "splice = { if = { limit = { always = yes } $payload$ } }",
    );
    let valid = open(
        &mut host,
        "events/splice-valid.txt",
        "country_event = { id = phase5.splice immediate = { splice = { payload = \"add_prestige = 1\" } } option = { name = phase5.splice } }",
    );
    let items = diagnostics(&host.snapshot(), &valid);
    assert!(
        !items.iter().any(|d| matches!(
            d.code,
            DiagnosticCode::Cardinality | DiagnosticCode::UnknownKey | DiagnosticCode::InvalidValue
        )),
        "the outer limit is already provided by the template: {items:#?}"
    );
    let invalid = open(
        &mut host,
        "events/splice-invalid.txt",
        "country_event = { id = phase5.splice_bad immediate = { splice = { payload = \"if = { add_prestige = 1 }\" } } option = { name = phase5.splice_bad } }",
    );
    assert!(
        diagnostics(&host.snapshot(), &invalid)
            .iter()
            .any(|d| d.code == DiagnosticCode::Cardinality && d.message.contains("`limit`")),
        "a nested complete block in the payload still needs its own guard"
    );
}

#[test]
fn ir_structural_flags_do_not_filter_symbol_references_or_completion() {
    let mut host = host();
    open(
        &mut host,
        "common/church_aspects/subtypes.txt",
        "ordinary = { } explicit = { is_blessing = no } blessed = { is_blessing = yes } malformed = { is_blessing = wrong }",
    );
    open(
        &mut host,
        "common/imperial_reforms/subtypes.txt",
        "hre_test = { empire = hre } china_test = { empire = celestial_empire }",
    );
    open(
        &mut host,
        "common/tradegoods/subtypes.txt",
        "coal_test = { is_latent = yes } grain_test = { }",
    );
    let valid = open(
        &mut host,
        "common/scripted_triggers/subtypes-valid.txt",
        "valid = { has_church_aspect = ordinary has_church_aspect = explicit hre_reform_passed = hre_test empire_of_china_reform_passed = china_test has_latent_trade_goods = coal_test }",
    );
    let valid_items = diagnostics(&host.snapshot(), &valid);
    assert!(
        !valid_items
            .iter()
            .any(|d| d.code == DiagnosticCode::InvalidValue),
        "{valid_items:#?}"
    );
    let religion = open(
        &mut host,
        "common/religions/subtypes.txt",
        "test_group = { test_religion = { aspects = { ordinary explicit } blessings = { blessed ordinary } } }",
    );
    let religion_items = diagnostics(&host.snapshot(), &religion);
    assert!(
        !religion_items
            .iter()
            .any(|d| d.code == DiagnosticCode::InvalidValue),
        "{religion_items:#?}"
    );
    let invalid = open(
        &mut host,
        "common/scripted_triggers/subtypes-invalid.txt",
        "invalid = { has_church_aspect = blessed has_church_aspect = malformed hre_reform_passed = china_test empire_of_china_reform_passed = hre_test has_latent_trade_goods = grain_test }",
    );
    let invalid_items = diagnostics(&host.snapshot(), &invalid);
    assert!(
        !invalid_items
            .iter()
            .any(|d| d.code == DiagnosticCode::InvalidValue),
        "{invalid_items:#?}"
    );
    let source = "suggestions = { has_church_aspect = }";
    let cursor = source.find("= }").unwrap() as u32 + 2;
    let id = open(
        &mut host,
        "common/scripted_triggers/all-aspects.txt",
        source,
    );
    let labels = complete(&host.snapshot(), &id, cursor)
        .items
        .into_iter()
        .map(|item| item.label)
        .collect::<Vec<_>>();
    for name in ["ordinary", "explicit", "blessed", "malformed"] {
        assert!(
            labels.iter().any(|label| label == name),
            "{name}: {labels:?}"
        );
    }
}

#[test]
fn ir_parameterised_symbol_patterns_follow_overlay_edits_and_closes() {
    let mut host = host();
    let declaration = open(
        &mut host,
        "common/scripted_effects/pattern.txt",
        "pattern = { set_country_flag = PREFIX_$name$_END }",
    );
    let usage = open(
        &mut host,
        "events/pattern.txt",
        "country_event = { id = phase5.pattern trigger = { has_country_flag = PREFIX_one_END } }",
    );
    let invalid = |host: &AnalysisHost| {
        diagnostics(&host.snapshot(), &usage)
            .into_iter()
            .filter(|item| item.code == DiagnosticCode::InvalidValue)
            .collect::<Vec<_>>()
    };
    assert!(invalid(&host).is_empty());
    host.apply_document_changes(
        &declaration,
        2,
        &[engine::TextChange::full(
            "pattern = { set_country_flag = OTHER_$name$_END }",
        )],
    )
    .unwrap();
    assert!(
        !invalid(&host).is_empty(),
        "the previous pattern must not survive an overlay edit"
    );
    host.apply_document_changes(
        &declaration,
        3,
        &[engine::TextChange::full(
            "pattern = { set_country_flag = PREFIX_$name$_END }",
        )],
    )
    .unwrap();
    assert!(invalid(&host).is_empty());
    host.close_document(&declaration).unwrap();
    assert!(
        !invalid(&host).is_empty(),
        "closing the only declaration must remove its pattern"
    );
}

#[test]
fn ir_scope_values_enforce_link_origins() {
    let mut host = host();
    let source = "country_event = { id = phase5.origins trigger = { same_continent = owner capital_scope = { same_continent = capital_scope same_continent = owner } same_continent = capital_scope } }";
    let id = open(&mut host, "events/origins.txt", source);
    let items = diagnostics(&host.snapshot(), &id);
    let invalid = items
        .iter()
        .filter(|item| item.code == DiagnosticCode::WrongScope)
        .map(|item| &source[item.range.start() as usize..item.range.end() as usize])
        .collect::<Vec<_>>();
    assert_eq!(invalid, ["owner", "capital_scope"], "{items:#?}");
    assert!(
        items
            .iter()
            .all(|item| item.code != DiagnosticCode::InvalidValue),
        "{items:#?}"
    );
}

#[test]
fn ir_indexed_symbol_patterns_are_hidden_by_overlays_and_restored_on_close() {
    let root = super::support::temp_root("ir-indexed-pattern");
    let path = root.join("common/scripted_effects/pattern.txt");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "pattern = { set_country_flag = PREFIX_$name$_END }").unwrap();
    let mut host = host();
    host.apply_change(engine::WorkspaceChange::SetSourceRoots(vec![
        engine::SourceRoot::new(
            engine::SourceRootId::new(1),
            engine::SourceRootKind::Project,
            text::AbsPath::normalize(&root),
        ),
    ]));
    host.refresh_source_roots().unwrap();
    let usage = open(
        &mut host,
        "events/indexed-pattern.txt",
        "country_event = { id = phase5.pattern trigger = { has_country_flag = PREFIX_one_END } }",
    );
    let invalid = |host: &AnalysisHost| {
        diagnostics(&host.snapshot(), &usage)
            .iter()
            .any(|item| item.code == DiagnosticCode::InvalidValue)
    };
    assert!(!invalid(&host)); // Populate the immutable index pattern cache.
    let declaration = DocumentId::new(format!("file://{}", path.display()));
    host.open_document(
        declaration.clone(),
        1,
        "pattern = { set_country_flag = OTHER_$name$_END }".into(),
        Some(text::AbsPath::normalize(&path)),
    )
    .unwrap();
    assert!(invalid(&host), "the overlay must hide the indexed pattern");
    host.close_document(&declaration).unwrap();
    assert!(
        !invalid(&host),
        "closing the overlay restores the indexed pattern"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn ir_iterators_and_weighted_branches_do_not_inherit_wrapper_requirements() {
    let mut host = host();
    let source = "country_event = { id = phase5.wrappers option = { name = phase5.choice } trigger = { calc_true_if = { amount = 1 all_country = { always = yes } } custom_trigger_tooltip = { tooltip = phase5.tip any_owned_province = { always = yes } } } immediate = { if = { limit = { always = yes } random_list = { 20 = { trigger = { always = yes } modifier = { factor = 2 any_country = { always = yes } } add_prestige = 1 } 80 = { } } } } }";
    let id = open(&mut host, "events/wrappers.txt", source);
    let items = diagnostics(&host.snapshot(), &id);
    assert!(
        items.iter().all(|item| !matches!(
            item.code,
            DiagnosticCode::Cardinality | DiagnosticCode::UnknownKey | DiagnosticCode::InvalidValue
        )),
        "{items:#?}"
    );
    assert!(
        items
            .iter()
            .all(|item| item.code != DiagnosticCode::EmptyBlock),
        "scalar tooltip metadata must not be treated as an empty block: {items:#?}"
    );
    let bad = open(
        &mut host,
        "events/wrappers-invalid.txt",
        "country_event = { id = phase5.bad trigger = { calc_true_if = { all_country = { always = yes } } } immediate = { random_list = { 10 = { modifier = { factor = wrong } add_prestige = wrong } } } }",
    );
    let items = diagnostics(&host.snapshot(), &bad);
    assert!(items.iter().any(|item| item.code == DiagnosticCode::Cardinality && item.message.contains("amount")), "the actual outer requirement remains: {items:#?}");
    assert_eq!(
        items
            .iter()
            .filter(|item| item.code == DiagnosticCode::InvalidValue)
            .count(),
        2,
        "{items:#?}"
    );
}

#[test]
fn ir_unit_battle_triggers_accept_confirmed_fields_without_accepting_provinces() {
    let mut host = host();
    open(
        &mut host,
        "common/mercenary_companies/unit.txt",
        "phase5_company = { }",
    );
    let valid = open(
        &mut host,
        "common/on_actions/unit.txt",
        "on_battle_won_unit = { if = { limit = { general_with_name = Damarwulan mercenary_company = phase5_company } } }",
    );
    let items = diagnostics(&host.snapshot(), &valid);
    assert!(
        items.iter().all(|d| !matches!(
            d.code,
            DiagnosticCode::WrongScope | DiagnosticCode::UnknownKey | DiagnosticCode::InvalidValue
        )),
        "{items:#?}"
    );
    let invalid = open(
        &mut host,
        "events/unit-triggers-in-province.txt",
        "country_event = { id = phase5.unit_invalid trigger = { any_owned_province = { general_with_name = Damarwulan mercenary_company = phase5_company } } option = { name = phase5.unit_invalid } }",
    );
    let items = diagnostics(&host.snapshot(), &invalid);
    for field in ["general_with_name", "mercenary_company"] {
        assert!(
            items
                .iter()
                .any(|d| d.code == DiagnosticCode::WrongScope && d.message.contains(field)),
            "{items:#?}"
        );
    }
}

#[test]
fn ir_trade_node_commands_accept_arbitrary_provinces_and_keep_country_errors() {
    let mut host = host();
    open(
        &mut host,
        "common/country_tags/trade-scope.txt",
        "FRA = \"countries/France.txt\"",
    );
    let triggers = [
        "all_privateering_country",
        "all_trade_node_member_country",
        "any_country_active_in_node",
        "any_privateering_country",
        "strongest_trade_power",
    ]
    .map(|key| format!("{key} = {{ tag = FRA }}"))
    .join(" ");
    let effects = [
        "every_privateering_country",
        "every_trade_node_member_country",
        "random_privateering_country",
        "random_trade_node_member_country",
    ]
    .map(|key| format!("{key} = {{ limit = {{ always = yes }} add_prestige = 1 }}"))
    .join(" ");
    let effects = format!(
        "{effects} random_trade_node_member_province = {{ limit = {{ always = yes }} add_base_tax = 1 }}"
    );
    let source = format!(
        "country_event = {{ id = phase5.province_node trigger = {{ any_owned_province = {{ {triggers} most_province_trade_power = {{ tag = FRA }} }} }} immediate = {{ every_owned_province = {{ {effects} }} }} option = {{ name = phase5.province_node }} }}"
    );
    let valid = open(&mut host, "events/province-node-commands.txt", &source);
    let items = diagnostics(&host.snapshot(), &valid);
    assert!(
        items.iter().all(|d| !matches!(
            d.code,
            DiagnosticCode::WrongScope
                | DiagnosticCode::UnknownKey
                | DiagnosticCode::InvalidValue
                | DiagnosticCode::Cardinality
        )),
        "{items:#?}"
    );

    let source = format!(
        "country_event = {{ id = phase5.country_node trigger = {{ {triggers} }} immediate = {{ {effects} }} option = {{ name = phase5.country_node }} }}"
    );
    let invalid = open(&mut host, "events/country-node-commands.txt", &source);
    let items = diagnostics(&host.snapshot(), &invalid);
    for key in [
        "all_privateering_country",
        "all_trade_node_member_country",
        "any_country_active_in_node",
        "any_privateering_country",
        "strongest_trade_power",
        "every_privateering_country",
        "every_trade_node_member_country",
        "random_privateering_country",
        "random_trade_node_member_country",
        "random_trade_node_member_province",
    ] {
        assert!(
            items
                .iter()
                .any(|d| d.code == DiagnosticCode::WrongScope && d.message.contains(key)),
            "{key}: {items:#?}"
        );
    }
}

#[test]
fn ir_trade_node_entries_push_province_and_references_still_navigate() {
    let mut host = host();
    open(&mut host, "map/positions.txt", "1 = { position = { 0 0 } }");
    let node = open(
        &mut host,
        "common/tradenodes/scope-merge.txt",
        "phase5_node = { location = 1 members = { 1 } }",
    );
    let body = "add_base_tax = 1 owner = { add_prestige = 1 } every_trade_node_member_country = { limit = { always = yes } add_prestige = 1 }";
    let iterators = [
        "every_active_trade_node",
        "every_trade_node",
        "random_active_trade_node",
        "random_trade_node",
        "home_trade_node_effect_scope",
    ];
    let effects = iterators
        .map(|key| format!("{key} = {{ limit = {{ always = yes }} {body} }}"))
        .join(" ");
    // The direct home-node scope has no iterator guard.
    let effects = effects.replace(
        "home_trade_node_effect_scope = { limit = { always = yes }",
        "home_trade_node_effect_scope = {",
    );
    let source = format!(
        "country_event = {{ id = phase5.node_entries immediate = {{ {effects} phase5_node = {{ {body} }} }} option = {{ name = phase5.node_entries }} }}"
    );
    let id = open(&mut host, "events/node-entries.txt", &source);
    let snapshot = host.snapshot();
    let items = diagnostics(&snapshot, &id);
    assert!(
        items.iter().all(|d| !matches!(
            d.code,
            DiagnosticCode::WrongScope
                | DiagnosticCode::UnknownKey
                | DiagnosticCode::InvalidValue
                | DiagnosticCode::Cardinality
        )),
        "{items:#?}"
    );
    let hir = snapshot.document(&id).unwrap().hir().unwrap();
    for key in iterators.into_iter().chain(["phase5_node"]) {
        let start = source.find(&format!("{key} = {{")).unwrap() as u32;
        let fact = hir
            .scope_facts()
            .iter()
            .find(|fact| fact.range.start() == start)
            .expect("entry scope fact");
        assert_eq!(
            fact.transition.as_ref().unwrap().current[0],
            hir::ScopeValue::known_single("province"),
            "{key}: {fact:?}"
        );
    }
    let targets = definition(
        &snapshot,
        &id,
        source.find("phase5_node").unwrap() as u32 + 1,
    );
    assert_eq!(targets.len(), 1, "{targets:?}");
    assert_eq!(targets[0].document.as_ref(), Some(&node));
    assert!(hir.references().iter().any(
        |reference| reference.kind.as_ref() == "trade_node" && reference.name == "phase5_node"
    ));

    let source = "province_event = { id = phase5.node_values trigger = { trade_node = ROOT same_trade_node_as = ROOT } }";
    let id = open(&mut host, "events/node-province-values.txt", source);
    assert!(
        diagnostics(&host.snapshot(), &id).iter().all(|d| !matches!(
            d.code,
            DiagnosticCode::InvalidValue | DiagnosticCode::WrongScope
        )),
        "{:?}",
        diagnostics(&host.snapshot(), &id)
    );
}

#[test]
fn ir_province_completion_includes_trade_node_commands_without_a_second_scope() {
    let mut host = host();
    for (scope, expected) in [("province", true), ("country", false)] {
        let source = format!("{scope}_event = {{ id = phase5.node_completion trigger = {{  }} }}");
        let id = open(
            &mut host,
            &format!("events/{scope}-node-completion.txt"),
            &source,
        );
        let items = complete(
            &host.snapshot(),
            &id,
            source.find("  }").unwrap() as u32 + 1,
        )
        .items;
        for key in [
            "all_privateering_country",
            "any_country_active_in_node",
            "strongest_trade_power",
            "trade_node_value",
        ] {
            assert_eq!(
                items.iter().any(|item| item.label == key),
                expected,
                "{scope}/{key}"
            );
        }
        assert!(
            !items
                .iter()
                .any(|item| item.label == "trade_node" && item.insert_text.contains('{')),
            "removed concrete scope is not a scope block"
        );
    }
}

#[test]
fn ir_trade_policy_from_register_uses_province_for_all_node_operations() {
    let mut host = host();
    let source = "phase5_policy = { can_select = { FROM = { is_city = yes strongest_trade_power = { always = yes } owner = { always = yes } } } can_maintain = { FROM = { any_country_active_in_node = { always = yes } is_city = yes } } node_province_modifier = { local_tax_modifier = 0.1 siege_ability = 0.1 } }";
    let id = open(&mut host, "common/trading_policies/merged-from.txt", source);
    let snapshot = host.snapshot();
    let items = diagnostics(&snapshot, &id);
    assert!(
        items.iter().all(|d| !matches!(
            d.code,
            DiagnosticCode::WrongScope | DiagnosticCode::UnknownKey | DiagnosticCode::InvalidValue
        )),
        "{items:#?}"
    );
    let hir = snapshot.document(&id).unwrap().hir().unwrap();
    for key in ["can_select", "can_maintain"] {
        let start = source.find(key).unwrap() as u32;
        let fact = hir
            .scope_facts()
            .iter()
            .find(|fact| fact.range.start() == start)
            .expect("policy register fact");
        assert_eq!(
            fact.transition.as_ref().unwrap().from[0],
            hir::ScopeValue::known_single("province"),
            "{fact:?}"
        );
    }
}

#[test]
fn ir_strongest_trade_power_accepts_provinces_and_pushes_country_scope() {
    let mut host = host();
    let valid = open(
        &mut host,
        "events/province-trade-power.txt",
        "country_event = { id = phase5.trade_power trigger = { any_owned_province = { strongest_trade_power = { tag = ROOT } } } option = { name = phase5.trade_power } }",
    );
    let items = diagnostics(&host.snapshot(), &valid);
    assert!(
        items.iter().all(|d| !matches!(
            d.code,
            DiagnosticCode::WrongScope | DiagnosticCode::UnknownKey | DiagnosticCode::InvalidValue
        )),
        "{items:#?}"
    );
    let invalid = open(
        &mut host,
        "events/country-trade-power.txt",
        "country_event = { id = phase5.trade_power_invalid trigger = { strongest_trade_power = { always = yes } } option = { name = phase5.trade_power_invalid } }",
    );
    assert!(
        diagnostics(&host.snapshot(), &invalid)
            .iter()
            .any(|d| d.code == DiagnosticCode::WrongScope
                && d.message.contains("strongest_trade_power"))
    );
}

#[test]
fn ir_trade_company_investment_filter_is_optional_but_other_fields_stay_checked() {
    let mut host = host();
    let id = open(
        &mut host,
        "events/investment-filter.txt",
        "country_event = { id = phase5.investment trigger = { any_owned_province = { has_trade_company_investment_in_area = { investor = ROOT count_one_per_area = yes } } } option = { name = phase5.investment } }",
    );
    let items = diagnostics(&host.snapshot(), &id);
    assert!(
        items.iter().all(|d| !matches!(
            d.code,
            DiagnosticCode::Cardinality | DiagnosticCode::InvalidValue | DiagnosticCode::WrongScope
        )),
        "{items:#?}"
    );
    let invalid = open(
        &mut host,
        "events/investment-filter-invalid.txt",
        "country_event = { id = phase5.investment_invalid trigger = { any_owned_province = { has_trade_company_investment_in_area = { count_one_per_area = no investment = phase5_missing_investment } } } option = { name = phase5.investment_invalid } }",
    );
    let items = diagnostics(&host.snapshot(), &invalid);
    assert!(
        items
            .iter()
            .any(|d| d.code == DiagnosticCode::Cardinality && d.message.contains("investor")),
        "{items:#?}"
    );
    assert!(
        items.iter().any(|d| d.code == DiagnosticCode::InvalidValue
            && d.message.contains("phase5_missing_investment")),
        "{items:#?}"
    );
    assert!(
        items
            .iter()
            .any(|d| d.code == DiagnosticCode::InvalidValue && d.message.contains("no")),
        "{items:#?}"
    );
}

#[test]
fn ir_trade_policy_node_province_modifiers_allow_confirmed_attributes_locally() {
    let mut host = host();
    let valid = open(
        &mut host,
        "common/trading_policies/node-modifiers.txt",
        "phase5_policy = { node_province_modifier = { siege_ability = 0.10 artillery_levels_available_vs_fort = 1 local_tax_modifier = 0.1 } }",
    );
    let items = diagnostics(&host.snapshot(), &valid);
    assert!(
        items.iter().all(|d| !matches!(
            d.code,
            DiagnosticCode::WrongScope | DiagnosticCode::UnknownKey | DiagnosticCode::InvalidValue
        )),
        "{items:#?}"
    );
    let invalid = open(
        &mut host,
        "common/trading_policies/node-modifiers-invalid.txt",
        "phase5_policy_invalid = { node_province_modifier = { siege_ability = wrong phase5_unknown_modifier = 1 global_tax_modifier = 0.1 } }",
    );
    let items = diagnostics(&host.snapshot(), &invalid);
    assert!(
        items
            .iter()
            .any(|d| d.code == DiagnosticCode::InvalidValue && d.message.contains("wrong")),
        "{items:#?}"
    );
    assert!(
        items.iter().any(|d| d.code == DiagnosticCode::UnknownKey
            && d.message.contains("phase5_unknown_modifier")),
        "{items:#?}"
    );
    assert!(
        items
            .iter()
            .any(|d| d.code == DiagnosticCode::WrongScope
                && d.message.contains("global_tax_modifier")),
        "{items:#?}"
    );
    let outside = open(
        &mut host,
        "common/trading_policies/country-modifiers.txt",
        "phase5_country_policy = { node_modifier = { siege_ability = 0.1 artillery_levels_available_vs_fort = 1 } }",
    );
    assert!(
        !diagnostics(&host.snapshot(), &outside)
            .iter()
            .any(|d| d.code == DiagnosticCode::WrongScope)
    );
}

#[test]
fn ir_repeated_government_custom_attributes_are_errors_and_values_stay_checked() {
    let mut host = host();
    let declaration = open(
        &mut host,
        "common/government_reforms/repeated-attributes.txt",
        "phase5_repeated_reform = { custom_attributes = { phase5_attribute_one = yes } custom_attributes = { phase5_attribute_two = no } }",
    );
    let items = diagnostics(&host.snapshot(), &declaration);
    assert_eq!(
        items
            .iter()
            .filter(|d| d.code == DiagnosticCode::Cardinality
                && d.message.contains("custom_attributes"))
            .count(),
        1,
        "{items:#?}"
    );
    let invalid = open(
        &mut host,
        "common/government_reforms/repeated-attributes-invalid.txt",
        "phase5_invalid_reform = { custom_attributes = { phase5_attribute_three = yes } custom_attributes = { phase5_attribute_four = wrong } }",
    );
    let items = diagnostics(&host.snapshot(), &invalid);
    assert!(
        items
            .iter()
            .any(|d| d.code == DiagnosticCode::InvalidValue && d.message.contains("wrong")),
        "{items:#?}"
    );
    assert!(
        items
            .iter()
            .any(|d| d.code == DiagnosticCode::Cardinality
                && d.message.contains("custom_attributes")),
        "{items:#?}"
    );
}

#[test]
fn ir_government_reforms_have_optional_properties_and_define_custom_attributes() {
    let mut host = host();
    let declaration = open(
        &mut host,
        "common/government_reforms/attributes.txt",
        "plain_reform = { custom_attributes = { phase5_new_attribute = yes gives_war_against_the_world_tooltip_dummy = no } } another_reform = { allow_banners = yes min_autonomy = 10 }",
    );
    let items = diagnostics(&host.snapshot(), &declaration);
    assert!(
        items.iter().all(|item| !matches!(
            item.code,
            DiagnosticCode::Cardinality | DiagnosticCode::UnknownKey | DiagnosticCode::InvalidValue
        )),
        "{items:#?}"
    );
    let usage = open(
        &mut host,
        "events/attributes.txt",
        "country_event = { id = phase5.attributes trigger = { has_government_attribute = phase5_new_attribute has_government_attribute = gives_war_against_the_world_tooltip_dummy has_government_attribute = raze_province has_government_attribute = cannot_form_alliances has_government_attribute = heir has_government_attribute = queen has_government_attribute = states_general_mechanic } }",
    );
    let items = diagnostics(&host.snapshot(), &usage);
    assert!(
        items
            .iter()
            .all(|item| item.code != DiagnosticCode::InvalidValue),
        "{items:#?}"
    );
    let unknown = open(
        &mut host,
        "events/attributes-unknown.txt",
        "country_event = { id = phase5.attributes_unknown trigger = { has_government_attribute = phase5_undefined_attribute } }",
    );
    assert!(
        diagnostics(&host.snapshot(), &unknown)
            .iter()
            .any(|item| item.code == DiagnosticCode::InvalidValue
                && item.message.contains("phase5_undefined_attribute"))
    );
    let bad = open(
        &mut host,
        "common/government_reforms/attributes-invalid.txt",
        "bad_reform = { min_autonomy = wrong custom_attributes = { phase5_bad_attribute = wrong } }",
    );
    let items = diagnostics(&host.snapshot(), &bad);
    assert_eq!(
        items
            .iter()
            .filter(|item| item.code == DiagnosticCode::InvalidValue)
            .count(),
        2,
        "{items:#?}"
    );
}

#[test]
fn ir_absolute_overlay_paths_are_classified_without_source_roots() {
    let mut host = host();
    let id = DocumentId::new("file:///tmp/phase5/interface/overlay.gfx");
    host.open_document(
        id.clone(),
        1,
        "spriteTypes = { spriteType = { name = GFX_phase5_overlay } }".into(),
        Some(text::AbsPath::normalize(std::path::Path::new(
            "/tmp/phase5/interface/overlay.gfx",
        ))),
    )
    .unwrap();
    let snapshot = host.snapshot();
    let hir = snapshot.document(&id).unwrap().hir().unwrap();
    assert!(hir.uses_ir());
    assert!(hir.definitions().iter().any(|definition| {
        definition.kind.as_ref() == "sprite" && definition.name == "GFX_phase5_overlay"
    }));
    let event = open(
        &mut host,
        "events/overlay.txt",
        "country_event = { id = phase5.overlay picture = GFX_phase5_overlay }",
    );
    assert!(diagnostics(&host.snapshot(), &event).iter().all(|item| {
        item.code != DiagnosticCode::InvalidValue || !item.message.contains("GFX_phase5_overlay")
    }));
}

#[test]
fn ir_definition_affixes_are_preserved_by_navigation_and_rename() {
    let mut host = host();
    open(
        &mut host,
        "common/scripted_effects/ancestors.txt",
        "add_ruler_personality_ancestor = { add_ruler_personality = ancestor_$key$_personality }",
    );
    let source = "ancestor_sage_personality = {}";
    let declaration = open(
        &mut host,
        "common/ancestor_personalities/affixes.txt",
        source,
    );
    let call_source = "country_event = { id = phase5.affixes immediate = { add_ruler_personality_ancestor = { key = sage } } }";
    let call = open(&mut host, "events/affixes.txt", call_source);
    let snapshot = host.snapshot();
    let position = call_source.find("sage").unwrap() as u32;
    let targets = definition(&snapshot, &call, position + 1);
    assert_eq!(targets.len(), 1, "{targets:?}");
    let range = targets[0].range;
    assert_eq!(
        &source[range.start() as usize..range.end() as usize],
        "sage"
    );
    let edits = crate::rename(&snapshot, &call, position + 1, "wise").unwrap();
    let edit = edits
        .edits
        .iter()
        .find(|edit| edit.location.document.as_ref() == Some(&declaration))
        .unwrap();
    assert_eq!(
        &source[edit.location.range.start() as usize..edit.location.range.end() as usize],
        "sage"
    );
}

#[test]
fn ir_iterators_preserve_their_trigger_and_effect_vocabularies() {
    let mut host = host();
    let source = "country_event = { id = phase5.iterators \
        trigger = { any_country = { always = yes } every_country = {} random_country = {} } \
        immediate = { any_country = {} every_country = { limit = { any_country = { always = yes } } \
        every_owned_province = { add_base_tax = 1 PREV = { add_prestige = 1 } } } \
        random_country = { add_prestige = 1 } } }";
    let id = open(&mut host, "events/iterators.txt", source);
    let snapshot = host.snapshot();
    let items = diagnostics(&snapshot, &id);
    let unknown = items
        .iter()
        .filter(|item| item.code == DiagnosticCode::UnknownKey)
        .map(|item| &source[item.range.start() as usize..item.range.end() as usize])
        .collect::<Vec<_>>();
    assert_eq!(
        unknown,
        ["every_country", "random_country", "any_country"],
        "{items:#?}"
    );
    assert!(
        items
            .iter()
            .all(|item| item.code != DiagnosticCode::WrongScope),
        "{items:#?}"
    );
    for (prefix, allowed, forbidden) in [
        ("trigger = { ", "any_country", "every_country"),
        ("immediate = { ", "every_country", "any_country"),
        ("limit = { ", "any_country", "random_country"),
    ] {
        let position = (source.find(prefix).unwrap() + prefix.len() - 1) as u32;
        let result = complete(&snapshot, &id, position);
        assert!(
            result.items.iter().any(|item| item.label == allowed),
            "{result:?}"
        );
        assert!(
            result.items.iter().all(|item| item.label != forbidden),
            "{result:?}"
        );
    }
    let input = crate::support::input_for_document(&snapshot, &id).unwrap();
    let hir = input.hir.unwrap();
    let tax = hir
        .properties()
        .iter()
        .find(|property| property.key == "add_base_tax")
        .unwrap();
    let state = &hir.scope_fact_at(tax.key_range).unwrap().state;
    assert_eq!(
        state.current.first(),
        Some(&hir::ScopeValue::known_single("province"))
    );
    assert_eq!(
        state.previous.first(),
        Some(&hir::ScopeValue::known_single("country"))
    );
}

#[test]
fn ir_template_this_retains_the_callers_current_scope() {
    let mut host = host();
    let id = open(
        &mut host,
        "common/scripted_effects/this-contract.txt",
        "clash = { add_prestige = 1 THIS = { change_province_name = \"X\" } }\n\
         province_only = { THIS = { change_province_name = \"X\" } }",
    );
    let snapshot = host.snapshot();
    let report = crate::dynamic_contracts::dynamic_contract_report_view(
        &snapshot,
        &crate::CancellationToken::new(),
    )
    .unwrap();
    assert_eq!(
        report.contract("scripted_effect", "clash"),
        Some(&crate::dynamic_contracts::ScopeContract::Empty)
    );
    assert_eq!(
        report.contract("scripted_effect", "province_only"),
        Some(&crate::dynamic_contracts::ScopeContract::Scopes(vec![
            "province".into()
        ]))
    );
    assert!(
        diagnostics(&snapshot, &id)
            .iter()
            .any(|item| item.code == DiagnosticCode::EmptyScopeContract)
    );
}

#[test]
fn ir_register_blocks_enter_the_selected_register_scope() {
    let mut host = host();
    let source = "country_event = { id = phase5.registers immediate = { capital_scope = { \
        ROOT = { add_prestige = 1 } PREV = { add_prestige = 1 } \
        THIS = { add_base_tax = 1 } PREV_PREV = { add_prestige = 1 } } } }";
    let id = open(&mut host, "events/registers.txt", source);
    let snapshot = host.snapshot();
    let input = crate::support::input_for_document(&snapshot, &id).unwrap();
    let hir = input.hir.unwrap();
    let fields = hir
        .properties()
        .iter()
        .filter(|property| property.key == "add_prestige")
        .collect::<Vec<_>>();
    assert_eq!(fields.len(), 3);
    for property in &fields[..2] {
        assert_eq!(
            hir.scope_fact_at(property.key_range)
                .unwrap()
                .state
                .current
                .first(),
            Some(&hir::ScopeValue::known_single("country"))
        );
    }
    assert!(
        diagnostics(&snapshot, &id)
            .iter()
            .all(|item| item.code != DiagnosticCode::WrongScope),
        "{:?}",
        diagnostics(&snapshot, &id)
    );
}

#[test]
fn ir_template_embedded_substitutions_require_bindings() {
    let mut host = host();
    open(
        &mut host,
        "common/scripted_effects/embedded-parameters.txt",
        "composite = { set_country_flag = PREFIX_$optional$_END }\n\
         inner = { [[optional] add_prestige = $optional$ ] }\n\
         outer = { inner = { optional = \"$optional$\" } }\n\
         protected = { [[optional] inner = { optional = \"$optional$\" } ] }\n\
         preview = { tooltip = { set_country_flag = PREVIEW_$optional$ } }",
    );
    let source = "country_event = { id = phase5.params immediate = { composite = {} outer = {} protected = {} preview = {} } }";
    let id = open(&mut host, "events/embedded-parameters.txt", source);
    let items = diagnostics(&host.snapshot(), &id);
    let missing = items
        .iter()
        .filter(|item| {
            item.code == DiagnosticCode::Cardinality && item.message.contains("optional")
        })
        .collect::<Vec<_>>();
    assert_eq!(missing.len(), 3, "{items:#?}");
    assert!(missing[0].message.contains("`composite`"), "{items:#?}");
    assert_eq!(
        missing[0].range.start(),
        source.find("composite").unwrap() as u32
    );
    for name in ["outer", "preview"] {
        assert!(
            missing
                .iter()
                .any(|d| d.message.contains(&format!("`{name}`"))
                    && d.range.start() == source.find(name).unwrap() as u32),
            "{items:#?}"
        );
    }
}

#[test]
fn ir_logic_constant_lints_require_a_declared_constant_predicate() {
    let mut host = host();
    let source = "province_event = { id = phase5.constant trigger = { \
        OR = { is_capital = yes } OR = { always = yes } NOT = { always = no } } }";
    let id = open(&mut host, "events/constants.txt", source);
    let items = diagnostics(&host.snapshot(), &id);
    let constants = items
        .iter()
        .filter(|item| item.code == DiagnosticCode::ConstantCondition)
        .collect::<Vec<_>>();
    assert_eq!(constants.len(), 2, "{items:#?}");
    assert!(
        constants
            .iter()
            .all(|item| item.range.start() > source.find("is_capital").unwrap() as u32),
        "{items:#?}"
    );
}

#[test]
fn ir_template_quoted_payload_references_navigate_and_rename() {
    let mut host = host();
    open(
        &mut host,
        "common/scripted_effects/quoted-navigation.txt",
        "inject = { $BODY$ }",
    );
    let source = "country_event = { id = phase5.target }\ncountry_event = { id = phase5.caller \
        immediate = { inject = { BODY = \"country_event = { id = phase5.target }\" } } }";
    let id = open(&mut host, "events/quoted-navigation.txt", source);
    let snapshot = host.snapshot();
    let reference = source.rfind("phase5.target").unwrap() as u32;
    let declaration = source.find("phase5.target").unwrap() as u32;
    let targets = definition(&snapshot, &id, reference + 1);
    assert_eq!(targets.len(), 1, "{targets:?}");
    assert_eq!(
        targets[0].range.start(),
        declaration,
        "targets {targets:?}; semantics {:?}",
        crate::resolution::semantic_data(
            &snapshot,
            &crate::support::input_for_document(&snapshot, &id).unwrap()
        )
    );
    let edit = crate::rename(&snapshot, &id, reference + 1, "phase5.renamed").unwrap();
    for offset in [reference, declaration] {
        assert!(
            edit.edits
                .iter()
                .any(|item| item.location.range.start() == offset),
            "{edit:?}"
        );
    }
}

#[test]
fn ir_modifier_containers_inherit_fields_and_templates_with_value_checks() {
    let mut host = host();
    let estate = open(
        &mut host,
        "common/estates/inherited.txt",
        "estate_clergy = { }",
    );
    for (path, source) in [
        (
            "common/policies/inherited.txt",
            "phase5_policy = { monarch_power = ADM potential = { always = yes } \
             allow = { always = yes } ai_will_do = { factor = 1 } \
             global_tax_modifier = 0.1 clergy_loyalty_modifier = 5 }",
        ),
        (
            "common/static_modifiers/inherited.txt",
            "phase5_static = { defensiveness = 0.1 global_trade_power = 0.2 prestige = 1 }",
        ),
        (
            "common/triggered_modifiers/inherited.txt",
            "phase5_triggered = { potential = { always = yes } trigger = { always = yes } \
             global_unrest = -1 }",
        ),
        (
            "common/ruler_personalities/inherited.txt",
            "phase5_personality = { nation_designer_cost = 0 gift_chance = 10 \
             global_tax_modifier = 0.1 }",
        ),
        (
            "common/ideas/inherited.txt",
            "phase5_ideas = { category = ADM bonus = { clergy_loyalty_modifier = 5 } phase5_idea = { clergy_loyalty_modifier = 5 } }",
        ),
    ] {
        let id = open(&mut host, path, source);
        let values = diagnostics(&host.snapshot(), &id);
        assert!(
            values.iter().all(|item| !matches!(
                item.code,
                DiagnosticCode::UnknownKey | DiagnosticCode::InvalidValue
            )),
            "{path}: {values:#?}"
        );
        if let Some(position) = source.find("clergy_loyalty_modifier") {
            let locations = definition(&host.snapshot(), &id, position as u32 + 2);
            assert!(
                locations
                    .iter()
                    .any(|location| location.document.as_ref() == Some(&estate)),
                "the stripped template name must navigate to estate_clergy: {locations:?}"
            );
        }
    }
    let invalid = open(
        &mut host,
        "common/static_modifiers/invalid-inherited.txt",
        "phase5_invalid = { prestige = yes arbitrary_modifier = 0.1 }",
    );
    let values = diagnostics(&host.snapshot(), &invalid);
    assert!(
        values
            .iter()
            .any(|item| item.code == DiagnosticCode::InvalidValue)
    );
    assert!(values.iter().any(|item| {
        item.code == DiagnosticCode::UnknownKey && item.message.contains("arbitrary_modifier")
    }));
}

#[test]
fn ir_inherited_logic_bodies_do_not_retain_wrapper_requirements() {
    let mut host = host();
    let source = "country_event = { id = phase5.logic \
        trigger = { if = { limit = { NOT = { always = no } } OR = { always = yes } } \
        custom_trigger_tooltip = { tooltip = phase5.tip NOT = { always = no } } } \
        mean_time_to_happen = { months = 12 modifier = { factor = 2 NOT = { always = no } } } \
        immediate = { if = { limit = { always = yes } hidden_effect = { add_prestige = 1 } } } \
        option = { name = phase5.tip } }";
    let id = open(&mut host, "events/logic.txt", source);
    let values = diagnostics(&host.snapshot(), &id);
    assert!(
        !values.iter().any(|item| matches!(
            item.code,
            DiagnosticCode::Cardinality | DiagnosticCode::UnknownKey
        )),
        "{values:#?}"
    );

    let invalid = open(
        &mut host,
        "events/missing-wrapper-fields.txt",
        "country_event = { id = phase5.invalid trigger = { custom_trigger_tooltip = { always = yes } } \
         mean_time_to_happen = { months = 12 modifier = { always = yes } } }",
    );
    let diagnostics = diagnostics(&host.snapshot(), &invalid);
    for field in ["tooltip", "factor"] {
        assert!(
            diagnostics.iter().any(|item| {
                item.code == DiagnosticCode::Cardinality && item.message.contains(field)
            }),
            "the wrapper still requires {field}: {diagnostics:#?}"
        );
    }
}

#[test]
fn ir_mission_titles_expand_the_active_localised_trait() {
    let host = host();
    let snapshot = host.snapshot();
    assert_eq!(
        snapshot.localisation_template_key("mission", "name", "phase5"),
        Some("phase5_title".into())
    );
    assert_eq!(
        snapshot.localisation_template_key("mission", "desc", "phase5"),
        Some("phase5_desc".into())
    );
    assert_eq!(
        snapshot.localisation_template_key("mission", "missing", "phase5"),
        None
    );
    assert_eq!(
        snapshot.localisation_template_key("event", "name", "phase5"),
        None
    );
}

#[test]
fn ir_localisation_value_completion_reads_overlay_keys() {
    let mut host = host();
    open(
        &mut host,
        "localisation/options_l_english.yml",
        "l_english:\n phase5_option:0 \"Option\"\n phase5_other:0 \"Other\"\n",
    );
    let source = "country_event = { id = phase5.loc option = { name = phase5_op } }";
    let id = open(&mut host, "events/localisation-completion.txt", source);
    let position = source.find("phase5_op").unwrap() as u32 + "phase5_op".len() as u32;
    let result = complete(&host.snapshot(), &id, position);
    assert!(
        result
            .items
            .iter()
            .any(|item| item.label == "phase5_option"),
        "{:?}",
        result.items
    );
    assert!(!result.items.iter().any(|item| item.label == "phase5_other"));
}

#[test]
fn ir_scalar_completions_filter_overloads_by_declared_and_inferred_scope() {
    let mut host = host();
    open(
        &mut host,
        "map/positions.txt",
        "100 = { position = { 0 0 } }",
    );
    open(
        &mut host,
        "common/country_tags/overloads.txt",
        "FRA = \"countries/France.txt\"",
    );
    for (path, source, expected, rejected) in [
        (
            "events/overloads.txt",
            "country_event = { id = phase5.overload immediate = { add_core = } }",
            "100",
            "FRA",
        ),
        (
            "history/provinces/100 - Overload.txt",
            "add_core = ",
            "FRA",
            "100",
        ),
        (
            "common/scripted_effects/overloads.txt",
            "select = { add_prestige = 1 add_core = 100 }",
            "100",
            "FRA",
        ),
    ] {
        let id = open(&mut host, path, source);
        let position = source.find("add_core =").unwrap() as u32 + "add_core =".len() as u32;
        let items = complete(&host.snapshot(), &id, position).items;
        assert!(
            items.iter().any(|item| item.label == expected),
            "{path}: missing {expected}: {items:?}"
        );
        assert!(
            !items.iter().any(|item| item.label == rejected),
            "{path}: wrong overload {rejected}"
        );
    }
    let source = "choose = { add_core = 100 }";
    let id = open(
        &mut host,
        "common/scripted_effects/unknown-overload.txt",
        source,
    );
    let position = source.find("add_core =").unwrap() as u32 + "add_core =".len() as u32;
    let items = complete(&host.snapshot(), &id, position).items;
    for label in ["100", "FRA"] {
        assert!(
            items.iter().any(|item| item.label == label),
            "an ambiguous entry scope must preserve both overloads: {items:?}"
        );
    }
}

#[test]
fn ir_scope_union_completions_check_link_origins_and_register_targets() {
    let mut host = host();
    open(
        &mut host,
        "map/positions.txt",
        "100 = { position = { 0 0 } }",
    );
    open(
        &mut host,
        "common/country_tags/union-completions.txt",
        "FRA = \"countries/France.txt\"",
    );
    for (path, source, expected, rejected) in [
        (
            "events/union-completions.txt",
            "country_event = { id = phase5.union immediate = { add_core = } }",
            vec!["100", "capital", "capital_scope", "FROM"],
            vec![
                "FRA",
                "home_province",
                "location",
                "sea_zone",
                "owner",
                "THIS",
                "ROOT",
            ],
        ),
        (
            "events/province-union-completions.txt",
            "province_event = { id = phase5.province_union immediate = { add_core = } }",
            vec!["FRA", "owner", "controller", "emperor", "FROM"],
            vec![
                "100",
                "capital",
                "capital_scope",
                "home_province",
                "location",
                "THIS",
                "ROOT",
            ],
        ),
        (
            "common/scripted_effects/union-completions.txt",
            "select = { add_prestige = 1 add_core = 100 }",
            vec!["100", "capital", "capital_scope", "FROM"],
            vec![
                "FRA",
                "home_province",
                "location",
                "sea_zone",
                "owner",
                "THIS",
            ],
        ),
    ] {
        let id = open(&mut host, path, source);
        let position = source.find("add_core =").unwrap() as u32 + "add_core =".len() as u32;
        let items = complete(&host.snapshot(), &id, position).items;
        for label in expected {
            assert!(
                items.iter().any(|item| item.label == label),
                "{path}: missing {label}"
            );
        }
        for label in rejected {
            assert!(
                !items.iter().any(|item| item.label == label),
                "{path}: invalid scope candidate {label}"
            );
        }
    }
}

#[test]
fn ir_completion_keeps_case_collisions_with_different_value_shapes() {
    let mut host = host();
    open(
        &mut host,
        "common/country_tags/collision.txt",
        "TEA = \"countries/Tea.txt\"",
    );
    open(&mut host, "common/tradegoods/collision.txt", "tea = { }");
    let source = "country_event = { id = phase5.collision trigger = { } }";
    let id = open(&mut host, "events/collision.txt", source);
    let items = complete(
        &host.snapshot(),
        &id,
        source.find("trigger = {").unwrap() as u32 + "trigger = { ".len() as u32,
    )
    .items;
    let country = items
        .iter()
        .find(|item| item.label == "TEA")
        .expect("country scope");
    let good = items
        .iter()
        .find(|item| item.label == "tea")
        .expect("trade good trigger");
    assert!(country.insert_text.contains('{'), "{country:?}");
    assert!(!good.insert_text.contains('{'), "{good:?}");
}

#[test]
fn ir_template_value_overloads_use_the_callers_current_scope() {
    let mut host = host();
    open(
        &mut host,
        "map/positions.txt",
        "100 = { position = { 0 0 } }",
    );
    open(
        &mut host,
        "common/country_tags/call-overload.txt",
        "FRA = \"countries/France.txt\"",
    );
    open(
        &mut host,
        "common/scripted_effects/call-overload.txt",
        "core_helper = { add_core = $target$ }",
    );
    for (name, body, invalid) in [
        ("country_valid", "core_helper = { target = 100 }", false),
        ("country_invalid", "core_helper = { target = FRA }", true),
        (
            "province_valid",
            "every_owned_province = { core_helper = { target = FRA } }",
            false,
        ),
        (
            "province_invalid",
            "every_owned_province = { core_helper = { target = 100 } }",
            true,
        ),
    ] {
        let source = format!("country_event = {{ id = phase5.{name} immediate = {{ {body} }} }}");
        let id = open(&mut host, &format!("events/{name}.txt"), &source);
        let items = diagnostics(&host.snapshot(), &id);
        assert_eq!(
            items.iter().any(|d| d.code == DiagnosticCode::InvalidValue
                && d.message.contains("parameter `target`")),
            invalid,
            "{name}: {items:?}"
        );
    }
}

#[test]
fn ir_template_payload_symbols_enter_hir_and_follow_overlay_signature_changes() {
    let mut host = host();
    let writer = open(
        &mut host,
        "common/scripted_effects/payload-symbols.txt",
        "writer = { $payload$ $payload$ }",
    );
    let source = "country_event = { id = phase5.symbols immediate = { writer = { payload = \"set_country_flag = phase5_payload_flag save_event_target_as = phase5_payload_target\" } } }";
    let id = open(&mut host, "events/payload-symbols.txt", source);
    let snapshot = host.snapshot();
    let defs = snapshot.document(&id).unwrap().hir().unwrap().definitions();
    for (kind, name) in [
        ("country_flag", "phase5_payload_flag"),
        ("event_target", "phase5_payload_target"),
    ] {
        assert_eq!(
            defs.iter()
                .filter(|def| def.kind.as_ref() == kind && def.name == name)
                .count(),
            1,
            "{defs:?}"
        );
        let definition = defs.iter().find(|def| def.name == name).unwrap();
        assert_eq!(
            &source[definition.selection_range.start() as usize
                ..definition.selection_range.end() as usize],
            name
        );
    }
    host.apply_document_changes(
        &writer,
        2,
        &[engine::TextChange::full(
            "writer = { custom_tooltip = $payload$ }",
        )],
    )
    .unwrap();
    let snapshot = host.snapshot();
    assert!(
        !snapshot
            .document(&id)
            .unwrap()
            .hir()
            .unwrap()
            .definitions()
            .iter()
            .any(|def| def.name.starts_with("phase5_payload_"))
    );
}

#[test]
fn ir_replacement_scope_keys_retain_symbols_without_inventing_field_selections() {
    let mut host = host();
    let writer = open(
        &mut host,
        "common/scripted_effects/scope-symbols.txt",
        "writer = { $destination$ = { save_event_target_as = selected_target set_country_flag = $name$_flag } }",
    );
    let snapshot = host.snapshot();
    let hir = snapshot.document(&writer).unwrap().hir().unwrap();
    assert!(
        hir.definitions()
            .iter()
            .any(|d| d.kind.as_ref() == "event_target" && d.name == "selected_target")
    );
    assert!(
        hir.definitions()
            .iter()
            .any(|d| d.kind.as_ref() == "country_flag" && d.name == "$name$_flag")
    );
    let site = hir
        .properties()
        .iter()
        .find(|p| p.key == "$destination$")
        .unwrap();
    assert!(hir.field_fact_at(site.key_range).unwrap().fields.is_empty());
    let other = open(
        &mut host,
        "events/unknown-scope-symbols.txt",
        "country_event = { id = phase5.unknown immediate = { unknown_scope = { save_event_target_as = invalid_target } } }",
    );
    assert!(
        !host
            .snapshot()
            .document(&other)
            .unwrap()
            .hir()
            .unwrap()
            .definitions()
            .iter()
            .any(|d| d.name == "invalid_target")
    );
}

#[test]
fn ir_reviewed_corpus_containers_keep_their_structural_contracts() {
    let mut host = host();
    open(
        &mut host,
        "localisation/reviewed_l_english.yml",
        "l_english:\n phase5_estate:0 \"Estate\"\n",
    );
    open(
        &mut host,
        "common/government_reforms/reviewed.txt",
        "phase5_reform = { } phase5_legacy = { legacy_government = yes }",
    );
    let government = open(
        &mut host,
        "common/governments/reviewed.txt",
        "monarchy = { reform_levels = { arbitrary_level = { reforms = { phase5_reform } } } } pre_dharma_mapping = { phase5_reform = { government = monarchy legacy_government = phase5_legacy } }",
    );
    let snapshot = host.snapshot();
    assert!(diagnostics(&snapshot, &government).is_empty());
    assert!(
        !snapshot
            .document(&government)
            .unwrap()
            .hir()
            .unwrap()
            .definitions()
            .iter()
            .any(|d| d.name == "pre_dharma_mapping")
    );
    let revolt = open(
        &mut host,
        "common/revolt_triggers/reviewed.txt",
        "phase5_revolt = { owner = { tag = F00 } }",
    );
    assert!(diagnostics(&host.snapshot(), &revolt).is_empty());
    let estate = open(
        &mut host,
        "common/estates_preload/reviewed.txt",
        "estate_burghers = { modifier_definition = { type = privileges key = phase5_estate trigger = { always = yes } } }",
    );
    open(
        &mut host,
        "common/estates/reviewed.txt",
        "estate_burghers = { }",
    );
    let estate_items = diagnostics(&host.snapshot(), &estate);
    assert!(estate_items.is_empty(), "{estate_items:?}");
    let effect = open(
        &mut host,
        "common/scripted_effects/reviewed.txt",
        "switch = { trigger_switch = { on_trigger = culture $case$ = { add_prestige = 1 } } change_country_color = { color = { 1 2 3 } } change_country_color = { country = F00 } }",
    );
    let effect_items = diagnostics(&host.snapshot(), &effect);
    assert!(effect_items.is_empty(), "{effect_items:?}");
    let invalid = open(
        &mut host,
        "common/scripted_effects/reviewed-invalid.txt",
        "bad = { change_country_color = { } trigger_switch = { on_trigger = culture } }",
    );
    let items = diagnostics(&host.snapshot(), &invalid);
    assert!(
        items
            .iter()
            .any(|d| d.code == DiagnosticCode::Cardinality && d.message.contains("forms")),
        "{items:?}"
    );
    assert!(
        items
            .iter()
            .any(|d| d.code == DiagnosticCode::Cardinality && d.message.contains("any scalar")),
        "{items:?}"
    );
}

#[test]
fn ir_file_roots_keep_metadata_fields_and_ambient_instance_names() {
    let mut host = host();
    let modifier = open(
        &mut host,
        "common/event_modifiers/metadata.txt",
        "phase5_modifier = { religion = yes secondary_religion = yes picture = phase5_picture }",
    );
    assert!(diagnostics(&host.snapshot(), &modifier).iter().all(|item| {
        !matches!(
            item.code,
            DiagnosticCode::UnknownKey | DiagnosticCode::InvalidValue
        )
    }));
    let ancestor = open(
        &mut host,
        "common/ancestor_personalities/metadata.txt",
        "phase5_ancestor = { }",
    );
    let snapshot = host.snapshot();
    let hir = snapshot.document(&ancestor).unwrap().hir().unwrap();
    assert!(hir.definitions().iter().any(|definition| {
        definition.kind.as_ref() == "ancestor_personality" && definition.name == "phase5_ancestor"
    }));
    let ambient = open(
        &mut host,
        "map/ambient_object.txt",
        "type = { type = eagle use_animation = yes object = { name = phase5_eagle \
         hidden_on_start = no position = { 1 2 3 } rotation = { 0 0 0 } } } \
         type = { type = windmill use_animation = no object = { name = phase5_windmill \
         hidden_on_start = yes position = { 4 5 6 } rotation = { 0 0 0 } } }",
    );
    let snapshot = host.snapshot();
    let hir = snapshot.document(&ambient).unwrap().hir().unwrap();
    for (kind, name) in [
        ("ambient_object_basis", "eagle"),
        ("ambient_object_basis", "windmill"),
        ("ambient_object", "phase5_eagle"),
        ("ambient_object", "phase5_windmill"),
    ] {
        assert!(
            hir.definitions()
                .iter()
                .any(|definition| { definition.kind.as_ref() == kind && definition.name == name }),
            "missing {kind} {name}: {:?}",
            hir.definitions()
        );
    }
    assert!(diagnostics(&snapshot, &ambient).iter().all(|item| {
        !matches!(
            item.code,
            DiagnosticCode::UnknownKey | DiagnosticCode::Cardinality
        )
    }));
}

#[test]
fn ir_symbols_navigate_across_overlays() {
    let mut host = host();
    let target = open(
        &mut host,
        "events/target.txt",
        "country_event = { id = phase4.1 is_triggered_only = yes }",
    );
    let source =
        "country_event = { id = phase4.2 immediate = { country_event = { id = phase4.1 } } }";
    let caller = open(&mut host, "events/caller.txt", source);
    let locations = definition(
        &host.snapshot(),
        &caller,
        source.rfind("phase4.1").unwrap() as u32 + 2,
    );
    assert!(
        locations
            .iter()
            .any(|location| location.document.as_ref() == Some(&target)),
        "{locations:?}"
    );
}

#[test]
fn ir_empty_blocks_offer_schema_fields_and_hover() {
    let mut host = host();
    let source = "country_event = { id = phase4.1 immediate = {  } }";
    let id = open(&mut host, "events/completion.txt", source);
    let position = source.find("  }").unwrap() as u32 + 1;
    let items = complete(&host.snapshot(), &id, position).items;
    assert!(
        items.iter().any(|item| item.label == "add_prestige"),
        "{} items",
        items.len()
    );
    let key_pos = source.find("immediate").unwrap() as u32 + 2;
    let value = hover(&host.snapshot(), &id, key_pos).expect("schema hover");
    assert!(value.contents.contains("block"), "{}", value.contents);
    assert!(value.contents.contains("Source:"), "{}", value.contents);
    assert!(!semantic_tokens(&host.snapshot(), &id).is_empty());
}

#[test]
fn ir_event_subtypes_restrict_on_action_references() {
    let mut host = host();
    open(
        &mut host,
        "events/types.txt",
        "country_event = { id = phase4.country } province_event = { id = phase4.province }",
    );
    let id = open(
        &mut host,
        "common/on_actions/test.txt",
        "on_yearly_pulse = { events = { phase4.province } }",
    );
    let values = diagnostics(&host.snapshot(), &id);
    assert!(
        values
            .iter()
            .any(|diagnostic| diagnostic.code == DiagnosticCode::InvalidValue),
        "{values:#?}"
    );
}

#[test]
fn ir_diagnostics_golden() {
    let mut host = host();
    let source = "country_event = {\n id = phase4.1\n is_triggered_only = maybe\n mystery = yes\n immediate = { if = { add_prestige = 1 } }\n}\n";
    let id = open(&mut host, "events/golden.txt", source);
    let values = diagnostics(&host.snapshot(), &id);
    assert!(
        values
            .iter()
            .any(|value| value.code == DiagnosticCode::InvalidValue)
    );
    assert!(
        values
            .iter()
            .any(|value| value.code == DiagnosticCode::UnknownKey)
    );
    let actual = values
        .iter()
        .map(|value| {
            format!(
                "{} {:?} {}..{} {}{}\n",
                value.code.as_str(),
                value.severity,
                value.range.start(),
                value.range.end(),
                value.message,
                value
                    .expected
                    .as_ref()
                    .map_or(String::new(), |expected| format!(
                        "\n  expected: {expected}"
                    ))
            )
        })
        .collect::<String>();
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/tests/golden/ir_schema_diagnostics.txt");
    if std::env::var_os("PDC_UPDATE_GOLDEN").is_some() {
        std::fs::write(path, actual).unwrap();
    } else {
        assert_eq!(
            std::fs::read_to_string(path).expect("IR golden fixture"),
            actual
        );
    }
}

#[test]
fn changing_ir_rebuilds_existing_overlay_facts() {
    let mut host = host();
    let source = "country_event = { id = phase4.1 immediate = { add_prestige = 1 } }";
    let id = open(&mut host, "events/reload.txt", source);
    let previous = host.snapshot();
    assert!(previous.document(&id).unwrap().hir().unwrap().uses_ir());
    host.set_ir(Arc::new(rules::ir::RulesIr::empty()));
    let current = host.snapshot();
    assert!(!current.document(&id).unwrap().hir().unwrap().uses_ir());
    assert_ne!(previous.ir_fingerprint(), current.ir_fingerprint());
    assert!(previous.document(&id).unwrap().hir().unwrap().uses_ir());
}

#[test]
fn ir_localisation_definitions_navigate_and_preview() {
    let mut host = host();
    let target = open(
        &mut host,
        "localisation/phase4_l_english.yml",
        "l_english:\n phase4.title:0 \"A title\"\n",
    );
    let source =
        "country_event = { id = phase4.1 title = phase4.title option = { name = phase4.title } }";
    let caller = open(&mut host, "events/localised.txt", source);
    let position = source.find("phase4.title").unwrap() as u32 + 2;
    let locations = definition(&host.snapshot(), &caller, position);
    assert!(
        locations
            .iter()
            .any(|location| location.document.as_ref() == Some(&target)),
        "{locations:?}"
    );
    let value = hover(&host.snapshot(), &caller, position).expect("localisation hover");
    assert!(value.contents.contains("A title"), "{}", value.contents);
}

#[test]
fn ir_nested_definitions_and_quoted_ranges_use_actual_source_shape() {
    let mut host = host();
    let source = "series = { slot = 1 potential = {} mission_one = { icon = GFX_unknown trigger = \"always = maybe\" effect = \"add_prestige = 1\" } }";
    let id = open(&mut host, "missions/phase4.txt", source);
    let snapshot = host.snapshot();
    let hir = snapshot.document(&id).unwrap().hir().unwrap();
    assert!(
        hir.definitions()
            .iter()
            .any(|definition| definition.kind.as_ref() == "mission"
                && definition.name == "mission_one"),
        "{:?}",
        hir.definitions()
    );
    let values = diagnostics(&snapshot, &id);
    for name in ["trigger", "effect"] {
        assert!(
            values
                .iter()
                .any(|value| value.code == DiagnosticCode::InvalidValue
                    && value.message.contains(&format!("`{name}` expects a block"))),
            "{values:#?}"
        );
    }
    assert!(
        !hir.references()
            .iter()
            .any(|reference| reference.name == "add_prestige")
    );
}

#[test]
fn ir_empty_rhs_and_template_parameters_complete() {
    let mut host = host();
    let source = "country_event = { id = phase4.1 is_triggered_only =  }";
    let id = open(&mut host, "events/rhs.txt", source);
    let items = complete(
        &host.snapshot(),
        &id,
        source.find("  }").unwrap() as u32 + 1,
    )
    .items;
    assert!(items.iter().any(|item| item.label == "yes"), "{items:?}");
    open(
        &mut host,
        "common/scripted_effects/phase4.txt",
        "phase4_effect = { add_prestige = $AMOUNT$ }",
    );
    let source = "country_event = { id = phase4.1 immediate = { phase4_effect = {  } } }";
    let id = open(&mut host, "events/call.txt", source);
    let items = complete(
        &host.snapshot(),
        &id,
        source.find("  }").unwrap() as u32 + 1,
    )
    .items;
    assert!(items.iter().any(|item| item.label == "AMOUNT"), "{items:?}");
}

#[test]
fn ir_file_roots_keep_country_tag_definitions_and_bare_graphical_culture_completion() {
    let mut host = host();
    let tags = open(
        &mut host,
        "common/country_tags/test.txt",
        "ABC = \"countries/Abc.txt\"\nF0",
    );
    let snapshot = host.snapshot();
    assert!(
        snapshot
            .document(&tags)
            .unwrap()
            .hir()
            .unwrap()
            .definitions()
            .iter()
            .any(|def| { def.kind.as_ref() == "country_tag" && def.name == "ABC" })
    );
    let items = complete(
        &snapshot,
        &tags,
        snapshot.document(&tags).unwrap().text().len() as u32,
    )
    .items;
    assert_eq!(
        items
            .iter()
            .find(|item| item.label == "F00")
            .expect("built-in tag")
            .insert_text,
        "F00 = \"$0\""
    );
    let cultures = open(&mut host, "common/graphicalculturetype.txt", "");
    let items = complete(&host.snapshot(), &cultures, 0).items;
    assert_eq!(items.len(), 12);
    assert_eq!(
        items
            .iter()
            .find(|item| item.label == "westerngfx")
            .unwrap()
            .insert_text,
        "westerngfx"
    );
}

#[test]
fn ir_map_files_select_their_own_definition_namespace() {
    let mut host = host();
    for (file, kind, source) in [
        ("area.txt", "area", "test_area = { 1 }"),
        ("continent.txt", "continent", "test_continent = { 1 }"),
        ("provincegroup.txt", "province_group", "test_group = { 1 }"),
        (
            "region.txt",
            "region",
            "test_region = { areas = { test_area } }",
        ),
        (
            "superregion.txt",
            "superregion",
            "test_superregion = { test_region }",
        ),
        ("positions.txt", "province_id", "1 = { position = { 0 0 } }"),
    ] {
        let id = open(&mut host, &format!("map/{file}"), source);
        let snapshot = host.snapshot();
        let defs = snapshot.document(&id).unwrap().hir().unwrap().definitions();
        assert_eq!(defs.len(), 1, "{file}: {defs:?}");
        assert_eq!(defs[0].kind.as_ref(), kind, "{file}");
    }
}

#[test]
fn ir_file_named_war_definitions_keep_the_filename_identity() {
    let mut host = host();
    for (file, source) in [
        ("phase5-war", "name = \"Display Title\""),
        ("phase5-unnamed", ""),
    ] {
        let id = open(&mut host, &format!("history/wars/{file}.txt"), source);
        let snapshot = host.snapshot();
        let defs = snapshot.document(&id).unwrap().hir().unwrap().definitions();
        assert_eq!(defs.len(), 1, "{defs:?}");
        assert_eq!(defs[0].kind.as_ref(), "war_history");
        assert_eq!(defs[0].name, file);
        assert!(!defs.iter().any(|def| def.name == "Display Title"));
    }
}

#[test]
fn ir_country_tags_validate_against_definitions_and_runtime_seeds() {
    let mut host = host();
    open(
        &mut host,
        "common/country_tags/test.txt",
        "FRA = \"countries/France.txt\"",
    );
    let id = open(
        &mut host,
        "history/provinces/1 - Test.txt",
        "owner = FRA controller = F00 add_core = FRA",
    );
    assert!(
        !diagnostics(&host.snapshot(), &id)
            .iter()
            .any(|value| { value.code == DiagnosticCode::InvalidValue })
    );
    let id = open(
        &mut host,
        "history/provinces/2 - Invalid.txt",
        "owner = name",
    );
    assert!(
        diagnostics(&host.snapshot(), &id)
            .iter()
            .any(|value| { value.code == DiagnosticCode::InvalidValue }),
        "metadata keys must never become country tags"
    );
}

#[test]
fn ir_value_definitions_record_one_payload_symbol_per_write() {
    let mut host = host();
    let source =
        "test = { set_ruler_flag = first set_ruler_flag = first set_country_flag = second }";
    let id = open(
        &mut host,
        "common/scripted_effects/value-definitions.txt",
        source,
    );
    let snapshot = host.snapshot();
    let hir = snapshot.document(&id).unwrap().hir().unwrap();
    for (kind, name, count) in [("ruler_flag", "first", 2), ("country_flag", "second", 1)] {
        let definitions = hir
            .definitions()
            .iter()
            .filter(|def| def.kind.as_ref() == kind)
            .collect::<Vec<_>>();
        assert_eq!(definitions.len(), count, "{definitions:#?}");
        assert!(definitions.iter().all(|def| def.name == name
            && &source[usize::try_from(def.selection_range.start()).unwrap()
                ..usize::try_from(def.selection_range.end()).unwrap()]
                == name));
    }
    assert!(
        !hir.definitions()
            .iter()
            .any(|def| def.name.starts_with("set_"))
    );
}

#[test]
fn ir_reform_and_rebel_files_do_not_define_unrelated_root_kinds() {
    let mut host = host();
    for (path, kind) in [
        ("common/government_reforms/test.txt", "government_reform"),
        ("common/rebel_types/test.txt", "rebel_type"),
    ] {
        let id = open(&mut host, path, "fixture_name = {}");
        let snapshot = host.snapshot();
        let defs = snapshot.document(&id).unwrap().hir().unwrap().definitions();
        assert_eq!(defs.len(), 1, "{path}: {defs:?}");
        assert_eq!(defs[0].kind.as_ref(), kind, "{path}");
    }
}

#[test]
fn ir_enum_literals_are_validated_without_workspace_reference_errors() {
    let mut host = host();
    for (path, source) in [
        (
            "events/months.txt",
            "country_event = { id = enum.1 mean_time_to_happen = { months = 3 } option = { name = enum.option } }",
        ),
        ("common/alerts.txt", "sound = { HIGH = new_alert }"),
    ] {
        let id = open(&mut host, path, source);
        assert!(
            !diagnostics(&host.snapshot(), &id)
                .iter()
                .any(|value| { value.code == DiagnosticCode::InvalidValue }),
            "{}: {:?}",
            path,
            diagnostics(&host.snapshot(), &id)
        );
    }
    let mut invalid_host = self::host();
    let id = open(
        &mut invalid_host,
        "common/alerts.txt",
        "sound = { INVALID_LEVEL = new_alert }",
    );
    assert!(
        diagnostics(&invalid_host.snapshot(), &id)
            .iter()
            .any(|value| {
                value.code == DiagnosticCode::UnknownKey
                    || value.code == DiagnosticCode::InvalidValue
            }),
        "undeclared enum rows remain rejected"
    );
}

#[test]
fn ir_inherited_scope_switch_does_not_require_the_outer_branch_guard() {
    let mut host = host();
    open(
        &mut host,
        "common/country_tags/test.txt",
        "FRA = \"countries/France.txt\"",
    );
    let id = open(
        &mut host,
        "events/scope-guard.txt",
        "country_event = { id = scope.1 immediate = { if = { limit = { always = yes } FRA = { add_prestige = 1 } } } option = { name = scope.option } }",
    );
    let values = diagnostics(&host.snapshot(), &id);
    assert!(
        !values
            .iter()
            .any(|value| value.code == DiagnosticCode::Cardinality),
        "{values:?}"
    );
}

#[test]
fn ir_file_containers_expose_nested_technology_and_custom_idea_definitions() {
    let mut host = host();
    for (path, kind, name, source) in [
        (
            "common/technology.txt",
            "technology_group",
            "western",
            "groups = { western = { start_level = 3 start_cost_modifier = 0 } }",
        ),
        (
            "common/custom_ideas/test.txt",
            "customideas",
            "custom_test",
            "idea_modifiers = { category = ADM custom_test = { discipline = 0.05 } }",
        ),
    ] {
        let id = open(&mut host, path, source);
        let snapshot = host.snapshot();
        let defs = snapshot.document(&id).unwrap().hir().unwrap().definitions();
        assert!(
            defs.iter()
                .any(|def| def.kind.as_ref() == kind && def.name == name),
            "{path}: {defs:?}"
        );
    }
}

#[test]
fn ir_inherited_effect_schemas_keep_scope_and_template_patterns() {
    let mut host = host();
    open(
        &mut host,
        "common/country_tags/test.txt",
        "FRA = \"countries/France.txt\"",
    );
    open(
        &mut host,
        "common/scripted_effects/test.txt",
        "phase5_effect = { add_prestige = 1 }",
    );
    let source = "country_event = { id = phase5.1 option = { name = phase5.option FRA = { phase5_effect = yes } } }";
    let id = open(&mut host, "events/inherited.txt", source);
    let snapshot = host.snapshot();
    let values = diagnostics(&snapshot, &id);
    assert!(
        !values
            .iter()
            .any(|value| value.code == DiagnosticCode::UnknownKey),
        "{values:?}"
    );
    let hir = snapshot.document(&id).unwrap().hir().unwrap();
    assert!(
        hir.references().iter().any(|reference| {
            reference.kind.as_ref() == "scripted_effect" && reference.name == "phase5_effect"
        }),
        "{:?}",
        hir.references()
    );
}

#[test]
fn ir_repeatable_event_and_effect_entries_remain_available_with_scope_filtered_keys() {
    let mut host = host();
    let source = "country_event = { id = phase5.1 option = { name = phase5.option add_prestige = 1 add_prestige = 2  } }\n";
    let id = open(&mut host, "events/repeat.txt", source);
    let snapshot = host.snapshot();
    assert!(
        !diagnostics(&snapshot, &id)
            .iter()
            .any(|value| value.code == DiagnosticCode::Cardinality)
    );
    let items = complete(&snapshot, &id, source.find("  }").unwrap() as u32 + 1).items;
    assert!(items.iter().any(|item| item.label == "add_prestige"));
    assert!(
        !items.iter().any(|item| item.label == "add_prosperity"),
        "province-only effect"
    );
    assert!(
        complete(&snapshot, &id, source.len() as u32)
            .items
            .iter()
            .any(|item| { item.label == "country_event" }),
        "event files can declare another event"
    );
}

#[test]
fn ir_queries_respect_cancellation() {
    let mut host = host();
    let id = open(
        &mut host,
        "events/cancel.txt",
        "country_event = { id = phase4.1 }",
    );
    let token = crate::CancellationToken::new();
    token.cancel();
    assert!(crate::diagnostics_with_cancellation(&host.snapshot(), &id, &token).is_err());
    assert!(crate::complete_with_cancellation(&host.snapshot(), &id, 0, &token).is_err());
}

#[test]
fn ir_template_arguments_follow_definition_value_constraints() {
    let mut host = host();
    open(
        &mut host,
        "common/scripted_effects/typed.txt",
        "phase4_effect = { add_prestige = $AMOUNT$ }",
    );
    let source =
        "country_event = { id = phase4.1 immediate = { phase4_effect = { AMOUNT = bad } } }";
    let id = open(&mut host, "events/typed-call.txt", source);
    let start = source.find("bad").unwrap() as u32;
    let values = diagnostics(&host.snapshot(), &id);
    assert!(values.iter().any(|value| value.code == DiagnosticCode::InvalidValue && value.range.start() == start), "{values:#?}");
}

#[test]
fn ir_event_hover_cards_use_definition_and_field_facts() {
    use engine::{SourceRoot, SourceRootId, SourceRootKind, WorkspaceChange};
    use text::AbsPath;
    let root = super::support::temp_root("ir-event-card");
    std::fs::create_dir_all(root.join("interface")).unwrap();
    std::fs::create_dir_all(root.join("gfx")).unwrap();
    std::fs::create_dir_all(root.join("localisation")).unwrap();
    std::fs::write(root.join("gfx/card.dds"), b"").unwrap();
    std::fs::write(
        root.join("interface/card.gfx"),
        "spriteTypes = {
 spriteType = {
 name = GFX_event_bg_top
 texturefile = \"gfx/card.dds\"
 }
 }",
    )
    .unwrap();
    std::fs::write(
        root.join("localisation/card_l_english.yml"),
        "l_english:\n phase4.title:0 \"Card title\"\n",
    )
    .unwrap();
    let mut host = host();
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(1),
        SourceRootKind::Project,
        AbsPath::normalize(&root),
    )]));
    host.refresh_source_roots().unwrap();
    let id = open(
        &mut host,
        "events/card.txt",
        "country_event = { id = phase4.1 title = phase4.title option = { name = phase4.title } }",
    );
    let snapshot = host.snapshot();
    assert!(
        !snapshot
            .index()
            .definitions("sprite", "GFX_event_bg_top")
            .is_empty(),
        "sprite definition missing"
    );
    assert!(
        snapshot.resolve_texture_path("gfx/card.dds").is_some(),
        "texture missing"
    );
    let card = crate::hover_card_with_cancellation(
        &host.snapshot(),
        &id,
        3,
        &crate::CancellationToken::new(),
    )
    .unwrap()
    .expect("event card");
    let event = card.event.unwrap();
    assert_eq!(event.id, "phase4.1");
    assert_eq!(event.title_key.as_deref(), Some("phase4.title"));
    assert_eq!(
        event.title.map(|(_, value)| value).as_deref(),
        Some("Card title")
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn ir_known_keys_report_value_shape_and_rule_source() {
    let mut host = host();
    let source = "country_event = { id = phase4.1 is_triggered_only = { } option = { name = phase4.title } }";
    let id = open(&mut host, "events/shape.txt", source);
    let values = diagnostics(&host.snapshot(), &id);
    let diagnostic = values
        .iter()
        .find(|diagnostic| {
            diagnostic.code == DiagnosticCode::InvalidValue
                && diagnostic.message.contains("scalar value")
        })
        .expect("known field has the wrong shape");
    let source = diagnostic
        .provenance
        .as_ref()
        .expect("IR diagnostic source");
    assert_eq!(source.source_file.as_deref(), Some("events.json"));
    assert!(
        source
            .source_pointer
            .as_ref()
            .is_some_and(|pointer| pointer.contains("is_triggered_only"))
    );
    assert!(
        !values
            .iter()
            .any(|diagnostic| diagnostic.code == DiagnosticCode::UnknownKey)
    );
}

#[test]
fn ir_pattern_cardinality_counts_each_instance_key() {
    let mut ir = (*game::eu4::first_party_ir().unwrap()).clone();
    let schema = ir.schema_by_name("mission_series_body").unwrap();
    let field = ir
        .lookup(schema, "one", rules::ir::Shape::Block)
        .find(|id| {
            matches!(
                ir.matcher(ir.field(*id).key),
                rules::ir::Matcher::Def { .. }
            )
        })
        .unwrap();
    ir.fields[field.index()].card.max = Some(1);
    let catalog = rules::RuleSet::from_ir_catalog(&ir);
    let mut host = AnalysisHost::with_ir(catalog, ir.game.profile.clone(), Arc::new(ir));
    let id = open(
        &mut host,
        "missions/unique.txt",
        "series = { one = { } two = { } }",
    );
    assert!(
        !diagnostics(&host.snapshot(), &id)
            .iter()
            .any(|diagnostic| diagnostic.code == DiagnosticCode::Cardinality
                && diagnostic.message.contains("may occur"))
    );
    let id = open(
        &mut host,
        "missions/duplicate.txt",
        "series = { one = { } one = { } }",
    );
    assert!(
        diagnostics(&host.snapshot(), &id)
            .iter()
            .any(|diagnostic| diagnostic.code == DiagnosticCode::Cardinality
                && diagnostic.message.contains("`one` may occur"))
    );
}

#[test]
fn ir_cached_cardinality_groups_apply_bounds_to_the_current_scope() {
    let mut ir = (*game::eu4::first_party_ir().unwrap()).clone();
    let schema = ir.schema_by_name("effect").unwrap();
    let fields = ir
        .lookup(schema, "add_core", rules::ir::Shape::Scalar)
        .collect::<Vec<_>>();
    let mut bounded = 0;
    let mut unbounded = 0;
    for id in fields {
        let country = ir.field(id).scope.as_ref().is_some_and(|scope| {
            scope
                .scopes_in
                .iter()
                .any(|name| ir.strings().resolve(*name) == "country")
        });
        if country {
            ir.fields[id.index()].card.min = 1;
            ir.fields[id.index()].card.max = Some(1);
            bounded += 1;
        } else {
            ir.fields[id.index()].card.min = 0;
            ir.fields[id.index()].card.max = None;
            unbounded += 1;
        }
    }
    assert!(bounded > 0 && unbounded > 0);
    let catalog = rules::RuleSet::from_ir_catalog(&ir);
    let mut host = AnalysisHost::with_ir(catalog, ir.game.profile.clone(), Arc::new(ir));
    for (path, source, expected) in [
        (
            "events/country-cardinality.txt",
            "country_event = { id = card.1 immediate = { add_core = 100 add_core = 100 } }",
            Some("may occur at most"),
        ),
        (
            "events/province-cardinality.txt",
            "province_event = { id = card.2 immediate = { add_core = FRA add_core = FRA } }",
            None,
        ),
        (
            "events/required-country.txt",
            "country_event = { id = card.3 immediate = { add_prestige = 1 } }",
            Some("missing required key"),
        ),
        (
            "events/optional-province.txt",
            "province_event = { id = card.4 immediate = { add_base_tax = 1 } }",
            None,
        ),
    ] {
        let id = open(&mut host, path, source);
        let values = diagnostics(&host.snapshot(), &id);
        let core = values
            .iter()
            .filter(|diagnostic| {
                diagnostic.code == DiagnosticCode::Cardinality
                    && diagnostic.message.contains("`add_core`")
            })
            .collect::<Vec<_>>();
        if let Some(expected) = expected {
            assert_eq!(core.len(), 1, "{path}: {values:?}");
            assert!(core[0].message.contains(expected), "{path}: {core:?}");
        } else {
            assert!(core.is_empty(), "{path}: {core:?}");
        }
    }
}

#[test]
fn ir_control_chains_and_value_branches_follow_field_declarations() {
    let mut host = host();
    let source = "country_event = { id = phase4.1 option = { name = phase4.title } immediate = { if = { limit = { always = yes } add_prestige = 1 } else_if = { limit = { always = no } add_prestige = 2 } else = { add_prestige = 3 } trigger_switch = { on_trigger = always yes = { add_prestige = 1 } no = { add_prestige = 2 } } random_list = { 10 = { add_prestige = 1 } 20 = { add_prestige = 2 } } } }";
    let id = open(&mut host, "events/control.txt", source);
    let values = diagnostics(&host.snapshot(), &id);
    assert!(
        !values.iter().any(|diagnostic| matches!(
            diagnostic.code,
            DiagnosticCode::OrphanElse
                | DiagnosticCode::UnknownKey
                | DiagnosticCode::InvalidValue
                | DiagnosticCode::Cardinality
        )),
        "{values:#?}"
    );
    let source = "country_event = { id = phase4.2 immediate = { trigger_switch = { on_trigger = always maybe = { add_prestige = 1 } no = { add_prestige = 2 } } } }";
    let id = open(&mut host, "events/invalid-switch.txt", source);
    assert!(
        diagnostics(&host.snapshot(), &id)
            .iter()
            .any(|diagnostic| diagnostic.code == DiagnosticCode::InvalidValue
                && diagnostic.range.start() == source.find("maybe").unwrap() as u32)
    );
}

#[test]
fn ir_switch_selectors_follow_schema_ids_after_schema_renaming() {
    let mut ir = (*game::eu4::first_party_ir().unwrap()).clone();
    let selector = ir.schema_by_name("trigger").unwrap();
    ir.schemas[selector.index()].name = ir.strings.intern_folded("predicate_vocabulary");
    let ir = rules::ir::RulesIr::from_baked(&serde_json::to_vec(&ir).unwrap()).unwrap();
    assert!(ir.schema_by_name("trigger").is_none());
    assert_eq!(ir.schema_by_name("predicate_vocabulary"), Some(selector));
    let catalog = rules::RuleSet::from_ir_catalog(&ir);
    let mut host = AnalysisHost::with_ir(catalog, ir.game.profile.clone(), Arc::new(ir));
    let source = "country_event = { id = phase5.selector immediate = { trigger_switch = { on_trigger = always yes = { add_prestige = 1 } maybe = { add_prestige = 2 } } } }";
    let id = open(&mut host, "events/selector.txt", source);
    let values = diagnostics(&host.snapshot(), &id);
    let invalid = values
        .iter()
        .filter(|d| d.code == DiagnosticCode::InvalidValue)
        .collect::<Vec<_>>();
    assert_eq!(invalid.len(), 1, "{values:#?}");
    assert_eq!(
        invalid[0].range.start(),
        source.find("maybe").unwrap() as u32
    );
}

#[test]
fn ir_switch_parameter_keys_use_the_selected_predicate_value_domain() {
    let mut host = host();
    open(
        &mut host,
        "common/scripted_effects/parameter-switch.txt",
        "phase5_switch = { trigger_switch = { [[selector] on_trigger = $selector$ ] [[case] $case$ = { add_prestige = 1 } ] } }",
    );
    for value in ["yes", "no", "maybe"] {
        let source = format!(
            "country_event = {{ id = phase5.switch immediate = {{ phase5_switch = {{ selector = always case = {value} }} }} }}"
        );
        let id = open(&mut host, &format!("events/switch-{value}.txt"), &source);
        let invalid = diagnostics(&host.snapshot(), &id)
            .into_iter()
            .filter(|d| d.code == DiagnosticCode::InvalidValue)
            .collect::<Vec<_>>();
        assert_eq!(invalid.is_empty(), value != "maybe", "{value}: {invalid:?}");
    }
    let source = "country_event = { id = phase5.switch immediate = { phase5_switch = { selector = always case =  } } }";
    let id = open(&mut host, "events/switch-completion.txt", source);
    let items = complete(
        &host.snapshot(),
        &id,
        source.find("  }").unwrap() as u32 + 1,
    )
    .items;
    let values = items
        .iter()
        .map(|item| item.label.as_str())
        .collect::<Vec<_>>();
    assert_eq!(values, ["no", "yes"], "{items:?}");
}

#[test]
fn ir_scope_values_use_the_enclosing_register_state() {
    let mut host = host();
    let source = "country_event = { id = phase4.1 option = { name = phase4.title } immediate = { every_owned_province = { add_core = ROOT } } }";
    let id = open(&mut host, "events/scope-value.txt", source);
    let values = diagnostics(&host.snapshot(), &id);
    assert!(
        !values.iter().any(|diagnostic| matches!(
            diagnostic.code,
            DiagnosticCode::InvalidValue | DiagnosticCode::WrongScope
        )),
        "{values:#?}"
    );
    let source = "country_event = { id = phase4.2 immediate = { every_owned_province = { add_core = THIS } } }";
    let id = open(&mut host, "events/invalid-scope-value.txt", source);
    assert!(
        diagnostics(&host.snapshot(), &id)
            .iter()
            .any(|diagnostic| diagnostic.code == DiagnosticCode::WrongScope
                && diagnostic.range.start() == source.find("THIS").unwrap() as u32)
    );
}

#[test]
fn ir_block_forms_preserve_alternative_variable_operands() {
    let mut host = host();
    for (name, source) in [
        (
            "named",
            "set_variable = { which = lhs value = 1 } change_variable = { which = lhs value = 2 }",
        ),
        (
            "two-vars",
            "set_variable = { which = lhs value = 1 } set_variable = { which = rhs value = 2 } change_variable = { which = lhs which = rhs }",
        ),
        (
            "shorthand",
            "set_variable = { lhs = 1 } change_variable = { lhs = 2 }",
        ),
    ] {
        let id = open(
            &mut host,
            &format!("common/scripted_effects/{name}.txt"),
            &format!("test = {{ {source} }}"),
        );
        let values = diagnostics(&host.snapshot(), &id);
        assert!(
            !values.iter().any(|diagnostic| matches!(
                diagnostic.code,
                DiagnosticCode::Cardinality
                    | DiagnosticCode::UnknownKey
                    | DiagnosticCode::InvalidValue
            )),
            "{name}: {values:#?}"
        );
    }
    for (index, source) in [
        "",
        "which = lhs",
        "which = lhs which = rhs value = 1",
        "which = lhs value = 1 other = 2",
        "one = 1 two = 2 three = 3",
    ]
    .into_iter()
    .enumerate()
    {
        let id = open(
            &mut host,
            &format!("common/scripted_effects/invalid-{index}.txt"),
            &format!("test = {{ change_variable = {{ {source} }} }}"),
        );
        assert!(
            diagnostics(&host.snapshot(), &id)
                .iter()
                .any(|diagnostic| diagnostic.code == DiagnosticCode::Cardinality
                    && diagnostic.message.contains("forms")),
            "invalid form accepted: {source}"
        );
    }
}

#[test]
fn ir_block_forms_preserve_estate_loyalty_alternatives() {
    let mut host = host();
    for (index, source) in ["loyalty = 30", "higher_than_influence = yes"]
        .into_iter()
        .enumerate()
    {
        let id = open(
            &mut host,
            &format!("common/scripted_triggers/estate-valid-{index}.txt"),
            &format!("test = {{ estate_loyalty = {{ estate = all {source} }} }}"),
        );
        assert!(
            !diagnostics(&host.snapshot(), &id)
                .iter()
                .any(|diagnostic| diagnostic.code == DiagnosticCode::Cardinality),
            "valid form rejected: {source}"
        );
    }
    for (index, source) in ["", "loyalty = 30 higher_than_influence = yes"]
        .into_iter()
        .enumerate()
    {
        let id = open(
            &mut host,
            &format!("common/scripted_triggers/estate-invalid-{index}.txt"),
            &format!("test = {{ estate_loyalty = {{ estate = all {source} }} }}"),
        );
        assert!(
            diagnostics(&host.snapshot(), &id)
                .iter()
                .any(|diagnostic| diagnostic.code == DiagnosticCode::Cardinality
                    && diagnostic.message.contains("forms")),
            "invalid form accepted: {source}"
        );
    }
    let id = open(
        &mut host,
        "common/scripted_triggers/estate.txt",
        "test = { estate_loyalty = { loyalty = 30 } }",
    );
    assert!(
        diagnostics(&host.snapshot(), &id)
            .iter()
            .any(|diagnostic| diagnostic.code == DiagnosticCode::Cardinality
                && diagnostic.message.contains("`estate`")),
        "unconditional required field was weakened"
    );
}

#[test]
fn template_scope_summary_retains_guards_and_actual_calls_activate_them() {
    let mut host = host();
    let definitions = "guarded_scope = { add_prestige = 1 [[P] change_province_name = \"X\" ] }";
    let id = open(
        &mut host,
        "common/scripted_effects/guarded-scope.txt",
        definitions,
    );
    assert!(
        !diagnostics(&host.snapshot(), &id)
            .iter()
            .any(|item| item.code == DiagnosticCode::EmptyScopeContract)
    );
    let guard = definitions.find("[[P]").unwrap() as u32 + 2;
    let info = hover(&host.snapshot(), &id, guard).unwrap();
    assert!(info.contents.contains("optional"), "{info:?}");
    for (name, argument, invalid) in [
        ("inactive", "", false),
        ("active", "P = yes", true),
        ("hole", "P =", true),
    ] {
        let source = format!(
            "country_event = {{ id = guard.{name} immediate = {{ guarded_scope = {{ {argument} }} }} option = {{ name = guard.{name} }} }}"
        );
        let caller = open(
            &mut host,
            &format!("events/guard-scope-{name}.txt"),
            &source,
        );
        let values = diagnostics(&host.snapshot(), &caller);
        assert_eq!(
            values
                .iter()
                .any(|item| item.code == DiagnosticCode::WrongScope),
            invalid,
            "{name}: {values:?}"
        );
    }
}

#[test]
fn template_fixed_callee_errors_are_checked_at_the_outer_call() {
    let mut host = host();
    open(
        &mut host,
        "common/scripted_effects/fixed-callee.txt",
        "bad_leaf = { add_prestige = wrong } outer_fixed = { bad_leaf = yes }",
    );
    let source = "country_event = { id = fixed.1 immediate = { outer_fixed = yes } option = { name = fixed.1 } }";
    let id = open(&mut host, "events/fixed-callee.txt", source);
    let values = diagnostics(&host.snapshot(), &id);
    assert!(
        values
            .iter()
            .any(|value| value.code == DiagnosticCode::InvalidValue
                && value.message.contains("add_prestige")),
        "{values:?}"
    );
}

fn overload_host() -> AnalysisHost {
    super::support::fixture_host(serde_json::json!({
        "traits":{"Template":{}},"types":{"fixture_template":{"impl":{"Template":{"body":"effect"}},"resolution":"replace"},"flag_a":{},"flag_b":{}},
        "schemas":{
            "fixture_root":{"fields":{"__templates":{"body":"definitions","card":"0..*"}}},
            "definitions":{"map":{"key":"def<fixture_template>","body":"effect"}},
            "arguments":{"map":{"key":"scalar","value":"scalar"}},
            "effect":{"fields":{"pick":[
                {"body":"country_variant","card":"0..*","scope":{"in":["country"],"push":"country"}},
                {"body":"province_variant","card":"0..*","scope":{"in":["country"],"push":"province"}}
            ]},"patterns":[{"key":"ref<fixture_template>","body":"arguments","card":"0..*"}]},
            "country_variant":{"fields":{"a":{"value":"bool","card":"1","scope":{"in":["country"]}},"flag":{"value":"def<flag_a>","card":"0..1"}}},
            "province_variant":{"fields":{"b":{"value":"bool","card":"1","scope":{"in":["province"]}},"flag":{"value":"def<flag_b>","card":"0..1"}}}
        }
    }))
}

#[test]
fn complete_block_selects_one_consistent_overload_scope_and_symbol_namespace() {
    let mut host = overload_host();
    let source = "country_event = { immediate = { pick = { b = yes flag = chosen } } }";
    let id = open(&mut host, "events/overload.txt", source);
    let snapshot = host.snapshot();
    let input = snapshot.document(&id).unwrap().hir().unwrap();
    assert!(
        !diagnostics(&snapshot, &id).iter().any(|item| matches!(
            item.code,
            DiagnosticCode::InvalidValue
                | DiagnosticCode::UnknownKey
                | DiagnosticCode::WrongScope
                | DiagnosticCode::Cardinality
        )),
        "{:?}",
        diagnostics(&snapshot, &id)
    );
    assert!(
        input
            .definitions()
            .iter()
            .any(|definition| definition.kind.as_ref() == "flag_b" && definition.name == "chosen")
    );
    assert!(
        !input
            .definitions()
            .iter()
            .any(|definition| definition.kind.as_ref() == "flag_a")
    );
    let b = source.find("b =").unwrap() as u32;
    assert_eq!(
        input
            .scope_fact_at(text::TextRange::new(b, b + 1).unwrap())
            .unwrap()
            .state
            .current
            .first(),
        Some(&hir::ScopeValue::known_single("province"))
    );
    assert_eq!(
        input.overload_facts()[0].validation,
        hir::analysis::Validation::Valid
    );
}

#[test]
fn template_block_overloads_cannot_mix_children_or_leak_speculative_facts() {
    let mut host = overload_host();
    open(
        &mut host,
        "definitions.txt",
        "__templates = { wrap = { $BODY$ } }",
    );
    let valid = "country_event = { immediate = { wrap = { BODY = \"pick = { b = yes flag = chosen }\" } } }";
    let id = open(&mut host, "events/template-overload.txt", valid);
    let values = diagnostics(&host.snapshot(), &id);
    assert!(
        !values.iter().any(|item| matches!(
            item.code,
            DiagnosticCode::InvalidValue
                | DiagnosticCode::UnknownKey
                | DiagnosticCode::WrongScope
                | DiagnosticCode::Cardinality
        )),
        "{values:?}"
    );
    let view = host.snapshot();
    let result = complete(&view, &id, valid.find("b = yes").unwrap() as u32 + 5);
    let yes = result
        .items
        .iter()
        .find(|item| item.label == "yes")
        .expect("valid bool candidate");
    let proof = yes
        .template_evidence
        .as_ref()
        .expect("Template candidate evidence");
    let effect = view.ir().schema_by_name("effect").unwrap();
    let selected = view
        .ir()
        .lookup(effect, "pick", rules::ir::Shape::Block)
        .nth(1)
        .unwrap();
    assert!(
        proof
            .interpretations
            .iter()
            .any(|proof| proof.schema == effect.index()
                && proof.fields == vec![selected.index()]
                && !proof.conditional),
        "{proof:?}"
    );
    let invalid =
        "country_event = { immediate = { pick = { a = yes b = yes flag = speculative } } }";
    let id = open(&mut host, "events/mixed-overload.txt", invalid);
    let snapshot = host.snapshot();
    let input = snapshot.document(&id).unwrap().hir().unwrap();
    assert_eq!(
        input.overload_facts()[0].validation,
        hir::analysis::Validation::Invalid
    );
    assert!(
        diagnostics(&snapshot, &id)
            .iter()
            .any(|item| item.code == DiagnosticCode::InvalidValue
                && item.message.contains("no rule overload"))
    );
    assert!(
        !input
            .definitions()
            .iter()
            .any(|definition| definition.name == "speculative"),
        "{:?}",
        input.definitions()
    );
}

#[test]
fn unknown_script_prefix_does_not_prove_a_suffix_scope_or_value_error() {
    let mut host = host();
    let definitions = "fragile = { $BODY$ add_prestige = 1 change_province_name = \"X\" } before_hole = { add_prestige = wrong $BODY$ }";
    let id = open(
        &mut host,
        "common/scripted_effects/open-boundary.txt",
        definitions,
    );
    assert!(
        !diagnostics(&host.snapshot(), &id)
            .iter()
            .any(|value| value.code == DiagnosticCode::EmptyScopeContract),
        "{:?}",
        diagnostics(&host.snapshot(), &id)
    );
    let source = "country_event = { id = prefix.1 immediate = { fragile = { BODY = \"# the suffix is a comment\" } before_hole = {} } option = { name = prefix.1 } }";
    let id = open(&mut host, "events/open-boundary.txt", source);
    let values = diagnostics(&host.snapshot(), &id);
    assert!(
        !values
            .iter()
            .any(|value| value.code == DiagnosticCode::WrongScope
                && value.message.contains("fragile")),
        "{values:?}"
    );
    assert!(
        values
            .iter()
            .any(|value| value.code == DiagnosticCode::InvalidValue
                && value.message.contains("before_hole")),
        "{values:?}"
    );
}

#[test]
fn ambiguous_callee_binding_maps_are_not_validated_as_statements() {
    let mut host = host();
    open(
        &mut host,
        "common/scripted_triggers/ambiguous.txt",
        "ambiguous_leaf = { always = $VALUE$ } ambiguous_leaf = { always = $VALUE$ } relay_ambiguous = { ambiguous_leaf = { VALUE = yes } }",
    );
    let source = "country_event = { id = unresolved.1 trigger = { relay_ambiguous = yes } option = { name = unresolved.1 } }";
    let id = open(&mut host, "events/ambiguous-callee.txt", source);
    let values = diagnostics(&host.snapshot(), &id);
    assert!(
        !values.iter().any(
            |value| value.code == DiagnosticCode::UnknownKey && value.message.contains("VALUE")
        ),
        "{values:?}"
    );
    assert!(
        values
            .iter()
            .any(|value| value.code == DiagnosticCode::AnalysisIncomplete),
        "{values:?}"
    );
}

fn script_items_host() -> AnalysisHost {
    super::support::fixture_host(serde_json::json!({
        "traits":{"Template":{}},"types":{"fixture_template":{"impl":{"Template":{"body":"effect"}},"resolution":"replace"}},
        "schemas":{"fixture_root":{"fields":{"__templates":{"body":"definitions","card":"0..*"}}},
            "definitions":{"map":{"key":"def<fixture_template>","body":"effect"}},"arguments":{"map":{"key":"scalar","value":"scalar"}},
            "effect":{"fields":{"numbers":{"list":"float","card":"0..*"}},"patterns":[{"key":"ref<fixture_template>","body":"arguments","card":"0..*"}]}}
    }))
}
#[test]
fn template_items_validate_every_inserted_token_without_treating_the_list_as_one_scalar() {
    let mut host = script_items_host();
    open(
        &mut host,
        "definitions.txt",
        "__templates = { list_wrap = { numbers = { $VALUES$ } } }",
    );
    for (name, value, invalid) in [
        ("quoted", "\"1 2\"", false),
        ("single", "3", false),
        ("bad", "\"1 wrong 2\"", true),
        ("quoted_item", "\"1 \\\"wrong\\\" 2\"", true),
    ] {
        let source =
            format!("country_event = {{ immediate = {{ list_wrap = {{ VALUES = {value} }} }} }}");
        let id = open(&mut host, &format!("events/items-{name}.txt"), &source);
        let values = diagnostics(&host.snapshot(), &id);
        assert_eq!(
            values
                .iter()
                .any(|value| value.code == DiagnosticCode::InvalidValue),
            invalid,
            "{name}: {values:?}"
        );
    }
}
#[test]
fn complete_operand_and_parent_siblings_share_completion_and_hover_context() {
    let mut host = host();
    open(
        &mut host,
        "common/scripted_effects/operand.txt",
        "operand = { if = $BLOCK$ } partial = { if = { limit = { always = yes } $BODY$ } }",
    );
    let source = r#"country_event = { id = operand.1 immediate = { operand = { BLOCK = "{ limit = { always = yes } add_pr }" } } option = { name = operand.1 } }"#;
    let id = open(&mut host, "events/operand.txt", source);
    let items = complete(
        &host.snapshot(),
        &id,
        (source.find("add_pr").unwrap() + 6) as u32,
    );
    assert!(
        items.items.iter().any(|item| item.label == "add_prestige"),
        "{items:?}"
    );
    let source = r#"country_event = { id = partial.1 immediate = { partial = { BODY = "l" } } option = { name = partial.1 } }"#;
    let id = open(&mut host, "events/partial.txt", source);
    let items = complete(
        &host.snapshot(),
        &id,
        (source.find("BODY =").unwrap() + 9) as u32,
    );
    assert!(
        !items.items.iter().any(|item| item.label == "limit"),
        "the fixed sibling already consumes its quota: {items:?}"
    );
    assert!(
        items.items.iter().any(|item| item.label == "log"),
        "{items:?}"
    );
}

#[test]
fn completion_edit_round_trips_nested_carriers_and_literal_quote_values() {
    let mut host = super::support::fixture_host(serde_json::json!({
        "traits":{"Template":{}},"types":{"fixture_template":{"impl":{"Template":{"body":"trigger"}},"resolution":"replace"}},
        "schemas":{"fixture_root":{"fields":{"__templates":{"body":"definitions","card":"0..*"}}},"definitions":{"map":{"key":"def<fixture_template>","body":"trigger"}},
            "arguments":{"map":{"key":"scalar","value":"scalar"}},"trigger":{"fields":{"message":{"value":"'a\"b'","card":"0..*"}},"patterns":[{"key":"ref<fixture_template>","body":"arguments","card":"0..*"}]}}
    }));
    open(
        &mut host,
        "definitions.txt",
        "__templates = { outer = { $BODY$ } inner = { $TEXT$ } }",
    );
    let payload = "message = ";
    let middle = format!(
        "inner = {{ TEXT = \"{}\" }}",
        parser::encode_quoted_script_text(payload)
    );
    let source = format!(
        "trigger = {{ outer = {{ BODY = \"{}\" }} }}",
        parser::encode_quoted_script_text(&middle)
    );
    let id = open(&mut host, "events/nested-carrier.txt", &source);
    let position = (source.find("message = ").unwrap() + "message = ".len()) as u32;
    let result = complete(&host.snapshot(), &id, position);
    let item = result
        .items
        .iter()
        .find(|item| item.label == "a\"b")
        .expect("literal quote candidate");
    let text = if item.is_snippet {
        crate::snippet_plain_text(&item.insert_text)
    } else {
        item.insert_text.clone()
    };
    let mut applied = source.clone();
    applied.replace_range(
        item.replacement_range.start() as usize..item.replacement_range.end() as usize,
        &text,
    );
    let patched = open(&mut host, "events/nested-carrier-applied.txt", &applied);
    let values = diagnostics(&host.snapshot(), &patched);
    assert!(
        !values.iter().any(|value| matches!(
            value.code,
            DiagnosticCode::Syntax | DiagnosticCode::InvalidValue | DiagnosticCode::UnknownKey
        )),
        "{applied}\n{values:?}"
    );
    assert_eq!(
        item.template_evidence.as_ref().unwrap().validation,
        hir::analysis::Validation::Valid
    );
}

#[test]
fn unfinished_fact_discovery_does_not_turn_a_negative_key_lookup_into_an_error() {
    let mut host = super::support::fixture_host(serde_json::json!({
        "types":{"node":{}},
        "schemas":{
            "fixture_root":{"fields":{"seed":{"value":"def<node>","card":"0..*"},"strict":{"body":"strict","card":"0..*"},"independent":{"value":"bool","card":"0..*"}},
                "patterns":[{"key":"ref<node>","body":"known","card":"0..*"},{"key":"scalar","body":"missing","card":"0..*"}]},
            "known":{"fields":{"write":{"value":"scalar","card":"1"}}},
            "missing":{"fields":{"write":{"value":"def<node>","card":"1"}}},
            "strict":{"patterns":[{"key":"ref<node>","body":"known","card":"0..*"}]}
        }
    }));
    let text = "seed = stable toggle = { write = toggle } strict = { pending_key = { write = valid } } independent = wrong";
    let id = open(&mut host, "events/pending-facts.txt", text);
    assert!(
        !host
            .snapshot()
            .document(&id)
            .unwrap()
            .fact_coverage
            .is_known()
    );
    let values = diagnostics(&host.snapshot(), &id);
    assert!(
        values
            .iter()
            .any(|item| item.code == DiagnosticCode::AnalysisIncomplete),
        "{values:?}"
    );
    assert!(
        !values
            .iter()
            .any(|item| item.code == DiagnosticCode::UnknownKey
                && item.message.contains("pending_key")),
        "a miss in unfinished facts is unknown: {values:?}"
    );
    assert!(
        values
            .iter()
            .any(|item| item.code == DiagnosticCode::InvalidValue
                && item.message.contains("independent")),
        "independent rejection must survive: {values:?}"
    );
}

#[test]
fn template_value_rejections_preserve_rule_warning_severity_and_the_actual_value() {
    let mut host = host();
    open(
        &mut host,
        "common/scripted_effects/advisory.txt",
        "advisory = { if = { limit = { has_government_attribute = $ATTRIBUTE$ } add_prestige = 1 } }",
    );
    let text = "country_event = { id = advisory.1 immediate = { advisory = { ATTRIBUTE = owned_missing_attribute } } }";
    let id = open(&mut host, "events/advisory.txt", text);
    let values = diagnostics(&host.snapshot(), &id);
    let advisory = values
        .iter()
        .find(|item| {
            item.code == DiagnosticCode::InvalidValue
                && item.message.contains("has_government_attribute")
        })
        .expect("reference rejection witness");
    assert_eq!(advisory.severity, crate::Severity::Warning, "{values:?}");
    assert!(
        advisory.message.contains("owned_missing_attribute"),
        "{advisory:?}"
    );
}
