use super::support::*;

#[test]
fn event_file_root_offers_all_entries_with_correct_shapes() {
    use std::path::PathBuf;

    // An empty event file offers the four real entry keys: two repeatable event blocks,
    // the namespace header, and the normal-or-historical-nations switch.
    let text = "\n";
    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    let id = DocumentId::new("file:///tmp/events/root-entries.txt");
    host.open_document(
        id.clone(),
        1,
        text.to_owned(),
        Some(PathBuf::from("events/root-entries.txt")),
    )
    .expect("open event document");
    let snapshot = host.snapshot();
    let result = complete(&snapshot, &id, 0);
    let by_label = |label: &str| result.items.iter().find(|item| item.label == label);
    for label in [
        "country_event",
        "province_event",
        "namespace",
        "normal_or_historical_nations",
    ] {
        assert!(
            by_label(label).is_some(),
            "event entry `{label}` missing: {:?}",
            result.items
        );
    }
    assert_eq!(
        by_label("country_event")
            .expect("country_event")
            .insert_text,
        "country_event = {\n\t$0\n}",
        "block entries must insert a skeleton: {result:?}"
    );
    assert_eq!(
        by_label("namespace").expect("namespace").insert_text,
        "namespace = ",
        "leaf entries must insert only the assignment: {result:?}"
    );
    assert_eq!(
        by_label("normal_or_historical_nations")
            .expect("normal_or_historical_nations")
            .insert_text,
        "normal_or_historical_nations = ",
        "leaf entries must insert only the assignment: {result:?}"
    );
}

#[test]
fn event_file_root_leaf_entry_completes_its_value_domain() {
    use std::path::PathBuf;

    // `normal_or_historical_nations = ` picks yes/no from the entry's value domain.
    let text = "normal_or_historical_nations = \n";
    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    let id = DocumentId::new("file:///tmp/events/root-leaf-value.txt");
    host.open_document(
        id.clone(),
        1,
        text.to_owned(),
        Some(PathBuf::from("events/root-leaf-value.txt")),
    )
    .expect("open event document");
    let snapshot = host.snapshot();
    let position = u32::try_from(text.find("= ").expect("assignment") + 2).expect("position");
    let result = complete(&snapshot, &id, position);
    let labels = result
        .items
        .iter()
        .map(|item| item.label.as_str())
        .collect::<Vec<_>>();
    assert_eq!(labels, vec!["no", "yes"], "{result:?}");

    // A namespace declaration has no value domain: its value is free text.
    let mut host2 = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    let id2 = DocumentId::new("file:///tmp/events/root-leaf-value-2.txt");
    host2
        .open_document(
            id2.clone(),
            1,
            "namespace = \n".to_owned(),
            Some(PathBuf::from("events/root-leaf-value-2.txt")),
        )
        .expect("open event document");
    let snapshot2 = host2.snapshot();
    let position2 =
        u32::try_from("namespace = \n".find("= ").expect("assignment") + 2).expect("position");
    let result2 = complete(&snapshot2, &id2, position2);
    assert!(
        result2.items.is_empty(),
        "namespace has no static value domain: {result2:?}"
    );
}

#[test]
fn event_file_root_repeats_blocks_but_not_single_declarations() {
    use std::path::PathBuf;

    // After `namespace`, the cursor on the root gap must still scaffold another event
    // block (repeatable), while the already-declared single entries disappear.
    let text = "namespace = ns\ncountry_event = { id = ns.1 }\n\n";
    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    let id = DocumentId::new("file:///tmp/events/root-gap.txt");
    host.open_document(
        id.clone(),
        1,
        text.to_owned(),
        Some(PathBuf::from("events/root-gap.txt")),
    )
    .expect("open event document");
    let snapshot = host.snapshot();
    let position = u32::try_from(text.find("\n\n").expect("root gap") + 1).expect("position");
    let result = complete(&snapshot, &id, position);
    let labels = result
        .items
        .iter()
        .map(|item| item.label.as_str())
        .collect::<Vec<_>>();
    assert!(
        labels.contains(&"country_event") && labels.contains(&"province_event"),
        "event blocks must stay scaffoldable: {labels:?}"
    );
    assert!(
        !labels.contains(&"namespace"),
        "the declared namespace header must not repeat: {labels:?}"
    );
    assert!(
        labels.contains(&"normal_or_historical_nations"),
        "the undeclared file switch must stay available: {labels:?}"
    );

    // The same rule family keeps decisions wrappers non-repeatable.
    let text2 = "country_decisions = {}\n";
    let mut host2 = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    let id2 = DocumentId::new("file:///tmp/decisions/root-gap.txt");
    host2
        .open_document(
            id2.clone(),
            1,
            text2.to_owned(),
            Some(PathBuf::from("decisions/root-gap.txt")),
        )
        .expect("open decision document");
    let snapshot2 = host2.snapshot();
    let position2 = u32::try_from(text2.find("}\n").expect("tail") + 2).expect("position");
    let result2 = complete(&snapshot2, &id2, position2);
    assert!(
        result2
            .items
            .iter()
            .all(|item| item.label != "country_decisions"),
        "an already-declared wrapper must not be re-offered: {result2:?}"
    );
}

#[test]
fn on_action_event_block_completion_excludes_namespace_headers() {
    use pdx_engine::{SourceRoot, SourceRootId, SourceRootKind, WorkspaceChange};
    use std::fs;

    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-analysis-event-symbols-{nonce}"));
    fs::create_dir_all(root.join("events")).expect("event directory");
    fs::create_dir_all(root.join("common").join("on_actions")).expect("on_action directory");
    let event_text = concat!(
        "namespace = flavor_x\n",
        "country_event = {\n",
        "\tid = flavor_x.1\n",
        "\ttitle = t\n",
        "\tdesc = d\n",
        "\toption = { name = a }\n",
        "}\n",
    );
    fs::write(root.join("events/flavor_x.txt"), event_text).expect("write event document");
    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(1),
        SourceRootKind::CurrentMod,
        root.clone(),
    )]));
    host.refresh_source_roots().expect("index event document");
    let id = DocumentId::new("file:///tmp/common/on_actions/completion.txt");
    let text = "on_startup = {\n  events = {\n    \n  }\n}\n";
    host.open_document(
        id.clone(),
        1,
        text.to_owned(),
        Some(std::path::PathBuf::from("common/on_actions/completion.txt")),
    )
    .expect("open on_action document");
    let snapshot = host.snapshot();
    let position =
        u32::try_from(text.find("    \n").expect("blank events body") + 4).expect("position");
    let result = complete(&snapshot, &id, position);
    let labels = result
        .items
        .iter()
        .map(|item| item.label.as_str())
        .collect::<Vec<_>>();
    assert!(
        labels.contains(&"flavor_x.1"),
        "the indexed event id must complete: {labels:?}"
    );
    assert!(
        !labels.contains(&"namespace"),
        "the namespace header must not be indexed as an event: {labels:?}"
    );
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn event_modifier_completion_inherits_generic_modifier_keys() {
    let text = "my_modifier = {\n  dis\n}\n";
    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    let id = DocumentId::new("file:///tmp/common/event_modifiers/test.txt");
    host.open_document(
        id.clone(),
        1,
        text.to_owned(),
        Some(std::path::PathBuf::from("common/event_modifiers/test.txt")),
    )
    .expect("open event modifier");
    let position = u32::try_from(text.find("dis").expect("completion prefix") + 3)
        .expect("completion position");
    let completion = complete(&host.snapshot(), &id, position);
    assert!(
        completion
            .items
            .iter()
            .any(|item| item.label == "discipline"),
        "event modifiers must complete generic modifier keys: {:?}",
        completion
            .items
            .iter()
            .map(|item| item.label.as_str())
            .collect::<Vec<_>>()
    );
}

#[test]
fn leaf_value_container_completion_offers_typed_workspace_members() {
    use pdx_engine::{SourceRoot, SourceRootId, SourceRootKind, WorkspaceChange};
    use std::fs;

    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-analysis-mission-complete-{nonce}"));
    fs::create_dir_all(root.join("missions")).expect("mission directory");
    let path = root.join("missions/test.txt");
    let text = concat!(
        "test_series = { slot = 1 generic = no ai = yes has_country_shield = no\n",
        "  mission_a = { icon = mission_unknown position = 1 }\n",
        "  mission_b = { icon = mission_unknown position = 2 required_missions = { } }\n",
        "}\n",
    );
    fs::write(&path, text).expect("write mission document");
    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(1),
        SourceRootKind::CurrentMod,
        root.clone(),
    )]));
    host.refresh_source_roots().expect("index mission document");
    let id = DocumentId::new("file:///tmp/missions/test.txt");
    host.open_document(id.clone(), 1, text.to_owned(), Some(path.clone()))
        .expect("open");
    let snapshot = host.snapshot();
    let position = u32::try_from(
        text.find("required_missions = { }")
            .expect("required_missions block")
            + "required_missions = {".len()
            + 1,
    )
    .expect("position");
    let result = complete(&snapshot, &id, position);
    assert!(
        result.items.iter().any(|item| item.label == "mission_a"),
        "a leaf-value container must complete workspace members: {:?}",
        result
            .items
            .iter()
            .map(|item| item.label.as_str())
            .collect::<Vec<_>>()
    );
    assert!(result.items.iter().any(|item| item.label == "mission_b"));
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn leaf_value_exact_literals_and_date_keys_complete_remaining_matchers() {
    let mut model = pdx_game::eu4::bootstrap_model();
    for rule in [
        SemanticRule {
            id: "fixture:container".to_owned(),
            context: "trigger".to_owned(),
            parent_path: Vec::new(),
            key: KeyMatcher::Exact("container".to_owned()),
            operator: None,
            value: ValueMatcher::AnyScalar,
            shape: RuleShape::Node,
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
        },
        SemanticRule {
            id: "fixture:exact-block".to_owned(),
            context: "trigger".to_owned(),
            parent_path: vec!["container".to_owned()],
            key: KeyMatcher::Exact("exact_block".to_owned()),
            operator: None,
            value: ValueMatcher::AnyScalar,
            shape: RuleShape::ValueClause,
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
        },
        SemanticRule {
            id: "fixture:exact-leaf".to_owned(),
            context: "trigger".to_owned(),
            parent_path: vec!["container".to_owned(), "exact_block".to_owned()],
            key: KeyMatcher::AnyScalar,
            operator: None,
            value: ValueMatcher::Exact("leader".to_owned()),
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
            min_occurs: None,
            strict_min: true,
            max_occurs: None,
            source_file: "fixture.semantic".to_owned(),
            line: 1,
        },
        SemanticRule {
            id: "fixture:date-key".to_owned(),
            context: "trigger".to_owned(),
            parent_path: vec!["container".to_owned()],
            key: KeyMatcher::Date,
            operator: None,
            value: ValueMatcher::AnyScalar,
            shape: RuleShape::Node,
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
        },
    ] {
        model.semantic.rules.push(rule);
    }
    let mut host = eu4_host(RuleSet::from_model(model));
    let id = DocumentId::new("file:///tmp/common/events/test.txt");

    // Inside the leaf-value container the exact literal completes as a key.
    let block_text = "trigger = { container = { exact_block = { lea } } }";
    host.open_document(id.clone(), 1, block_text.to_owned(), None)
        .expect("open block document");
    let block_position =
        u32::try_from(block_text.find("lea").expect("literal prefix") + 2).expect("position");
    let block_result = complete(&host.snapshot(), &id, block_position);
    assert!(
        block_result.items.iter().any(|item| item.label == "leader"),
        "an exact leaf-value container must complete the literal: {:?}",
        block_result
            .items
            .iter()
            .map(|item| item.label.as_str())
            .collect::<Vec<_>>()
    );

    // A bare value of the value_clause inherits the leaf-value literal as well.
    let bare_text = "trigger = { container = { exact_block = lea } }";
    host.stage_document_text(&id, 2, bare_text.to_owned())
        .expect("stage bare document");
    let prepared = host
        .snapshot()
        .prepare_document(&id)
        .expect("prepare bare document");
    assert!(host.commit_prepared_document(prepared));
    let bare_position =
        u32::try_from(bare_text.find("lea").expect("literal prefix") + 2).expect("position");
    let bare_result = complete(&host.snapshot(), &id, bare_position);
    assert!(
        bare_result.items.iter().any(|item| item.label == "leader"),
        "a value_clause bare value must complete the leaf-value literal: {:?}",
        bare_result
            .items
            .iter()
            .map(|item| item.label.as_str())
            .collect::<Vec<_>>()
    );

    // Date-keyed rules complete a campaign-date template.
    let date_text = "trigger = { container = {  } }";
    host.stage_document_text(&id, 3, date_text.to_owned())
        .expect("stage date document");
    let prepared = host
        .snapshot()
        .prepare_document(&id)
        .expect("prepare date document");
    assert!(host.commit_prepared_document(prepared));
    let date_result = complete(
        &host.snapshot(),
        &id,
        u32::try_from(
            date_text.find("container = {").expect("container block") + "container = {".len() + 1,
        )
        .expect("position"),
    );
    assert!(
        date_result
            .items
            .iter()
            .any(|item| item.label == "1444.11.11"),
        "date-keyed rules must complete a campaign date template"
    );
}

