#![allow(unused_imports)]

use super::support::*;

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
        "",
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
    assert_eq!(snippet.insert_text, "apply = {\n\tzeta = $1\n\t$0\n}");
    assert_eq!(
        crate::semantic::scripted_definition_snippet(
            &host.snapshot(),
            "scripted_effect",
            "plain",
            ""
        ),
        "plain = yes"
    );
    assert_eq!(
        crate::semantic::scripted_definition_snippet(
            &host.snapshot(),
            "scripted_effect",
            "scalar",
            ""
        ),
        "scalar = {\n\tamount = $1\n\t$0\n}"
    );
    assert_eq!(
        crate::semantic::scripted_definition_snippet(
            &host.snapshot(),
            "scripted_effect",
            "optional_only",
            ""
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
    assert_eq!(amount.kind, CompletionKind::Key);
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

    assert!(
        items.iter().any(|item| item.label == "add_prestige"),
        "quoted macro argument did not inherit effect completion: {items:?}"
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
        scoped.iter().any(|item| item.label == "this"),
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
        nested_scoped.iter().any(|item| item.label == "this"),
        "macro body scope transition was not applied: {nested_scoped:?}"
    );

    let conflict = complete_argument(&host, "conflict", "conflict = { VALUE =  }");
    assert!(
        conflict.is_empty(),
        "incompatible use-site constraints must not offer guesses: {conflict:?}"
    );
    let cycle = complete_argument(&host, "cycle", "cycle_a = { VALUE =  }");
    assert!(
        cycle.is_empty(),
        "cyclic constraint inference must fall back without guessing: {cycle:?}"
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
    let cache = VanillaIndexCache::from_snapshot(&vanilla_host.snapshot()).expect("build cache");
    std::fs::remove_dir_all(&root).expect("discard Vanilla source");

    let mut host = eu4_host(rules);
    host.install_vanilla_cache(cache).expect("install cache");
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
    let cache = VanillaIndexCache::from_snapshot(&vanilla_host.snapshot()).expect("build cache");
    std::fs::remove_dir_all(&root).expect("discard Vanilla source");

    let mut host = eu4_host(rules);
    host.install_vanilla_cache(cache).expect("install cache");
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
    let cache = VanillaIndexCache::from_snapshot(&vanilla_host.snapshot()).expect("build cache");
    std::fs::remove_dir_all(&root).expect("discard Vanilla source");

    let mut host = eu4_host(rules);
    host.install_vanilla_cache(cache).expect("install cache");
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
        scoped.iter().any(|item| item.label == "this"),
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
    let cache = VanillaIndexCache::from_snapshot(&vanilla_host.snapshot()).expect("build cache");
    std::fs::remove_dir_all(&vanilla).expect("discard Vanilla source");

    let mut host = eu4_host(rules);
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(1),
        SourceRootKind::CurrentMod,
        current,
    )]));
    host.refresh_source_roots().expect("scan current Mod");
    host.install_vanilla_cache(cache).expect("install cache");
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
    assert_eq!(item.kind, CompletionKind::Key);
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
    };
    let labels = crate::scope_expression_candidates(&snapshot, &context, Some("province"))
        .into_iter()
        .map(|(label, _)| label)
        .collect::<Vec<_>>();
    assert!(labels.iter().any(|label| label == "province"), "{labels:?}");
    assert!(
        labels.iter().any(|label| label == "this"),
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
    assert_eq!(bar.insert_text, "bar = {\n\t\t$0\n\t}");
}
