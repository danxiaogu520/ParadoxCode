//! Phase-four integration checks run with an empty legacy semantic model.
use crate::{DiagnosticCode, complete, definition, diagnostics, hover, semantic_tokens};
use engine::{AnalysisHost, DocumentId};
use std::sync::Arc;

fn host() -> AnalysisHost {
    let ir = game::eu4::first_party_ir().expect("embedded IR");
    let catalog = rules::RuleSet::from_ir_catalog(&ir);
    assert!(catalog.semantic_rules().next().is_none());
    AnalysisHost::with_ir(catalog, ir.game.profile.clone(), ir)
}
fn open(host: &mut AnalysisHost, path: &str, source: &str) -> DocumentId {
    let id = DocumentId::new(format!("file:///fixture/{path}"));
    host.open_document(id.clone(), 1, source.to_owned(), None)
        .expect("open");
    id
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
                "{} {:?} {}..{} {}\n",
                value.code.as_str(),
                value.severity,
                value.range.start(),
                value.range.end(),
                value.message
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
    let start = source.find("maybe").unwrap() as u32;
    assert!(
        values
            .iter()
            .any(|value| value.code == DiagnosticCode::InvalidValue
                && value.range.start() == start
                && value.range.end() == start + 5),
        "{values:#?}"
    );
    let position = source.find("add_prestige").unwrap() as u32 + 3;
    assert!(
        hover(&snapshot, &id, position).is_some(),
        "quoted rule hover"
    );
}

#[test]
fn ir_empty_rhs_and_callable_parameters_complete() {
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
fn ir_callable_arguments_follow_definition_value_constraints() {
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
fn ir_scope_values_use_the_enclosing_register_state() {
    let mut host = host();
    let source = "country_event = { id = phase4.1 option = { name = phase4.title } immediate = { all_owned_province = { add_core = ROOT } } }";
    let id = open(&mut host, "events/scope-value.txt", source);
    let values = diagnostics(&host.snapshot(), &id);
    assert!(
        !values.iter().any(|diagnostic| matches!(
            diagnostic.code,
            DiagnosticCode::InvalidValue | DiagnosticCode::WrongScope
        )),
        "{values:#?}"
    );
    let source = "country_event = { id = phase4.2 immediate = { all_owned_province = { add_core = THIS } } }";
    let id = open(&mut host, "events/invalid-scope-value.txt", source);
    assert!(
        diagnostics(&host.snapshot(), &id)
            .iter()
            .any(|diagnostic| diagnostic.code == DiagnosticCode::InvalidValue
                && diagnostic.range.start() == source.find("THIS").unwrap() as u32)
    );
}