#[test]
fn leaf_value_clause_bare_value_completion_offers_typed_workspace_members() {
    use pdx_engine::{SourceRoot, SourceRootId, SourceRootKind, WorkspaceChange};
    use std::fs;

    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-analysis-mission-bare-{nonce}"));
    fs::create_dir_all(root.join("missions")).expect("mission directory");
    let path = root.join("missions/test.txt");
    let text = concat!(
        "test_series = { slot = 1 generic = no ai = yes has_country_shield = no\n",
        "  mission_a = { icon = mission_unknown position = 1 }\n",
        "  mission_b = { icon = mission_unknown position = 2 required_missions = \n",
        "  }\n",
        "}\n",
    );
    fs::write(&path, text).expect("write mission document");
    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(1),
        SourceRootKind::CurrentMod,
        root.clone(),
    )]));
    host.refresh_source_roots().expect("index mission document");
    let id = DocumentId::new("file:///tmp/missions/test.txt");
    host.open_document(id.clone(), 1, text.to_owned(), Some(path.clone()))
        .expect("open");
    let snapshot = host.snapshot();
    let position = u32::try_from(
        text.find("required_missions = ")
            .expect("required_missions value")
            + "required_missions = ".len(),
    )
    .expect("position");
    let result = complete(&snapshot, &id, position);
    assert!(
        result.items.iter().any(|item| item.label == "mission_a"),
        "a value_clause bare value must complete workspace members: {:?}",
        result
            .items
            .iter()
            .map(|item| item.label.as_str())
            .collect::<Vec<_>>()
    );
    assert!(result.items.iter().any(|item| item.label == "mission_b"));
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn quoted_script_completion_distinguishes_keys_values_and_escapes_snippets() {
    let key_text = "trigger = { embedded = \"\n fo\n\" }\n";
    let (host, id) = quoted_script_snapshot(key_text);
    let key_position =
        u32::try_from(key_text.find("fo").expect("foo prefix") + 2).expect("position");
    let keys = complete(&host.snapshot(), &id, key_position);
    assert!(keys.items.iter().any(|item| item.label == "foo"));

    let value_text = "trigger = { embedded = \"\n foo = \n\" }\n";
    let (host, id) = quoted_script_snapshot(value_text);
    let value_position =
        u32::try_from(value_text.find("foo = ").expect("value") + 6).expect("position");
    let values = complete(&host.snapshot(), &id, value_position);
    assert!(values.items.iter().any(|item| item.label == "yes"));

    let blank_text = "trigger = { embedded = \"\n \n\" }\n";
    let (host, id) = quoted_script_snapshot(blank_text);
    let blank_position =
        u32::try_from(blank_text.find("\n \n").expect("blank") + 2).expect("position");
    let items = complete(&host.snapshot(), &id, blank_position);
    let embedded = items
        .items
        .iter()
        .find(|item| item.label == "embedded")
        .expect("recursive quoted Script key");
    assert!(embedded.insert_text.contains("\\\""));
    assert!(!embedded.insert_text.contains(" = \"\n"));
}

#[test]
fn quoted_script_completion_survives_incomplete_payload_syntax() {
    let text = "trigger = { embedded = \"\n foo = \n";
    let (host, id) = quoted_script_snapshot(text);
    let position = u32::try_from(text.len() - 1).expect("position");
    let result = complete(&host.snapshot(), &id, position);
    assert!(result.items.iter().any(|item| item.label == "yes"));
}

#[test]
fn incomplete_input_has_syntax_diagnostics_and_completion() {
    let text = "country_event = { id = test.1\n  mt";
    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    let id = DocumentId::new("file:///tmp/common/events/test.txt");
    host.open_document(id.clone(), 1, text.to_owned(), None)
        .expect("open");
    let snapshot = host.snapshot();
    assert!(
        diagnostics(&snapshot, &id)
            .iter()
            .any(|item| item.code == DiagnosticCode::Syntax)
    );
    let result = complete(
        &snapshot,
        &id,
        u32::try_from(text.find("mt").expect("prefix") + 1).expect("position"),
    );
    assert!(
        !result.items.is_empty(),
        "a partially typed key inside a covered block must still complete"
    );
}

#[test]
fn semantic_completion_does_not_materialize_the_full_workspace() {
    let text = concat!(
        "country_event = { mean_time_to_happen = { ",
        "modifier = { factor = 0.5 always = maybe }",
        " } }\n",
    );
    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    let id = DocumentId::new("file:///tmp/events/completion-fast-path.txt");
    host.open_document(id.clone(), 1, text.to_owned(), None)
        .expect("open");
    let snapshot = host.snapshot();
    let position = u32::try_from(text.find("always").expect("completion key")).expect("position");

    crate::ALL_SEMANTICS_CALLS.with(|calls| calls.set(0));
    let completion = complete(&snapshot, &id, position);

    assert!(completion.items.iter().any(|item| item.label == "always"));
    crate::ALL_SEMANTICS_CALLS.with(|calls| {
        assert_eq!(
            calls.get(),
            0,
            "contextual completion must not clone all workspace definitions and references"
        );
    });
}

#[test]
fn completion_traversal_uses_hir_to_disambiguate_nested_rule_contexts() {
    let text = concat!(
        "country_event = { mean_time_to_happen = { ",
        "modifier = { factor = 0.5 always = maybe }",
        " } }\n",
    );
    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    let id = DocumentId::new("file:///tmp/events/completion_scope.txt");
    host.open_document(id.clone(), 1, text.to_owned(), None)
        .expect("open");
    let snapshot = host.snapshot();
    let input = input_for_document(&snapshot, &id).expect("analysis input");
    let position = u32::try_from(text.find("always").expect("trigger child")).expect("position");
    let context = semantic_completion_context(&snapshot, &input, position)
        .expect("semantic completion context");
    assert_eq!(context.context, "trigger");
    assert!(context.parent_path.is_empty());
    assert_eq!(
        context.structural_containers,
        [("modifier_rule".to_owned(), vec!["modifier".to_owned()])]
    );
    assert_eq!(context.scope.current, "country");
    let mut all_key_items = Vec::new();
    let mut member_cache = crate::CompletionMemberCache::default();
    crate::add_semantic_key_items(
        &snapshot,
        &context,
        &mut member_cache,
        &mut all_key_items,
        TextRange::empty(position),
        "",
        true,
    );
    assert!(all_key_items.iter().any(|item| item.label == "always"));
    assert!(all_key_items.iter().any(|item| item.label == "factor"));
    let completion = complete(&snapshot, &id, position);
    assert!(
        completion.items.iter().any(|item| item.label == "always"),
        "trigger keys must be offered inside the disambiguated modifier block: {:?}",
        completion.items
    );
    let results = diagnostics(&snapshot, &id);
    assert!(
        results
            .iter()
            .all(|item| item.code != DiagnosticCode::UnknownKey),
        "structural modifier fields and nested trigger keys must both be recognized: {results:?}"
    );
    assert!(
        results.iter().any(|item| {
            item.code == DiagnosticCode::InvalidValue && item.message.contains("always")
        }),
        "the nested trigger context must validate `always`: {results:?}"
    );
}

#[test]
fn decision_completion_skips_type_instance_wrapper() {
    use std::path::PathBuf;

    let text = concat!(
        "country_decisions = {\n",
        "  my_decision = {\n",
        "    \n",
        "    potential = {\n",
        "      \n",
        "    }\n",
        "  }\n",
        "}\n",
    );
    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    let id = DocumentId::new("file:///tmp/common/decisions/completion.txt");
    host.open_document(
        id.clone(),
        1,
        text.to_owned(),
        Some(PathBuf::from("common/decisions/completion.txt")),
    )
    .expect("open decision document");
    let snapshot = host.snapshot();
    let input = input_for_document(&snapshot, &id).expect("analysis input");
    let position =
        u32::try_from(text.find("    \n").expect("blank decision body") + 4).expect("position");
    let context = semantic_completion_context(&snapshot, &input, position)
        .expect("semantic completion context");
    assert_eq!(context.context, "type:decision");
    assert!(
        context.parent_path.is_empty(),
        "decision instance names must not become semantic parent paths: {context:?}"
    );

    let result = complete(&snapshot, &id, position);
    for label in ["potential", "allow", "effect"] {
        assert!(
            result.items.iter().any(|item| item.label == label),
            "decision key `{label}` was not completed: {:?}",
            result.items
        );
    }

    let nested_position = u32::try_from(text.find("      \n").expect("blank potential body") + 6)
        .expect("nested position");
    let nested_result = complete(&snapshot, &id, nested_position);
    assert!(
        nested_result
            .items
            .iter()
            .any(|item| item.label == "has_country_flag"),
        "potential must enter the trigger context: {:?}",
        nested_result.items
    );
}

#[test]
fn decision_file_root_offers_only_the_country_decisions_entry() {
    use std::path::PathBuf;

    // An empty decision file: the only candidate is the `country_decisions` wrapper. No
    // decision body keys may appear because no decision instance exists yet.
    let text = "\n";
    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    let id = DocumentId::new("file:///tmp/decisions/root-entry.txt");
    host.open_document(
        id.clone(),
        1,
        text.to_owned(),
        Some(PathBuf::from("decisions/root-entry.txt")),
    )
    .expect("open decision document");
    let snapshot = host.snapshot();
    let result = complete(&snapshot, &id, 0);
    let labels = result
        .items
        .iter()
        .map(|item| item.label.as_str())
        .collect::<Vec<_>>();
    assert_eq!(labels, vec!["country_decisions"], "{result:?}");
    let entry = &result.items[0];
    assert_eq!(
        entry.insert_text, "country_decisions = {\n\t$0\n}",
        "the wrapper entry must insert an empty block skeleton: {result:?}"
    );
    assert_eq!(entry.kind, CompletionKind::Key);

    // A typed prefix narrows the candidate but keeps the same entry.
    let mut host2 = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    let id2 = DocumentId::new("file:///tmp/decisions/root-entry-2.txt");
    host2
        .open_document(
            id2.clone(),
            1,
            "cou".to_owned(),
            Some(PathBuf::from("decisions/root-entry-2.txt")),
        )
        .expect("open prefixed document");
    let snapshot2 = host2.snapshot();
    let prefixed = complete(&snapshot2, &id2, 3);
    assert!(
        prefixed
            .items
            .iter()
            .any(|item| item.label == "country_decisions"),
        "prefix `cou` must still resolve the entry: {prefixed:?}"
    );
    assert!(
        prefixed.items.iter().all(|item| item.label != "allow"),
        "no decision body keys may leak into the file root: {prefixed:?}"
    );

    // The entry is path-scoped: event files must not offer the decision wrapper.
    let mut host3 = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    let id3 = DocumentId::new("file:///tmp/events/root-entry.txt");
    host3
        .open_document(
            id3.clone(),
            1,
            "\n".to_owned(),
            Some(PathBuf::from("events/root-entry.txt")),
        )
        .expect("open event document");
    let snapshot3 = host3.snapshot();
    let event_root = complete(&snapshot3, &id3, 0);
    assert!(
        event_root
            .items
            .iter()
            .all(|item| item.label != "country_decisions"),
        "the decision wrapper must not leak into event files: {event_root:?}"
    );

    // Once the wrapper is declared, the file root stops suggesting it again.
    let text4 = "country_decisions = {}\n";
    let mut host4 = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    let id4 = DocumentId::new("file:///tmp/decisions/root-entry-4.txt");
    host4
        .open_document(
            id4.clone(),
            1,
            text4.to_owned(),
            Some(PathBuf::from("decisions/root-entry-4.txt")),
        )
        .expect("open populated document");
    let snapshot4 = host4.snapshot();
    let populated = complete(
        &snapshot4,
        &id4,
        text4.len().try_into().expect("tail position"),
    );
    assert!(
        populated
            .items
            .iter()
            .all(|item| item.label != "country_decisions"),
        "an already-declared wrapper must not be re-offered: {populated:?}"
    );
}

#[test]
fn decision_wrapper_body_without_instance_offers_no_key_candidates() {
    use std::path::PathBuf;

    // Inside `country_decisions = { … }` but before any decision instance, the only legal
    // content is a free-form decision id, so no rule-backed key may be completed.
    let text = "country_decisions = {\n  \n}\n";
    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    let id = DocumentId::new("file:///tmp/decisions/wrapper-body.txt");
    host.open_document(
        id.clone(),
        1,
        text.to_owned(),
        Some(PathBuf::from("decisions/wrapper-body.txt")),
    )
    .expect("open decision document");
    let snapshot = host.snapshot();
    let position =
        u32::try_from(text.find("  \n").expect("blank wrapper body") + 2).expect("position");
    let result = complete(&snapshot, &id, position);
    assert!(
        result.items.is_empty(),
        "the wrapper container must not offer decision body keys: {:?}",
        result
            .items
            .iter()
            .map(|item| item.label.as_str())
            .collect::<Vec<_>>()
    );

    // Same after an existing instance: the gap between instances is still an instance-name
    // position, never a key position.
    let text2 = "country_decisions = {\n  d1 = { allow = {} }\n  \n}\n";
    let mut host2 = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    let id2 = DocumentId::new("file:///tmp/decisions/wrapper-body-2.txt");
    host2
        .open_document(
            id2.clone(),
            1,
            text2.to_owned(),
            Some(PathBuf::from("decisions/wrapper-body-2.txt")),
        )
        .expect("open decision document");
    let snapshot2 = host2.snapshot();
    let position2 =
        u32::try_from(text2.find("  \n").expect("blank wrapper body") + 2).expect("position");
    let result2 = complete(&snapshot2, &id2, position2);
    assert!(
        result2.items.is_empty(),
        "the wrapper gap must stay empty: {:?}",
        result2
            .items
            .iter()
            .map(|item| item.label.as_str())
            .collect::<Vec<_>>()
    );

    // Sanity: once the cursor is inside a decision instance, body keys come back.
    let text3 = "country_decisions = {\n  d1 = {\n    \n  }\n}\n";
    let mut host3 = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    let id3 = DocumentId::new("file:///tmp/decisions/wrapper-body-3.txt");
    host3
        .open_document(
            id3.clone(),
            1,
            text3.to_owned(),
            Some(PathBuf::from("decisions/wrapper-body-3.txt")),
        )
        .expect("open decision document");
    let snapshot3 = host3.snapshot();
    let position3 =
        u32::try_from(text3.find("    \n").expect("blank instance body") + 4).expect("position");
    let result3 = complete(&snapshot3, &id3, position3);
    for label in ["potential", "allow", "effect", "major", "ai_will_do"] {
        assert!(
            result3.items.iter().any(|item| item.label == label),
            "decision key `{label}` was not completed inside the instance: {:?}",
            result3.items
        );
    }
}

#[test]
fn empty_ambiguous_block_completion_unions_possible_rule_destinations() {
    let text = concat!(
        "country_event = {\n",
        "  mean_time_to_happen = {\n",
        "    \n",
        "  }\n",
        "}\n",
    );
    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    let id = DocumentId::new("file:///tmp/events/empty-scope-completion.txt");
    host.open_document(id.clone(), 1, text.to_owned(), None)
        .expect("open");
    let snapshot = host.snapshot();
    let input = input_for_document(&snapshot, &id).expect("analysis input");
    let position = u32::try_from(text.find("    \n").expect("blank completion line"))
        .expect("position")
        .saturating_add(4);
    let context = semantic_completion_context(&snapshot, &input, position)
        .expect("semantic completion context");
    assert!(
        context
            .alternative_containers
            .iter()
            .any(|container| container.context == "modifier_rule"),
        "the conflicting modifier destination must remain available"
    );
    let completion = complete(&snapshot, &id, position);
    assert!(completion.items.iter().any(|item| item.label == "days"));
    assert!(completion.items.iter().any(|item| item.label == "modifier"));
}

#[test]
fn semantic_rules_drive_value_completion_and_hover() {
    let (host, id) = semantic_snapshot("trigger = { foo = yes }\n");
    let snapshot = host.snapshot();
    let value = u32::try_from("trigger = { foo = ".len()).expect("offset");
    let result = complete(&snapshot, &id, value);
    assert!(result.items.iter().any(|item| item.label == "yes"));
    let property = u32::try_from("trigger = { ".len() + 1).expect("offset");
    let property_hover = hover(&snapshot, &id, property).expect("semantic hover");
    assert!(property_hover.contents.contains("PDX property `foo`"));
    assert!(
        property_hover
            .contents
            .starts_with("### PDX property `foo`")
    );
    assert!(
        property_hover
            .contents
            .contains("- value: `bool (`yes` / `no`)`")
    );

    assert!(!property_hover.contents.contains("context: `trigger`"));
    assert!(!property_hover.contents.contains("shape: `scalar`"));
    assert!(!property_hover.contents.contains("Provenance"));

    let value_position = u32::try_from("trigger = { foo = yes".find("yes").expect("value") + 1)
        .expect("value offset");
    let value_hover = hover(&snapshot, &id, value_position).expect("value hover");
    assert!(value_hover.contents.contains("PDX value `yes`"));
    assert!(value_hover.contents.starts_with("### PDX value `yes`"));
    assert!(value_hover.contents.contains("- validation: `accepted`"));
    assert!(value_hover.contents.contains("validation: `accepted`"));

    let (invalid_host, invalid_id) = semantic_snapshot("trigger = { foo = maybe }\n");
    let invalid_text = "trigger = { foo = maybe }\n";
    let invalid_position = u32::try_from(invalid_text.find("maybe").expect("invalid value") + 1)
        .expect("invalid offset");
    let invalid_hover = hover(&invalid_host.snapshot(), &invalid_id, invalid_position)
        .expect("invalid value hover");
    assert!(
        invalid_hover
            .contents
            .contains("validation: `does not match`")
    );
}

#[test]
fn localisation_key_position_completes_existing_and_workspace_keys() {
    let mut host = eu4_host(pdx_game::eu4::bootstrap_rules());
    let id = DocumentId::new("file:///tmp/localisation/test.yml");
    let text = "l_english:\nfoo_name:0 \"Foo\"\nbar:0 \"\"\nfo";
    host.open_document(id.clone(), 1, text.to_owned(), None)
        .expect("open");
    let header_result = complete(
        &host.snapshot(),
        &id,
        u32::try_from("l_english:".len()).expect("header position"),
    );
    assert!(
        header_result.items.is_empty(),
        "language headers must not offer localisation entry keys: {:?}",
        header_result.items
    );
    let result = complete(
        &host.snapshot(),
        &id,
        u32::try_from(text.len()).expect("position"),
    );
    let labels = result
        .items
        .iter()
        .map(|item| item.label.as_str())
        .collect::<Vec<_>>();
    assert!(
        labels.contains(&"foo_name"),
        "a partially typed localisation key must complete: {labels:?}"
    );
    assert!(
        !labels.contains(&"bar"),
        "non-matching localisation keys must be filtered: {labels:?}"
    );
    assert!(
        result
            .items
            .iter()
            .all(|item| item.kind == CompletionKind::Localisation),
        "localisation key completion must not offer PDX properties: {:?}",
        result.items
    );
}

#[test]
fn scripted_definition_completion_snippet_includes_parameters() {
    use pdx_engine::{SourceRoot, SourceRootId, SourceRootKind, WorkspaceChange};
    use std::fs;

    let root = std::env::temp_dir().join(format!("pdx-analysis-snippet-{}", std::process::id()));
    fs::create_dir_all(root.join("common/scripted_effects")).expect("effect directory");
    fs::create_dir_all(root.join("common/scripted_triggers")).expect("trigger directory");
    fs::write(
        root.join("common/scripted_effects/00_test.txt"),
        concat!(
            "apply = { value = $zeta$ [[alpha] enabled = yes ] }\n",
            "plain = { add_prestige = 1 }\n",
            "scalar = { add_prestige = $amount$ }\n",
            "optional_only = { [[value] add_prestige = $value$ ] }\n",
        ),
    )
    .expect("scripted effect definition");
    fs::write(
        root.join("common/scripted_triggers/00_test.txt"),
        "check = { always = yes }\n",
    )
    .expect("scripted trigger definition");
    let rules = pdx_game::eu4::first_party_rules().expect("load first-party rules");
    let mut host = eu4_host(rules);
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot {
        id: SourceRootId::new(1),
        kind: SourceRootKind::CurrentMod,
        path: root.clone(),
        order: 0,
        writable: true,
    }]));
    host.refresh_source_roots().expect("scan effect definition");
    let id = DocumentId::new("file:///tmp/events/snippet.txt");
    let text = "country_event = { immediate = { ap";
    host.open_document(id.clone(), 1, text.to_owned(), None)
        .expect("open");
    let position = u32::try_from(text.find("ap").expect("prefix") + 1).expect("position");
    let completion = complete(&host.snapshot(), &id, position);
    let snippet = completion
        .items
        .iter()
        .find(|item| item.label == "apply")
        .expect("scripted effect item");
    assert_eq!(snippet.kind, CompletionKind::ScriptedMacro);
    assert_eq!(snippet.insert_text, "apply = {\n\tzeta = $1\n\t$0\n}");

    let trigger_id = DocumentId::new("file:///tmp/events/snippet-trigger.txt");
    let trigger_text = "country_event = { trigger = { ch";
    host.open_document(trigger_id.clone(), 1, trigger_text.to_owned(), None)
        .expect("open trigger completion document");
    let trigger_completion = complete(
        &host.snapshot(),
        &trigger_id,
        u32::try_from(trigger_text.find("ch").expect("trigger prefix") + 1)
            .expect("trigger position"),
    );
    let trigger_macro = trigger_completion
        .items
        .iter()
        .find(|item| item.label == "check")
        .expect("scripted trigger item");
    assert_eq!(trigger_macro.kind, CompletionKind::ScriptedMacro);
    assert_eq!(
        crate::semantic::scripted_definition_snippet(&host.snapshot(), "scripted_effect", "plain"),
        "plain = yes"
    );
    assert_eq!(
        crate::semantic::scripted_definition_snippet(&host.snapshot(), "scripted_effect", "scalar"),
        "scalar = {\n\tamount = $1\n\t$0\n}"
    );
    assert_eq!(
        crate::semantic::scripted_definition_snippet(
            &host.snapshot(),
            "scripted_effect",
            "optional_only"
        ),
        "optional_only = {\n\t$0\n}"
    );
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn scripted_macro_call_block_completes_only_the_owners_parameter_keys() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-analysis-macro-call-keys-{nonce}"));
    let definitions = root.join("common/scripted_effects");
    std::fs::create_dir_all(&definitions).expect("definition directory");
    std::fs::write(
        definitions.join("00_complete.txt"),
        concat!(
            "scaled = { add_prestige = $AMOUNT$ custom_tooltip = $TOOLTIP$ }\n",
            "other = { add_stability = $OTHER$ }\n",
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
    let id = DocumentId::new("file:///tmp/events/macro-call-keys.txt");
    let text = "country_event = { immediate = { scaled = { AM } } }\n";
    host.open_document(id.clone(), 1, text.to_owned(), None)
        .expect("open call");
    let position = u32::try_from(text.find("AM }").expect("prefix") + 2).expect("position");

    let items = complete(&host.snapshot(), &id, position).items;
    let amount = items
        .iter()
        .find(|item| item.label == "AMOUNT")
        .unwrap_or_else(|| panic!("missing owner parameter completion: {items:?}"));
    assert_eq!(amount.kind, CompletionKind::MacroParameter);
    assert_eq!(amount.detail, "parameter");
    assert!(items.iter().all(|item| item.label != "OTHER"), "{items:?}");
    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn scripted_macro_argument_values_follow_direct_and_nested_body_constraints() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-analysis-macro-call-values-{nonce}"));
    let definitions = root.join("common/scripted_effects");
    std::fs::create_dir_all(&definitions).expect("definition directory");
    std::fs::write(
        definitions.join("00_complete.txt"),
        concat!(
            "direct_bool = { set_primitive = $VALUE$ }\n",
            "direct_float = { add_prestige = $VALUE$ }\n",
            "inner_bool = { set_primitive = $VALUE$ }\n",
            "outer_bool = { inner_bool = { VALUE = $OUTER$ } }\n",
        ),
    )
    .expect("macro definitions");
    let triggers = root.join("common/scripted_triggers");
    std::fs::create_dir_all(&triggers).expect("trigger definition directory");
    std::fs::write(
        triggers.join("00_complete.txt"),
        "bool_trigger = { uses_karma = $VALUE$ }\n",
    )
    .expect("macro trigger definition");
    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(1),
        SourceRootKind::CurrentMod,
        root.clone(),
    )]));
    host.refresh_source_roots().expect("scan definitions");

    let cases = [
        ("direct_bool", "VALUE", &["yes", "no"][..]),
        ("direct_float", "VALUE", &["0"][..]),
        ("outer_bool", "OUTER", &["yes", "no"][..]),
    ];
    for (index, (macro_name, parameter, expected)) in cases.into_iter().enumerate() {
        let id = DocumentId::new(format!("file:///tmp/events/macro-value-{index}.txt"));
        let text = format!(
            "country_event = {{ immediate = {{ {macro_name} = {{ {parameter} =  }} }} }}\n"
        );
        host.open_document(id.clone(), 1, text.clone(), None)
            .expect("open call");
        let position =
            u32::try_from(text.find("=  }").expect("empty value") + 2).expect("position");
        let items = complete(&host.snapshot(), &id, position).items;
        let labels = items
            .iter()
            .map(|item| item.label.as_str())
            .collect::<Vec<_>>();
        for label in expected {
            assert!(
                labels.contains(label),
                "{macro_name}.{parameter} missing {label}: {items:?}"
            );
        }
    }
    let trigger_id = DocumentId::new("file:///tmp/events/macro-trigger-value.txt");
    let trigger_text = "country_event = { trigger = { bool_trigger = { VALUE =  } } }\n";
    host.open_document(trigger_id.clone(), 1, trigger_text.to_owned(), None)
        .expect("open trigger call");
    let trigger_position =
        u32::try_from(trigger_text.find("=  }").expect("empty value") + 2).expect("position");
    let trigger_items = complete(&host.snapshot(), &trigger_id, trigger_position).items;
    assert!(
        trigger_items.iter().any(|item| item.label == "yes"),
        "scripted trigger constraint missing: {trigger_items:?}"
    );
    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn scripted_macro_bare_parameter_infers_quoted_effect_completion_context() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-analysis-macro-quoted-{nonce}"));
    let definitions = root.join("common/scripted_effects");
    std::fs::create_dir_all(&definitions).expect("definition directory");
    std::fs::write(definitions.join("00_complete.txt"), "inject = { $BODY$ }\n")
        .expect("macro definition");
    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(1),
        SourceRootKind::CurrentMod,
        root.clone(),
    )]));
    host.refresh_source_roots().expect("scan definitions");
    let id = DocumentId::new("file:///tmp/events/macro-quoted.txt");
    let text = "country_event = { immediate = { inject = { BODY = \"add_pre\" } } }\n";
    host.open_document(id.clone(), 1, text.to_owned(), None)
        .expect("open call");
    let position =
        u32::try_from(text.find("add_pre").expect("prefix") + "add_pre".len()).expect("position");

    let items = complete(&host.snapshot(), &id, position).items;

    let add_prestige = items
        .iter()
        .find(|item| item.label == "add_prestige")
        .expect("quoted macro argument did not inherit effect completion");
    assert!(
        add_prestige.sort_score < 10_000_000,
        "macro-inferred candidates must retain the highest schema tier: {items:?}"
    );
    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn scripted_macro_argument_value_inference_handles_conditionals_scope_and_conflicts() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-analysis-macro-value-kinds-{nonce}"));
    let definitions = root.join("common/scripted_effects");
    std::fs::create_dir_all(&definitions).expect("definition directory");
    std::fs::write(
        definitions.join("00_complete.txt"),
        concat!(
            "conditional = { [[FLAG] set_primitive = $VALUE$ ] [[!FLAG] custom_tooltip = $VALUE$ ] }\n",
            "scoped = { join_trade_league = $WHO$ }\n",
            "nested_scoped = { owner = { join_trade_league = $WHO$ } }\n",
            "conflict = { set_primitive = $VALUE$ custom_tooltip = $VALUE$ }\n",
            "cycle_a = { cycle_b = { VALUE = $VALUE$ } }\n",
            "cycle_b = { cycle_a = { VALUE = $VALUE$ } }\n",
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
    let localisation_id = DocumentId::new("file:///tmp/localisation/macro_l_english.yml");
    host.open_document(
        localisation_id,
        1,
        "l_english:\n macro_tip:0 \"Macro tip\"\n".to_owned(),
        None,
    )
    .expect("open localisation");

    let complete_argument = |host: &AnalysisHost, id_suffix: &str, body: &str| {
        let id = DocumentId::new(format!("file:///tmp/events/{id_suffix}.txt"));
        let text = format!("country_event = {{ immediate = {{ {body} }} }}\n");
        let mut host = host.clone();
        host.open_document(id.clone(), 1, text.clone(), None)
            .expect("open call");
        let position =
            u32::try_from(text.find("=  }").expect("empty target value") + 2).expect("position");
        complete(&host.snapshot(), &id, position).items
    };

    let active = complete_argument(
        &host,
        "conditional-active",
        "conditional = { FLAG = yes VALUE =  }",
    );
    assert!(active.iter().any(|item| item.label == "yes"), "{active:?}");
    assert!(active.iter().any(|item| item.label == "no"), "{active:?}");
    assert!(
        active.iter().all(|item| item.label != "macro_tip"),
        "inactive negative branch leaked: {active:?}"
    );

    let inactive = complete_argument(&host, "conditional-inactive", "conditional = { VALUE =  }");
    assert!(
        inactive.iter().any(|item| item.label == "macro_tip"),
        "negative branch localisation constraint missing: {inactive:?}"
    );
    assert!(
        inactive.iter().all(|item| item.label != "yes"),
        "inactive positive branch leaked: {inactive:?}"
    );

    let scoped = complete_argument(&host, "scoped", "scoped = { WHO =  }");
    assert!(
        scoped.iter().any(|item| item.label == "THIS"),
        "call-site scope candidates missing: {scoped:?}"
    );

    let nested_scope_id = DocumentId::new("file:///tmp/events/nested-scoped.txt");
    let nested_scope_text = "province_event = { immediate = { nested_scoped = { WHO =  } } }\n";
    host.open_document(
        nested_scope_id.clone(),
        1,
        nested_scope_text.to_owned(),
        None,
    )
    .expect("open nested scope call");
    let nested_scope_position =
        u32::try_from(nested_scope_text.find("=  }").expect("empty value") + 2).expect("position");
    let nested_scoped = complete(&host.snapshot(), &nested_scope_id, nested_scope_position).items;
    assert!(
        nested_scoped.iter().any(|item| item.label == "THIS"),
        "macro body scope transition was not applied: {nested_scoped:?}"
    );

    let conflict = complete_argument(&host, "conflict", "conflict = { VALUE =  }");
    assert!(
        conflict.is_empty(),
        "incompatible use-site constraints must not offer guesses: {conflict:?}"
    );
    let cycle = complete_argument(&host, "cycle", "cycle_a = { VALUE =  }");
    assert!(
        cycle.iter().any(|item| item.label == "THIS"),
        "cyclic inference must fall back to rule-backed scope candidates: {cycle:?}"
    );
    assert!(
        cycle
            .iter()
            .all(|item| item.detail == "scope" || item.detail == "scope link"),
        "cyclic fallback must not guess unrelated value types: {cycle:?}"
    );
    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn vanilla_cache_only_macro_value_completion_uses_persisted_body_constraints() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-analysis-cached-completion-{nonce}"));
    let definitions = root.join("common/scripted_effects");
    std::fs::create_dir_all(&definitions).expect("definition directory");
    std::fs::write(
        definitions.join("00_cached.txt"),
        "cached_bool = { set_primitive = $VALUE$ }\n",
    )
    .expect("macro definition");
    let rules = pdx_game::eu4::first_party_rules().expect("first-party rules");
    let mut vanilla_host = eu4_host(rules.clone());
    vanilla_host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(0),
        SourceRootKind::Vanilla,
        root.clone(),
    )]));
    vanilla_host.refresh_source_roots().expect("scan Vanilla");
    let cache = IndexCache::from_snapshot(&vanilla_host.snapshot()).expect("build cache");
    std::fs::remove_dir_all(&root).expect("discard Vanilla source");

    let mut host = eu4_host(rules);
    host.install_index_cache(cache).expect("install cache");
    let id = DocumentId::new("file:///tmp/events/cached-completion.txt");
    let text = "country_event = { immediate = { cached_bool = { VALUE =  } } }\n";
    host.open_document(id.clone(), 1, text.to_owned(), None)
        .expect("open call");
    let position = u32::try_from(text.find("=  }").expect("empty value") + 2).expect("position");
    let items = complete(&host.snapshot(), &id, position).items;
    assert!(
        items.iter().any(|item| item.label == "yes") && items.iter().any(|item| item.label == "no"),
        "cache-only macro body constraints were not restored: {items:?}"
    );
}

#[test]
fn vanilla_cache_only_macro_completes_inside_quoted_effect_payload() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-analysis-cached-quoted-complete-{nonce}"));
    let definitions = root.join("common/scripted_effects");
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
        root.clone(),
    )]));
    vanilla_host.refresh_source_roots().expect("scan Vanilla");
    let cache = IndexCache::from_snapshot(&vanilla_host.snapshot()).expect("build cache");
    std::fs::remove_dir_all(&root).expect("discard Vanilla source");

    let mut host = eu4_host(rules);
    host.install_index_cache(cache).expect("install cache");
    let id = DocumentId::new("file:///tmp/events/cached-quoted-completion.txt");
    let text = "country_event = { immediate = { cached_inject = { BODY = \"add_pre\" } } }\n";
    host.open_document(id.clone(), 1, text.to_owned(), None)
        .expect("open call");
    let position =
        u32::try_from(text.find("add_pre").expect("prefix") + "add_pre".len()).expect("position");

    let items = complete(&host.snapshot(), &id, position).items;
    assert!(
        items.iter().any(|item| item.label == "add_prestige"),
        "cache-only quoted macro argument did not inherit effect completion: {items:?}"
    );
}

#[test]
fn vanilla_cache_macro_templates_preserve_nested_conditional_and_scope_semantics() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-analysis-cached-macro-semantics-{nonce}"));
    let definitions = root.join("common/scripted_effects");
    let localisation = root.join("localisation");
    std::fs::create_dir_all(&definitions).expect("definition directory");
    std::fs::create_dir_all(&localisation).expect("localisation directory");
    std::fs::write(
        definitions.join("00_cached.txt"),
        concat!(
            "cached_inner = { set_primitive = $VALUE$ }\n",
            "cached_outer = { cached_inner = { VALUE = $VALUE$ } }\n",
            "cached_conditional = { [[FLAG] set_primitive = $VALUE$ ] [[!FLAG] custom_tooltip = $VALUE$ ] }\n",
            "cached_scoped = { owner = { join_trade_league = $WHO$ } }\n",
        ),
    )
    .expect("macro definitions");
    std::fs::write(
        localisation.join("cached_l_english.yml"),
        "l_english:\n cached_macro_tip:0 \"Cached macro tip\"\n",
    )
    .expect("localisation");
    let rules = pdx_game::eu4::first_party_rules().expect("first-party rules");
    let mut vanilla_host = eu4_host(rules.clone());
    vanilla_host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(0),
        SourceRootKind::Vanilla,
        root.clone(),
    )]));
    vanilla_host.refresh_source_roots().expect("scan Vanilla");
    let cache = IndexCache::from_snapshot(&vanilla_host.snapshot()).expect("build cache");
    std::fs::remove_dir_all(&root).expect("discard Vanilla source");

    let mut host = eu4_host(rules);
    host.install_index_cache(cache).expect("install cache");
    let complete_argument = |host: &AnalysisHost, id_suffix: &str, event: &str, body: &str| {
        let id = DocumentId::new(format!("file:///tmp/events/{id_suffix}.txt"));
        let text = format!("{event} = {{ immediate = {{ {body} }} }}\n");
        let mut host = host.clone();
        host.open_document(id.clone(), 1, text.clone(), None)
            .expect("open call");
        let position =
            u32::try_from(text.find("=  }").expect("empty target value") + 2).expect("position");
        complete(&host.snapshot(), &id, position).items
    };

    let nested = complete_argument(
        &host,
        "cached-nested",
        "country_event",
        "cached_outer = { VALUE =  }",
    );
    assert!(nested.iter().any(|item| item.label == "yes"), "{nested:?}");

    let active = complete_argument(
        &host,
        "cached-conditional-active",
        "country_event",
        "cached_conditional = { FLAG = yes VALUE =  }",
    );
    assert!(active.iter().any(|item| item.label == "yes"), "{active:?}");
    assert!(
        active.iter().all(|item| item.label != "cached_macro_tip"),
        "inactive negative branch leaked from cached template: {active:?}"
    );

    let inactive = complete_argument(
        &host,
        "cached-conditional-inactive",
        "country_event",
        "cached_conditional = { VALUE =  }",
    );
    assert!(
        inactive.iter().any(|item| item.label == "cached_macro_tip"),
        "negative branch was not restored from cached template: {inactive:?}"
    );
    assert!(
        inactive.iter().all(|item| item.label != "yes"),
        "inactive positive branch leaked from cached template: {inactive:?}"
    );

    let scoped = complete_argument(
        &host,
        "cached-scoped",
        "province_event",
        "cached_scoped = { WHO =  }",
    );
    assert!(
        scoped.iter().any(|item| item.label == "THIS"),
        "scope transition was not restored from cached template: {scoped:?}"
    );
}

#[test]
fn current_mod_macro_template_overrides_cached_vanilla_template() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-analysis-macro-priority-{nonce}"));
    let vanilla = root.join("vanilla");
    let current = root.join("current");
    std::fs::create_dir_all(vanilla.join("common/scripted_effects")).expect("Vanilla directory");
    std::fs::create_dir_all(current.join("common/scripted_effects")).expect("current directory");
    std::fs::create_dir_all(current.join("localisation")).expect("localisation directory");
    std::fs::write(
        vanilla.join("common/scripted_effects/00_priority.txt"),
        "priority_macro = { set_primitive = $VALUE$ }\n",
    )
    .expect("Vanilla macro");
    std::fs::write(
        current.join("common/scripted_effects/00_priority.txt"),
        "priority_macro = { custom_tooltip = $VALUE$ }\n",
    )
    .expect("current macro");
    std::fs::write(
        current.join("localisation/priority_l_english.yml"),
        "l_english:\n current_macro_tip:0 \"Current macro tip\"\n",
    )
    .expect("current localisation");
    let rules = pdx_game::eu4::first_party_rules().expect("first-party rules");
    let mut vanilla_host = eu4_host(rules.clone());
    vanilla_host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(0),
        SourceRootKind::Vanilla,
        vanilla.clone(),
    )]));
    vanilla_host.refresh_source_roots().expect("scan Vanilla");
    let cache = IndexCache::from_snapshot(&vanilla_host.snapshot()).expect("build cache");
    std::fs::remove_dir_all(&vanilla).expect("discard Vanilla source");

    let mut host = eu4_host(rules);
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(1),
        SourceRootKind::CurrentMod,
        current,
    )]));
    host.refresh_source_roots().expect("scan current Mod");
    host.install_index_cache(cache).expect("install cache");
    let id = DocumentId::new("file:///tmp/events/priority-macro.txt");
    let text = "country_event = { immediate = { priority_macro = { VALUE =  } } }\n";
    host.open_document(id.clone(), 1, text.to_owned(), None)
        .expect("open call");
    let position = u32::try_from(text.find("=  }").expect("empty value") + 2).expect("position");
    let items = complete(&host.snapshot(), &id, position).items;

    assert!(
        items.iter().any(|item| item.label == "current_macro_tip"),
        "current Mod macro template did not win: {items:?}"
    );
    assert!(
        items
            .iter()
            .all(|item| item.label != "yes" && item.label != "no"),
        "cached Vanilla macro template leaked through an active override: {items:?}"
    );
    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn scripted_macro_body_completes_owner_local_dollar_parameters() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-analysis-macro-param-complete-{nonce}"));
    let path = root.join("common/scripted_effects/00_complete.txt");
    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(1),
        SourceRootKind::CurrentMod,
        root,
    )]));
    let id = DocumentId::new("file:///tmp/common/scripted_effects/00_complete.txt");
    let text =
        "probe = { add_prestige = $PRESTIGE$ custom_tooltip = $TOOLTIP$ add_stability = $ }\n";
    host.open_document(id.clone(), 1, text.to_owned(), Some(path))
        .expect("open macro definition");
    let position =
        u32::try_from(text.find("$ }").expect("incomplete parameter") + 1).expect("position");

    let result = complete(&host.snapshot(), &id, position);
    let labels = result
        .items
        .iter()
        .map(|item| item.label.as_str())
        .collect::<Vec<_>>();
    assert!(labels.contains(&"$PRESTIGE$"), "{labels:?}");
    assert!(labels.contains(&"$TOOLTIP$"), "{labels:?}");
    assert!(result.items.iter().all(|item| {
        item.replacement_range == TextRange::new(position - 1, position).expect("range")
    }));
}

#[test]
fn dollar_completion_does_not_leak_parameters_between_macro_owners() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-analysis-macro-param-owner-{nonce}"));
    let path = root.join("common/scripted_effects/00_complete.txt");
    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(1),
        SourceRootKind::CurrentMod,
        root,
    )]));
    let id = DocumentId::new("file:///tmp/common/scripted_effects/00_complete.txt");
    let text = concat!(
        "first = { add_prestige = $FIRST$ }\n",
        "second = { add_prestige = $SECOND$ add_stability = $ }\n",
    );
    host.open_document(id.clone(), 1, text.to_owned(), Some(path))
        .expect("open macro definitions");
    let position =
        u32::try_from(text.rfind("$ }").expect("incomplete parameter") + 1).expect("position");

    let labels = complete(&host.snapshot(), &id, position)
        .items
        .into_iter()
        .map(|item| item.label)
        .collect::<Vec<_>>();
    assert!(labels.iter().any(|label| label == "$SECOND$"), "{labels:?}");
    assert!(!labels.iter().any(|label| label == "$FIRST$"), "{labels:?}");
}

#[test]
fn scripted_macro_dollar_completion_marks_key_usage() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-analysis-macro-key-complete-{nonce}"));
    let path = root.join("common/scripted_effects/00_complete.txt");
    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(1),
        SourceRootKind::CurrentMod,
        root,
    )]));
    let id = DocumentId::new("file:///tmp/common/scripted_effects/00_complete.txt");
    let text = "probe = { $EFFECT$ = yes $ = yes }\n";
    host.open_document(id.clone(), 1, text.to_owned(), Some(path))
        .expect("open macro definition");
    let position = u32::try_from(text.rfind("$ =").expect("incomplete key") + 1).expect("position");

    let item = complete(&host.snapshot(), &id, position)
        .items
        .into_iter()
        .find(|item| item.label == "$EFFECT$")
        .expect("key parameter completion");
    assert_eq!(item.kind, CompletionKind::MacroParameter);
    assert_eq!(item.detail, "macro parameter (key)");
}

#[test]
fn scope_value_completion_offers_intrinsics_links_and_chains() {
    let mut model = pdx_game::eu4::bootstrap_model();
    for (id, key, allowed, push) in [
        (
            "fixture:link:owner",
            "owner",
            vec!["province".to_owned()],
            "country",
        ),
        (
            "fixture:link:controller",
            "controller",
            vec!["province".to_owned()],
            "country",
        ),
        (
            "fixture:link:capital_scope",
            "capital_scope",
            vec!["country".to_owned()],
            "province",
        ),
        ("fixture:link:emperor", "emperor", Vec::new(), "country"),
    ] {
        model.semantic.rules.push(SemanticRule {
            id: id.to_owned(),
            context: "effect".to_owned(),
            parent_path: Vec::new(),
            key: KeyMatcher::Exact(key.to_owned()),
            operator: None,
            value: ValueMatcher::AnyScalar,
            shape: RuleShape::Node,
            child_context: Some("effect".to_owned()),
            alternative_id: None,
            severity: None,
            required: false,
            deprecated: false,
            documentation: Vec::new(),
            allowed_scopes: allowed,
            push_scope: Some(push.to_owned()),
            replace_scope: Vec::new(),
            min_occurs: None,
            strict_min: true,
            max_occurs: None,
            source_file: "fixture.semantic".to_owned(),
            line: 1,
        });
    }
    let host = eu4_host(RuleSet::from_model(model));
    let snapshot = host.snapshot();
    let context = crate::SemanticCompletionContext {
        context: "effect".to_owned(),
        parent_path: Vec::new(),
        structural_containers: Vec::new(),
        alternative_containers: Vec::new(),
        existing_keys: Vec::new(),
        macro_inferred: false,
        scope: crate::ScopeContext {
            profile: snapshot.game_profile_handle(),
            root: "province".to_owned(),
            current: "province".to_owned(),
            from: Vec::new(),
            previous: Vec::new(),
        },
        container_property: None,
        property: None,
        quoted_depth: 0,
        embedded_value_context: None,
        wrapper_container: false,
        root_entry_container: false,
    };
    let labels = crate::scope_expression_candidates(&snapshot, &context, Some("province"))
        .into_iter()
        .map(|(label, _)| label)
        .collect::<Vec<_>>();
    assert!(labels.iter().any(|label| label == "province"), "{labels:?}");
    assert!(
        labels.iter().any(|label| label == "THIS"),
        "intrinsics must stay visible: {labels:?}"
    );
    assert!(
        !labels.iter().any(|label| label == "owner"),
        "a single link targeting another scope must not be offered: {labels:?}"
    );
    assert!(
        labels.iter().any(|label| label == "owner.capital_scope"),
        "a one-hop chain back to the expected scope must be offered: {labels:?}"
    );
    assert!(
        labels
            .iter()
            .any(|label| label == "controller.capital_scope"),
        "{labels:?}"
    );

    let country_labels = crate::scope_expression_candidates(&snapshot, &context, Some("country"))
        .into_iter()
        .map(|(label, _)| label)
        .collect::<Vec<_>>();
    assert!(
        country_labels.iter().any(|label| label == "country"),
        "{country_labels:?}"
    );
    assert!(
        country_labels.iter().any(|label| label == "emperor"),
        "unrestricted scope links must be offered: {country_labels:?}"
    );
    assert!(
        !country_labels.iter().any(|label| label == "province"),
        "incompatible concrete scopes must be filtered: {country_labels:?}"
    );
    assert!(
        !country_labels.iter().any(|label| label == "trade_node"),
        "incompatible concrete scopes must be filtered: {country_labels:?}"
    );
}

#[test]
fn fuzzy_completion_prefers_prefix_over_substring_matches() {
    let mut host = eu4_host(pdx_game::eu4::bootstrap_rules());
    let def_id = DocumentId::new("file:///tmp/localisation/fuzzy-defs.yml");
    host.open_document(
        def_id.clone(),
        1,
        "l_english:\nfuzz_name:0 \"A\"\nrefuse_name:0 \"B\"\n".to_owned(),
        None,
    )
    .expect("open definitions");
    let id = DocumentId::new("file:///tmp/localisation/fuzzy-use.yml");
    let text = "l_english:\nfu";
    host.open_document(id.clone(), 1, text.to_owned(), None)
        .expect("open use site");
    let result = complete(
        &host.snapshot(),
        &id,
        u32::try_from(text.len()).expect("position"),
    );
    let prefix = result
        .items
        .iter()
        .position(|item| item.label == "fuzz_name")
        .expect("prefix match");
    let substring = result
        .items
        .iter()
        .position(|item| item.label == "refuse_name")
        .expect("substring match");
    assert!(
        prefix < substring,
        "prefix matches must sort before substring matches: {:?}",
        result.items
    );
}

#[test]
fn semantic_context_unavailable_returns_empty_completion() {
    let mut host = eu4_host(pdx_game::eu4::bootstrap_rules());
    let id = DocumentId::new("file:///tmp/common/events/test.txt");
    let text = "unknown_root = { eve";
    host.open_document(id.clone(), 1, text.to_owned(), None)
        .expect("open");
    let result = complete(
        &host.snapshot(),
        &id,
        u32::try_from(text.len()).expect("position"),
    );
    assert!(
        result.items.is_empty(),
        "an uncovered context must not fall back to unrelated candidates: {:?}",
        result.items
    );
}

#[test]
fn completion_detail_uses_bare_categories() {
    let trigger_text = "trigger = { fo";
    let (host, id) = semantic_snapshot(trigger_text);
    let result = complete(
        &host.snapshot(),
        &id,
        u32::try_from(trigger_text.find("fo").expect("prefix") + 1).expect("position"),
    );
    let foo = result
        .items
        .iter()
        .find(|item| item.label == "foo")
        .expect("trigger rule item");
    assert_eq!(foo.detail, "trigger");
    assert_eq!(foo.kind, CompletionKind::Command);

    let mut effect_model = pdx_game::eu4::bootstrap_model();
    effect_model.semantic.rules.push(SemanticRule {
        id: "fixture:effect:bar".to_owned(),
        context: "effect".to_owned(),
        parent_path: Vec::new(),
        key: KeyMatcher::Exact("bar".to_owned()),
        operator: None,
        value: ValueMatcher::AnyScalar,
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
    let effect_text = "effect = { ba";
    let mut host = eu4_host(RuleSet::from_model(effect_model));
    let id = DocumentId::new("file:///tmp/common/events/test.txt");
    host.open_document(id.clone(), 1, effect_text.to_owned(), None)
        .expect("open");
    let result = complete(
        &host.snapshot(),
        &id,
        u32::try_from(effect_text.find("ba").expect("prefix") + 1).expect("position"),
    );
    let bar = result
        .items
        .iter()
        .find(|item| item.label == "bar")
        .expect("effect rule item");
    assert_eq!(bar.detail, "effect");
    assert_eq!(bar.kind, CompletionKind::Command);

    let mut root_model = pdx_game::eu4::bootstrap_model();
    root_model.semantic.rules.push(SemanticRule {
        id: "fixture:root:baz".to_owned(),
        context: "root:government_reform".to_owned(),
        parent_path: Vec::new(),
        key: KeyMatcher::Exact("baz".to_owned()),
        operator: None,
        value: ValueMatcher::AnyScalar,
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
    let root_text = "government_reform = { ba";
    let mut host = eu4_host(RuleSet::from_model(root_model));
    let id = DocumentId::new("file:///tmp/common/events/test.txt");
    host.open_document(id.clone(), 1, root_text.to_owned(), None)
        .expect("open");
    let result = complete(
        &host.snapshot(),
        &id,
        u32::try_from(root_text.find("ba").expect("prefix") + 1).expect("position"),
    );
    let baz = result
        .items
        .iter()
        .find(|item| item.label == "baz")
        .expect("root rule item");
    assert_eq!(baz.detail, "government_reform");
    assert_eq!(baz.kind, CompletionKind::Key);

    let mut enum_model = pdx_game::eu4::bootstrap_model();
    enum_model
        .semantic
        .enum_values
        .insert("fixture_enum".to_owned(), vec!["member_a".to_owned()]);
    enum_model.semantic.rules.push(SemanticRule {
        id: "fixture:enum:qux".to_owned(),
        context: "trigger".to_owned(),
        parent_path: Vec::new(),
        key: KeyMatcher::Enum("fixture_enum".to_owned()),
        operator: None,
        value: ValueMatcher::AnyScalar,
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
    let enum_text = "trigger = { ";
    let mut host = eu4_host(RuleSet::from_model(enum_model));
    let id = DocumentId::new("file:///tmp/common/events/test.txt");
    host.open_document(id.clone(), 1, enum_text.to_owned(), None)
        .expect("open");
    let result = complete(
        &host.snapshot(),
        &id,
        u32::try_from(enum_text.len().saturating_sub(1)).expect("position"),
    );
    let member = result
        .items
        .iter()
        .find(|item| item.label == "member_a")
        .expect("enum member item");
    assert_eq!(member.detail, "fixture_enum");
    assert_eq!(member.kind, CompletionKind::EnumMember);
}

#[test]
fn dynamic_value_completion_covers_scope_expressions_and_same_named_enums() {
    let mut model = pdx_game::eu4::bootstrap_model();
    model.semantic.enum_values.insert(
        "fixture_dynamic".to_owned(),
        vec!["member_a".to_owned(), "member_b".to_owned()],
    );
    for (id, key, value) in [
        (
            "fixture:dynamic-scope",
            "dynamic_scope",
            ValueMatcher::Dynamic("scope_field".to_owned()),
        ),
        (
            "fixture:dynamic-enum",
            "dynamic_enum",
            ValueMatcher::Dynamic("fixture_dynamic".to_owned()),
        ),
    ] {
        model.semantic.rules.push(SemanticRule {
            id: id.to_owned(),
            context: "trigger".to_owned(),
            parent_path: Vec::new(),
            key: KeyMatcher::Exact(key.to_owned()),
            operator: None,
            value,
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
    }
    let mut host = eu4_host(RuleSet::from_model(model.clone()));
    let id = DocumentId::new("file:///tmp/common/events/test.txt");

    // A scope-field dynamic value completes scope expressions and variable names.
    let scope_text = "trigger = { dynamic_scope = ";
    host.open_document(id.clone(), 1, scope_text.to_owned(), None)
        .expect("open scope document");
    let scope_result = complete(
        &host.snapshot(),
        &id,
        u32::try_from(scope_text.len()).expect("position"),
    );
    assert!(
        scope_result.items.iter().any(|item| item.label == "ROOT"),
        "a scope_field dynamic value must complete scope expressions: {:?}",
        scope_result
            .items
            .iter()
            .map(|item| item.label.as_str())
            .collect::<Vec<_>>()
    );
    assert!(
        scope_result
            .items
            .iter()
            .filter(|item| item.label == "ROOT")
            .all(|item| item.kind == CompletionKind::Scope),
        "scope expressions must use the scope completion kind: {:?}",
        scope_result.items
    );

    // A dynamic value with a same-named static enum completes the enum members.
    let enum_text = "trigger = { dynamic_enum = memb";
    host.stage_document_text(&id, 2, enum_text.to_owned())
        .expect("stage enum document");
    let prepared = host
        .snapshot()
        .prepare_document(&id)
        .expect("prepare enum document");
    assert!(host.commit_prepared_document(prepared));
    let enum_result = complete(
        &host.snapshot(),
        &id,
        u32::try_from(enum_text.find("memb").expect("prefix") + 2).expect("position"),
    );
    assert!(
        enum_result
            .items
            .iter()
            .any(|item| item.label == "member_a"),
        "a dynamic value must complete same-named static enum members: {:?}",
        enum_result
            .items
            .iter()
            .map(|item| item.label.as_str())
            .collect::<Vec<_>>()
    );
    assert!(
        enum_result
            .items
            .iter()
            .any(|item| item.label == "member_b")
    );
    assert!(
        enum_result
            .items
            .iter()
            .filter(|item| item.label == "member_a" || item.label == "member_b")
            .all(|item| item.kind == CompletionKind::EnumMember),
        "enum members must use the enum completion kind: {:?}",
        enum_result.items
    );

    // An ordinary value rule also completes when the cursor sits directly after `key = ` at
    // the end of the line (the half-open property range boundary).
    model.semantic.rules.push(SemanticRule {
        id: "fixture:bool-value".to_owned(),
        context: "trigger".to_owned(),
        parent_path: Vec::new(),
        key: KeyMatcher::Exact("bool_value".to_owned()),
        operator: None,
        value: ValueMatcher::Bool,
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
    let bool_text = "trigger = { bool_value = ";
    host.open_document(id.clone(), 1, bool_text.to_owned(), None)
        .expect("open bool document");
    let bool_result = complete(
        &host.snapshot(),
        &id,
        u32::try_from(bool_text.len()).expect("position"),
    );
    assert!(
        bool_result.items.iter().any(|item| item.label == "yes"),
        "a value rule must complete after an unfinished assignment: {:?}",
        bool_result
            .items
            .iter()
            .map(|item| item.label.as_str())
            .collect::<Vec<_>>()
    );
}

#[test]
fn deprecated_semantic_rules_are_flagged_and_sorted_below() {
    let mut model = pdx_game::eu4::bootstrap_model();
    for (id, key, deprecated) in [
        ("fixture:trigger:foo", "foo", false),
        ("fixture:trigger:foobar", "foobar", true),
    ] {
        model.semantic.rules.push(SemanticRule {
            id: id.to_owned(),
            context: "trigger".to_owned(),
            parent_path: Vec::new(),
            key: KeyMatcher::Exact(key.to_owned()),
            operator: None,
            value: ValueMatcher::Bool,
            shape: RuleShape::Leaf,
            child_context: None,
            alternative_id: None,
            severity: None,
            required: false,
            deprecated,
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
    }
    let mut host = eu4_host(RuleSet::from_model(model));
    let id = DocumentId::new("file:///tmp/common/events/test.txt");
    let text = "trigger = { foo";
    host.open_document(id.clone(), 1, text.to_owned(), None)
        .expect("open");
    let result = complete(
        &host.snapshot(),
        &id,
        u32::try_from(text.find("foo").expect("prefix") + 1).expect("position"),
    );
    let current = result
        .items
        .iter()
        .find(|item| item.label == "foo")
        .expect("current rule item");
    assert!(!current.deprecated, "current rules must not be flagged");
    let deprecated_item = result
        .items
        .iter()
        .find(|item| item.label == "foobar")
        .expect("deprecated rule item");
    assert!(
        deprecated_item.deprecated,
        "deprecated rules must be flagged"
    );
    let current_index = result
        .items
        .iter()
        .position(|item| item.label == "foo")
        .expect("current index");
    let deprecated_index = result
        .items
        .iter()
        .position(|item| item.label == "foobar")
        .expect("deprecated index");
    assert!(
        current_index < deprecated_index,
        "deprecated rules must sort below current rules: {:?}",
        result.items
    );
}

#[test]
fn key_completion_inserts_equals_for_scalars_and_skeletons_for_blocks() {
    let mut model = pdx_game::eu4::bootstrap_model();
    for (id, key, shape) in [
        ("fixture:trigger:foo", "foo", RuleShape::Leaf),
        ("fixture:trigger:bar", "bar", RuleShape::Node),
    ] {
        model.semantic.rules.push(SemanticRule {
            id: id.to_owned(),
            context: "trigger".to_owned(),
            parent_path: Vec::new(),
            key: KeyMatcher::Exact(key.to_owned()),
            operator: None,
            value: ValueMatcher::AnyScalar,
            shape,
            child_context: if shape == RuleShape::Node {
                Some("trigger".to_owned())
            } else {
                None
            },
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
    }
    let scalar = "trigger = { fo";
    let (scalar_host, scalar_id) = {
        let mut host = eu4_host(RuleSet::from_model(model.clone()));
        let id = DocumentId::new("file:///tmp/common/events/test.txt");
        host.open_document(id.clone(), 1, scalar.to_owned(), None)
            .expect("open");
        (host, id)
    };
    let scalar_result = complete(
        &scalar_host.snapshot(),
        &scalar_id,
        u32::try_from(scalar.find("fo").expect("prefix") + 1).expect("position"),
    );
    let foo = scalar_result
        .items
        .iter()
        .find(|item| item.label == "foo")
        .expect("scalar rule item");
    assert_eq!(foo.insert_text, "foo = ");

    let existing = "trigger = { ba = yes }";
    let mut existing_host = eu4_host(RuleSet::from_model(model.clone()));
    let existing_id = DocumentId::new("file:///tmp/common/events/test.txt");
    existing_host
        .open_document(existing_id.clone(), 1, existing.to_owned(), None)
        .expect("open existing assignment");
    let existing_result = complete(
        &existing_host.snapshot(),
        &existing_id,
        u32::try_from(existing.find("ba").expect("existing key") + 1).expect("position"),
    );
    let replacement = existing_result
        .items
        .iter()
        .find(|item| item.label == "bar")
        .expect("replacement key item");
    assert_eq!(replacement.insert_text, "bar");

    let block = "trigger = {\n\tba";
    let mut block_host = eu4_host(RuleSet::from_model(model));
    let block_id = DocumentId::new("file:///tmp/common/events/test.txt");
    block_host
        .open_document(block_id.clone(), 1, block.to_owned(), None)
        .expect("open");
    let block_result = complete(
        &block_host.snapshot(),
        &block_id,
        u32::try_from(block.find("ba").expect("prefix") + 1).expect("position"),
    );
    let bar = block_result
        .items
        .iter()
        .find(|item| item.label == "bar")
        .expect("block rule item");
    assert_eq!(bar.insert_text, "bar = {\n\t$0\n}");
}

#[test]
fn if_limit_trigger_context_completes_trigger_keys() {
    let text = concat!(
        "country_event = {\n",
        "  trigger = {\n",
        "    if = {\n",
        "      limit = {\n",
        "        has_glob\n",
        "      }\n",
        "    }\n",
        "  }\n",
        "}\n",
    );
    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    let id = DocumentId::new("file:///tmp/events/if-limit-trigger.txt");
    host.open_document(id.clone(), 1, text.to_owned(), None)
        .expect("open");
    let snapshot = host.snapshot();
    let offset = text.find("has_glob").expect("completion prefix");
    let position = u32::try_from(offset).expect("position").saturating_add(8);
    let input = input_for_document(&snapshot, &id).expect("analysis input");
    let context = semantic_completion_context(&snapshot, &input, position)
        .expect("semantic completion context");
    let completion = complete(&snapshot, &id, position);
    assert!(
        completion
            .items
            .iter()
            .any(|item| item.label == "has_global_flag"),
        "trigger-side if/limit block must complete trigger keys: context={} path={:?} items={:?}",
        context.context,
        context.parent_path,
        completion
            .items
            .iter()
            .map(|item| item.label.as_str())
            .collect::<Vec<_>>()
    );
}

#[test]
fn if_limit_effect_context_completes_trigger_keys() {
    let text = concat!(
        "country_event = {\n",
        "  option = {\n",
        "    if = {\n",
        "      limit = {\n",
        "        has_glob\n",
        "      }\n",
        "      add_prestige = 1\n",
        "    }\n",
        "  }\n",
        "}\n",
    );
    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    let id = DocumentId::new("file:///tmp/events/if-limit-effect.txt");
    host.open_document(id.clone(), 1, text.to_owned(), None)
        .expect("open");
    let snapshot = host.snapshot();
    let offset = text.find("has_glob").expect("completion prefix");
    let position = u32::try_from(offset).expect("position").saturating_add(8);
    let input = input_for_document(&snapshot, &id).expect("analysis input");
    let context = semantic_completion_context(&snapshot, &input, position)
        .expect("semantic completion context");
    let completion = complete(&snapshot, &id, position);
    assert!(
        completion
            .items
            .iter()
            .any(|item| item.label == "has_global_flag"),
        "effect-side if/limit trigger block must complete trigger keys: context={} path={:?} items={:?}",
        context.context,
        context.parent_path,
        completion
            .items
            .iter()
            .map(|item| item.label.as_str())
            .collect::<Vec<_>>()
    );
}

#[test]
fn event_option_members_rank_before_effect_commands() {
    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    let id = DocumentId::new("file:///tmp/events/event-option-order.txt");

    let partial = "country_event = { option = { a } }\n";
    host.open_document(id.clone(), 1, partial.to_owned(), None)
        .expect("open partial option");
    let partial_position =
        u32::try_from(partial.find("a }").expect("partial prefix") + 1).expect("position");
    let partial_items = complete(&host.snapshot(), &id, partial_position).items;
    let option_member = partial_items
        .iter()
        .position(|item| item.label == "ai_chance")
        .expect("event.option member");
    let effect_command = partial_items
        .iter()
        .position(|item| item.label == "add_prestige")
        .expect("effect command");
    assert!(
        option_member < effect_command,
        "explicit event.option members must precede effect commands for a shared prefix: {:?}",
        partial_items
            .iter()
            .take(effect_command.saturating_add(1))
            .map(|item| (&item.label, &item.detail))
            .collect::<Vec<_>>()
    );

    let empty = "country_event = { option = {  } }\n";
    let empty_id = DocumentId::new("file:///tmp/events/event-option-empty.txt");
    host.open_document(empty_id.clone(), 1, empty.to_owned(), None)
        .expect("open empty option");
    let empty_position =
        u32::try_from(empty.find("  }").expect("empty option") + 1).expect("position");
    let empty_items = complete(&host.snapshot(), &empty_id, empty_position).items;
    let first_effect = empty_items
        .iter()
        .position(|item| item.detail == "effect")
        .expect("effect candidates");
    for label in ["ai_chance", "name", "required_personality"] {
        let position = empty_items
            .iter()
            .position(|item| item.label == label && item.detail == "event")
            .unwrap_or_else(|| panic!("missing explicit event.option member `{label}`"));
        assert!(
            position < first_effect,
            "explicit event.option member `{label}` must precede effect candidates: {empty_items:?}"
        );
    }
    assert!(
        empty_items[..first_effect]
            .iter()
            .any(|item| item.label == "ai_chance" && item.detail == "event"),
        "the explicit option member tier must be materialized before effect candidates: {:?}",
        empty_items
            .iter()
            .take(first_effect.saturating_add(1))
            .map(|item| (&item.label, &item.detail))
            .collect::<Vec<_>>()
    );
}

#[test]
fn scope_link_limit_clauses_complete_trigger_keys() {
    let limit_blocks = [
        "while = { limit = { has_glob } }",
        "else_if = { limit = { has_glob } }",
        "every_province = { limit = { has_glob } }",
        "random_owned_province = { limit = { has_glob } }",
    ];
    for line in limit_blocks {
        let text = format!("country_event = {{ option = {{ {line} }} }}\n");
        let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
        let id = DocumentId::new("file:///tmp/events/scope-link-limit.txt");
        host.open_document(id.clone(), 1, text.to_owned(), None)
            .expect("open");
        let snapshot = host.snapshot();
        let offset = text.find("has_glob").expect("completion prefix");
        let position = u32::try_from(offset).expect("position").saturating_add(8);
        let completion = complete(&snapshot, &id, position);
        assert!(
            completion
                .items
                .iter()
                .any(|item| item.label == "has_global_flag"),
            "limit clause under `{line}` must complete trigger keys: {:?}",
            completion
                .items
                .iter()
                .map(|item| item.label.as_str())
                .collect::<Vec<_>>()
        );
    }
}

#[test]
fn limit_clause_value_position_completes_bool_values() {
    let text = concat!(
        "country_event = {\n",
        "  option = {\n",
        "    if = {\n",
        "      limit = { always = \n",
        "    }\n",
        "  }\n",
        "}\n",
    );
    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    let id = DocumentId::new("file:///tmp/events/limit-value.txt");
    host.open_document(id.clone(), 1, text.to_owned(), None)
        .expect("open");
    let snapshot = host.snapshot();
    let offset = text.find("always = ").expect("value position");
    let position = u32::try_from(offset).expect("position").saturating_add(9);
    let completion = complete(&snapshot, &id, position);
    assert!(
        completion.items.iter().any(|item| item.label == "yes"),
        "bool value must complete after a trigger key inside a limit clause: {:?}",
        completion
            .items
            .iter()
            .map(|item| item.label.as_str())
            .collect::<Vec<_>>()
    );
}

#[test]
fn if_block_key_completion_still_offers_limit_clause() {
    let text = concat!(
        "country_event = {\n",
        "  option = {\n",
        "    if = {\n",
        "      li\n",
        "    }\n",
        "  }\n",
        "}\n",
    );
    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    let id = DocumentId::new("file:///tmp/events/if-level-key.txt");
    host.open_document(id.clone(), 1, text.to_owned(), None)
        .expect("open");
    let snapshot = host.snapshot();
    let offset = text.find("li").expect("completion prefix");
    let position = u32::try_from(offset).expect("position").saturating_add(2);
    let completion = complete(&snapshot, &id, position);
    assert!(
        completion.items.iter().any(|item| item.label == "limit"),
        "key completion inside an `if` block (before its body) must still offer `limit`: {:?}",
        completion
            .items
            .iter()
            .map(|item| item.label.as_str())
            .collect::<Vec<_>>()
    );
}

#[test]
fn file_root_scaffolds_use_rule_backed_entry_containers() {
    use std::path::PathBuf;

    // An empty decisions file resolves to the rule-backed `root:decision_entries` container
    // rather than a profile side table.
    let mut decision_host =
        eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    let decision_id = DocumentId::new("file:///tmp/decisions/entry-context.txt");
    decision_host
        .open_document(
            decision_id.clone(),
            1,
            "\n".to_owned(),
            Some(PathBuf::from("decisions/entry-context.txt")),
        )
        .expect("open decision document");
    let snapshot = decision_host.snapshot();
    let input = input_for_document(&snapshot, &decision_id).expect("decision input");
    let context =
        semantic_completion_context(&snapshot, &input, 0).expect("decision entry context");
    assert_eq!(context.context, "root:decision_entries");
    assert!(context.parent_path.is_empty());

    // An empty events file resolves to `root:event_entries` with all four entries scaffolded
    // by ordinary semantic rules (blocks get skeletons, leaves the bare assignment).
    let mut event_host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    let event_id = DocumentId::new("file:///tmp/events/entry-context.txt");
    event_host
        .open_document(
            event_id.clone(),
            1,
            "\n".to_owned(),
            Some(PathBuf::from("events/entry-context.txt")),
        )
        .expect("open event document");
    let snapshot = event_host.snapshot();
    let input = input_for_document(&snapshot, &event_id).expect("event input");
    let context = semantic_completion_context(&snapshot, &input, 0).expect("event entry context");
    assert_eq!(context.context, "root:event_entries");
    assert!(context.parent_path.is_empty());
    let result = complete(&snapshot, &event_id, 0);
    let by_label = |label: &str| result.items.iter().find(|item| item.label == label);
    assert_eq!(
        by_label("country_event")
            .expect("country_event")
            .insert_text,
        "country_event = {\n\t$0\n}",
        "block entries must keep the rule-backed skeleton"
    );
    assert_eq!(
        by_label("normal_or_historical_nations")
            .expect("normal_or_historical_nations")
            .insert_text,
        "normal_or_historical_nations = "
    );
}

fn mission_probe(
    text: &str,
    needle: &str,
    path: &str,
) -> (Vec<String>, Option<(String, Vec<String>)>) {
    use std::path::PathBuf;

    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    let id = DocumentId::new(format!("file:///tmp/{path}"));
    host.open_document(id.clone(), 1, text.to_owned(), Some(PathBuf::from(path)))
        .expect("open mission document");
    let snapshot = host.snapshot();
    let position =
        u32::try_from(text.find(needle).expect("needle") + needle.len()).expect("position");
    let input = input_for_document(&snapshot, &id).expect("analysis input");
    let ctx = semantic_completion_context(&snapshot, &input, position)
        .map(|c| (c.context, c.parent_path.to_vec()));
    let result = complete(&snapshot, &id, position);
    let labels = result
        .items
        .iter()
        .map(|item| item.label.clone())
        .collect::<Vec<_>>();
    (labels, ctx)
}

fn assert_sorted_labels_eq(labels: &[String], expected: &[&str], what: &str) {
    let mut actual = labels.to_vec();
    actual.sort();
    let mut want = expected.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    want.sort();
    assert_eq!(actual, want, "{what}: {labels:?}");
}

#[test]
fn mission_file_root_and_root_gaps_offer_no_candidates() {
    // An empty missions file has a free-form series name on the root, so no key candidates.
    let (labels, ctx) = mission_probe("\n", "", "missions/root-entry.txt");
    assert!(
        labels.is_empty(),
        "empty root must not offer candidates: {labels:?}"
    );
    assert!(
        ctx.is_none(),
        "empty root has no semantic container: {ctx:?}"
    );

    // A root gap after a finished series must stay candidate-free for the next series name.
    let gap = "test_series = {\n  slot = 1\n}\n\n";
    let (labels, ctx) = mission_probe(gap, "\n\n", "missions/root-gap.txt");
    assert!(
        labels.is_empty(),
        "root gap must not offer candidates: {labels:?}"
    );
    assert!(ctx.is_none(), "root gap has no semantic container: {ctx:?}");
}

#[test]
fn mission_series_block_offers_exactly_the_series_keys() {
    let text = concat!(
        "test_series = {\n",
        "  slot = 1\n",
        "  generic = no\n",
        "  ai = yes\n",
        "  has_country_shield = no\n",
        "  \n",
        "}\n",
    );
    let (labels, ctx) = mission_probe(text, "  \n", "missions/series-block.txt");
    assert_eq!(ctx, Some(("type:mission_series".to_owned(), Vec::new())));
    assert_sorted_labels_eq(
        &labels,
        &[
            "ai",
            "generic",
            "has_country_shield",
            "potential",
            "potential_on_load",
            "slot",
        ],
        "series body",
    );
}

#[test]
fn mission_instance_body_offers_exactly_the_instance_keys() {
    let text = concat!(
        "test_series = {\n",
        "  slot = 1\n",
        "  mission_a = {\n",
        "    icon = mission_unknown position = 1\n",
        "    \n",
        "  }\n",
        "}\n",
    );
    let (labels, ctx) = mission_probe(text, "    \n", "missions/instance-body.txt");
    assert_eq!(
        ctx,
        Some((
            "type:mission_series".to_owned(),
            vec!["mission_a".to_owned()]
        )),
        "the instance key must become the semantic parent path"
    );
    assert_sorted_labels_eq(
        &labels,
        &[
            "ai_weight",
            "completed_by",
            "effect",
            "icon",
            "position",
            "provinces_to_highlight",
            "required_missions",
            "trigger",
        ],
        "instance body",
    );
}

#[test]
fn mission_potential_trigger_and_effect_blocks_enter_scoped_contexts() {
    let potential_text = concat!(
        "test_series = {\n",
        "  potential = {\n",
        "    \n",
        "  }\n",
        "  mission_a = {\n",
        "    icon = mission_unknown position = 1\n",
        "    trigger = {\n",
        "      \n",
        "    }\n",
        "  }\n",
        "}\n",
    );
    // Series-level potential enters the generic trigger context; the instance-level
    // specialization rule must not leak into it.
    let (labels, ctx) = mission_probe(potential_text, "    \n", "missions/series-potential.txt");
    assert_eq!(ctx, Some(("trigger".to_owned(), Vec::new())));
    for label in ["ai", "always", "custom_trigger_tooltip", "has_country_flag"] {
        assert!(
            labels.iter().any(|item| item == label),
            "series potential must offer trigger `{label}`: {labels:?}"
        );
    }
    assert!(
        !labels.iter().any(|item| item == "mil_ideas"),
        "the mission-only specialization leaf must not appear in series potential: {labels:?}"
    );
    assert!(
        !labels.iter().any(|item| item == "slot"),
        "series keys must not leak into the trigger context: {labels:?}"
    );

    // The instance trigger adds the mission specialization rule.
    let (labels, ctx) = mission_probe(potential_text, "      \n", "missions/instance-trigger.txt");
    assert_eq!(ctx, Some(("trigger".to_owned(), Vec::new())));
    for label in [
        "ai",
        "has_country_flag",
        "has_completed_idea_group_of_category",
    ] {
        assert!(
            labels.iter().any(|item| item == label),
            "instance trigger must offer `{label}`: {labels:?}"
        );
    }

    // The instance effect enters the effect context.
    let effect_text = concat!(
        "test_series = {\n",
        "  slot = 1\n",
        "  mission_a = {\n",
        "    icon = mission_unknown position = 1\n",
        "    effect = {\n",
        "      \n",
        "    }\n",
        "  }\n",
        "}\n",
    );
    let (labels, ctx) = mission_probe(effect_text, "      \n", "missions/instance-effect.txt");
    assert_eq!(ctx, Some(("effect".to_owned(), Vec::new())));
    for label in [
        "add_country_modifier",
        "country_event",
        "custom_tooltip",
        "define_advisor",
        "hidden_effect",
    ] {
        assert!(
            labels.iter().any(|item| item == label),
            "instance effect must offer `{label}`: {labels:?}"
        );
    }
    assert!(
        !labels.iter().any(|item| item == "slot"),
        "series keys must not leak into the effect context: {labels:?}"
    );
}

#[test]
fn mission_provinces_to_highlight_scopes_trigger_to_province() {
    let text = concat!(
        "test_series = {\n",
        "  slot = 1\n",
        "  mission_a = {\n",
        "    icon = mission_unknown position = 1\n",
        "    provinces_to_highlight = {\n",
        "      \n",
        "    }\n",
        "  }\n",
        "}\n",
    );
    let (labels, ctx) = mission_probe(text, "      \n", "missions/provinces-to-highlight.txt");
    assert_eq!(ctx, Some(("trigger".to_owned(), Vec::new())));
    for label in ["area", "always", "base_tax", "province_id"] {
        assert!(
            labels.iter().any(|item| item == label),
            "province-scoped trigger context must offer `{label}`: {labels:?}"
        );
    }
    assert!(
        !labels.iter().any(|item| item == "add_country_modifier"),
        "effects must not appear in the province trigger context: {labels:?}"
    );
    assert!(
        !labels.iter().any(|item| item == "slot"),
        "series keys must not leak into the province trigger context: {labels:?}"
    );
}

#[test]
fn mission_ai_weight_offers_the_modifier_rule_keys() {
    let text = concat!(
        "test_series = {\n",
        "  slot = 1\n",
        "  mission_a = {\n",
        "    icon = mission_unknown position = 1\n",
        "    ai_weight = {\n",
        "      \n",
        "    }\n",
        "  }\n",
        "}\n",
    );
    let (labels, ctx) = mission_probe(text, "      \n", "missions/ai-weight.txt");
    assert_eq!(ctx, Some(("modifier_rule".to_owned(), Vec::new())));
    assert_sorted_labels_eq(&labels, &["factor", "modifier"], "ai_weight body");
}
