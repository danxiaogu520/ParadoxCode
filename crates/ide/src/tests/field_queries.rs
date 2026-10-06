//! Field query editor regressions use a small, repository-owned rule package.
use super::support::*;
use crate::completion_resolve;
use serde_json::json;

fn query_host() -> AnalysisHost {
    super::support::fixture_host(json!({
        "traits":{"Template":{}},
        "types":{
            "government_mechanic_power":{}, "estate":{}, "faction":{}, "estate_modifier":{}, "culture":{},
            "fixture_template":{"resolution":"replace","impl":{"Template":{"body":"effect"}}},
            "scripted_trigger":{"resolution":"replace","impl":{"Template":{"body":"trigger"}}}
        },
        "enums":{"numeric_strings":["1","2"]},
        "schemas":{
            "fixture_root":{"fields":{
                "powers":{"body":"powers"}, "estates":{"body":"estates"}, "factions":{"body":"factions"},
                "estate_modifiers":{"body":"estate_modifiers"}, "cultures":{"body":"cultures"},
                "templates":{"body":"templates"}, "scripted_triggers":{"body":"scripted_triggers"}
            }},
            "powers":{"map":{"key":"def<government_mechanic_power>","value":"bool"}},
            "estates":{"map":{"key":"def<estate>","value":"bool"}},
            "factions":{"map":{"key":"def<faction>","value":"bool"}},
            "estate_modifiers":{"map":{"key":"def<estate_modifier>","value":"bool"}},
            "cultures":{"map":{"key":"def<culture>","value":"bool"}},
            "templates":{"map":{"key":"def<fixture_template>","body":"effect"}},
            "scripted_triggers":{"map":{"key":"def<scripted_trigger>","body":"trigger"}},
            "arguments":{"map":{"key":"scalar","value":"scalar"}},
            "modifier":{"fields":{
                "country_only":{"value":"float","scope":{"in":["country"]}},
                "province_only":{"value":"float","scope":{"in":["province"]}},
                "monthly_shadowed":{"value":"float","scope":{"in":["province"]}}
            },"patterns":[
                {"key":"'monthly_{ref<government_mechanic_power>}'","value":"float","doc":"Monthly mechanic power source.","scope":{"in":["country"]}},
                {"key":"'{ref<estate strip_prefix estate_>}_influence_modifier'","value":"float","scope":{"in":["country"]}},
                {"key":"'{ref<estate strip_prefix estate_>}_loyalty_modifier'","value":"float","scope":{"in":["country"]}},
                {"key":"'{ref<estate strip_prefix estate_>}_privilege_slots'","value":"float","scope":{"in":["country"]}},
                {"key":"'{ref<faction>}_influence'","value":"float","scope":{"in":["country"]}},
                {"key":"'{ref<government_mechanic_power>}_gain_modifier'","value":"float","scope":{"in":["country"]}},
                {"key":"ref<estate_modifier>","value":"float","scope":{"in":["country"]}}
            ]},
            "trigger":{"fields":{
                "always":{"value":"bool","doc":"Boolean trigger source."},
                "primary_culture":{"value":"ref<culture>","doc":"Culture trigger source."},
                "integer_test":{"value":"int"}, "number_union":{"value":"scalar|float|bool"},
                "text_only":{"value":"scalar"}, "numeric_enum":{"value":"enum<numeric_strings>"},
                "block_only":{"body":"effect"}
            },"patterns":[
                {"key":"ref<scripted_trigger>","value":"bool","capabilities":["trigger_value_export"],"doc":"Scripted boolean trigger."},
                {"key":"ref<scripted_trigger>","body":"arguments"}
            ]},
            "effect":{"fields":{
                "check_modifier":{"value":"keysof<modifier,scope_accepts=country>"},
                "numeric_selector":{"value":"keysof<trigger,shape=scalar,value_kind_any=(int|float|bool)>"},
                "export_data":{"value":"'trigger_value:{keysof<trigger,shape=scalar,value_kind_any=(int|float|bool),capability=trigger_value_export,call_args=none>}'"},
                "ordinary_reference":{"value":"ref<scripted_trigger>"},
                "switch":{"body":"switch"}, "do_effect":{"value":"bool"}
            },"patterns":[{"key":"ref<fixture_template>","body":"arguments"}]},
            "switch":{"fields":{"on_trigger":{"value":"keysof<trigger,shape=scalar,call_args=none>","card":"1"},
                "decorated":{"value":"'pre_{valuesof<trigger,key=sibling<on_trigger>,shape=scalar,call_args=none>}_post'"}},
                "patterns":[{"key":"valuesof<trigger,key=sibling<on_trigger>,shape=scalar,call_args=none>","body":"effect"}]}
        }
    }))
}

fn open(host: &mut AnalysisHost, name: &str, source: &str) -> DocumentId {
    let id = DocumentId::new(format!("file:///fixture/{name}.txt"));
    host.open_document(id.clone(), 1, source.to_owned(), None)
        .unwrap();
    id
}

fn labels(host: &AnalysisHost, id: &DocumentId, source: &str, marker: &str) -> Vec<String> {
    complete(
        &host.snapshot(),
        id,
        (source.find(marker).unwrap() + marker.len()) as u32,
    )
    .items
    .into_iter()
    .map(|item| item.label)
    .collect()
}

fn semantic_errors(host: &AnalysisHost, id: &DocumentId) -> Vec<crate::Diagnostic> {
    diagnostics(&host.snapshot(), id)
        .into_iter()
        .filter(|issue| {
            matches!(
                issue.code,
                DiagnosticCode::UnknownKey
                    | DiagnosticCode::InvalidValue
                    | DiagnosticCode::WrongScope
            )
        })
        .collect()
}

#[test]
fn field_queries_complete_seven_dynamic_modifier_families_with_source_navigation() {
    let mut host = query_host();
    let definitions = "powers = { mod_power = yes shadowed = yes } estates = { estate_mod = yes } factions = { mod_faction = yes } estate_modifiers = { mod_estate_bonus = yes }";
    let definitions_id = open(&mut host, "members", definitions);
    for (name, member) in [
        ("monthly_mod_power", "mod_power"),
        ("mod_influence_modifier", "estate_mod"),
        ("mod_loyalty_modifier", "estate_mod"),
        ("mod_privilege_slots", "estate_mod"),
        ("mod_faction_influence", "mod_faction"),
        ("mod_power_gain_modifier", "mod_power"),
        ("mod_estate_bonus", "mod_estate_bonus"),
    ] {
        let source = format!("country_event = {{ immediate = {{ check_modifier = {name} }} }}");
        let id = open(&mut host, name, &source);
        assert!(
            semantic_errors(&host, &id).is_empty(),
            "{name}: {:?}",
            semantic_errors(&host, &id)
        );
        assert!(
            labels(&host, &id, &source, "check_modifier = ").contains(&name.to_owned()),
            "{name}"
        );
        let pos = (source.find(name).unwrap()
            + name.find(member.trim_start_matches("estate_")).unwrap_or(0))
            as u32;
        let targets = definition(&host.snapshot(), &id, pos);
        assert!(
            targets
                .iter()
                .any(|target| target.document.as_ref() == Some(&definitions_id)),
            "{name}: {targets:?}"
        );
    }
    let source = "country_event = { immediate = { check_modifier =  } }";
    let id = open(&mut host, "all-modifiers", source);
    let items = complete(
        &host.snapshot(),
        &id,
        (source.find("check_modifier = ").unwrap() + 17) as u32,
    )
    .items;
    assert!(items.iter().any(|item| item.label == "country_only"));
    assert!(
        !items
            .iter()
            .any(|item| item.label == "province_only" || item.label == "monthly_shadowed")
    );
    let item = items
        .iter()
        .find(|item| item.label == "monthly_mod_power")
        .unwrap();
    assert_eq!(
        completion_resolve(&host.snapshot(), item)
            .documentation
            .as_deref(),
        Some("Monthly mechanic power source.")
    );
    let source = "country_event = { immediate = { check_modifier = monthly_mod_power } }";
    let id = open(&mut host, "query-hover", source);
    let value = hover(
        &host.snapshot(),
        &id,
        source.find("monthly_mod_power").unwrap() as u32,
    )
    .unwrap();
    assert!(
        value.contents.contains("Monthly mechanic power source."),
        "{}",
        value.contents
    );
    assert!(value.contents.contains("patterns/0"), "{}", value.contents);
}

#[test]
fn field_queries_numeric_unions_do_not_guess_scalar_or_enum_capabilities() {
    let mut host = query_host();
    let source = "country_event = { immediate = { numeric_selector =  } }";
    let id = open(&mut host, "numeric", source);
    let candidates = labels(&host, &id, source, "numeric_selector = ");
    for valid in ["always", "integer_test", "number_union"] {
        assert!(
            candidates.iter().any(|item| item == valid),
            "{candidates:?}"
        );
    }
    for invalid in ["text_only", "numeric_enum", "primary_culture", "block_only"] {
        assert!(
            !candidates.iter().any(|item| item == invalid),
            "{candidates:?}"
        );
    }
}

#[test]
fn field_queries_switch_uses_current_physical_sibling_and_tracks_edits() {
    let mut host = query_host();
    open(&mut host, "cultures", "cultures = { mod_culture = yes }");
    let source = "country_event = { immediate = { switch = { on_trigger = always yes = { }  } switch = { on_trigger = primary_culture mod_culture = { }  } } }";
    let id = open(&mut host, "switch", source);
    assert!(
        semantic_errors(&host, &id).is_empty(),
        "{:?}",
        semantic_errors(&host, &id)
    );
    let bool_candidates = labels(&host, &id, source, "on_trigger = always yes = { } ");
    assert!(
        bool_candidates.contains(&"no".to_owned()),
        "{bool_candidates:?}"
    );
    assert!(!bool_candidates.contains(&"mod_culture".to_owned()));
    let culture_candidates = labels(
        &host,
        &id,
        source,
        "on_trigger = primary_culture mod_culture = { } ",
    );
    assert!(
        culture_candidates.contains(&"mod_culture".to_owned()),
        "{culture_candidates:?}"
    );
    assert!(!culture_candidates.contains(&"yes".to_owned()));
    let edited = source.replacen("on_trigger = always", "on_trigger = primary_culture", 1);
    host.apply_document_changes(&id, 2, &[engine::TextChange::full(edited.clone())])
        .unwrap();
    assert!(
        semantic_errors(&host, &id)
            .iter()
            .any(|issue| issue.message.contains("yes")),
        "{:?}",
        semantic_errors(&host, &id)
    );
    let candidates = labels(
        &host,
        &id,
        &edited,
        "on_trigger = primary_culture yes = { } ",
    );
    assert!(
        candidates.contains(&"mod_culture".to_owned()),
        "{candidates:?}"
    );
    assert!(!candidates.contains(&"yes".to_owned()));
    let selector = "country_event = { immediate = { switch = { on_trigger =  } } }";
    let selector_id = open(&mut host, "selector", selector);
    let candidates = labels(&host, &selector_id, selector, "on_trigger = ");
    assert!(candidates.contains(&"always".to_owned()));
    assert!(!candidates.contains(&"block_only".to_owned()));
}

#[test]
fn field_queries_missing_duplicate_invalid_selectors_never_offer_all_trigger_values() {
    let mut host = query_host();
    open(&mut host, "cultures", "cultures = { mod_culture = yes }");
    for selector in [
        "",
        "on_trigger = always on_trigger = primary_culture",
        "on_trigger = { }",
        "on_trigger = nonexistent",
    ] {
        let source = format!("country_event = {{ immediate = {{ switch = {{ {selector}  }} }} }}");
        let id = open(
            &mut host,
            &format!("bad-selector-{}", selector.len()),
            &source,
        );
        let candidates = complete(
            &host.snapshot(),
            &id,
            (source.find("  }").unwrap() + 1) as u32,
        )
        .items;
        assert!(
            !candidates
                .iter()
                .any(|item| matches!(item.label.as_str(), "yes" | "no" | "mod_culture")),
            "{selector}: {candidates:?}"
        );
    }
}

#[test]
fn field_queries_templates_bind_selector_and_keep_dynamic_members_live() {
    let mut host = query_host();
    let members = open(
        &mut host,
        "members",
        "cultures = { mod_culture = yes } powers = { mod_power = yes }",
    );
    open(
        &mut host,
        "templates",
        "templates = { choose = { switch = { on_trigger = $SELECTOR$ $CASE$ = { do_effect = yes } } } modifier_check = { check_modifier = $MODIFIER$ } }",
    );
    let source = "country_event = { immediate = { choose = { SELECTOR = always CASE =  } modifier_check = { MODIFIER =  } } }";
    let id = open(&mut host, "template-call", source);
    let candidates = labels(&host, &id, source, "CASE = ");
    assert!(candidates.contains(&"yes".to_owned()), "{candidates:?}");
    assert!(!candidates.contains(&"mod_culture".to_owned()));
    let candidates = labels(&host, &id, source, "MODIFIER = ");
    assert!(
        candidates.contains(&"monthly_mod_power".to_owned()),
        "{candidates:?}"
    );
    let items = complete(
        &host.snapshot(),
        &id,
        (source.find("CASE = ").unwrap() + "CASE = ".len()) as u32,
    )
    .items;
    let item = items.iter().find(|item| item.label == "yes").unwrap();
    assert_eq!(
        item.documentation.as_deref(),
        Some("Boolean trigger source.")
    );
    let items = complete(
        &host.snapshot(),
        &id,
        (source.find("MODIFIER = ").unwrap() + "MODIFIER = ".len()) as u32,
    )
    .items;
    let item = items
        .iter()
        .find(|item| item.label == "monthly_mod_power")
        .unwrap();
    assert_eq!(
        item.documentation.as_deref(),
        Some("Monthly mechanic power source.")
    );
    let valid = "country_event = { immediate = { choose = { SELECTOR = primary_culture CASE = mod_culture } modifier_check = { MODIFIER = monthly_mod_power } } }";
    host.apply_document_changes(&id, 2, &[engine::TextChange::full(valid.to_owned())])
        .unwrap();
    assert!(
        semantic_errors(&host, &id).is_empty(),
        "{:?}",
        semantic_errors(&host, &id)
    );
    host.apply_document_changes(
        &members,
        2,
        &[engine::TextChange::full(
            "cultures = { different_culture = yes } powers = { changed_power = yes }",
        )],
    )
    .unwrap();
    assert!(
        !semantic_errors(&host, &id).is_empty(),
        "removed member must invalidate existing query use"
    );
    let completion = "country_event = { immediate = { modifier_check = { MODIFIER =  } } }";
    host.apply_document_changes(&id, 3, &[engine::TextChange::full(completion.to_owned())])
        .unwrap();
    let candidates = labels(&host, &id, completion, "MODIFIER = ");
    assert!(
        candidates.contains(&"monthly_changed_power".to_owned()),
        "{candidates:?}"
    );
    assert!(!candidates.contains(&"monthly_mod_power".to_owned()));
}

#[test]
fn field_queries_dynamic_scripted_trigger_export_and_switch_share_bool_domain() {
    let mut host = query_host();
    let definitions = "scripted_triggers = { mod_predicate = { always = yes } }";
    let declarations = open(&mut host, "scripted-triggers", definitions);
    let source = "country_event = { immediate = { export_data = trigger_value:mod_predicate switch = { on_trigger = mod_predicate yes = { }  } } }";
    let id = open(&mut host, "scripted-selector", source);
    assert!(
        semantic_errors(&host, &id).is_empty(),
        "{:?}",
        semantic_errors(&host, &id)
    );
    let candidates = labels(&host, &id, source, "on_trigger = mod_predicate yes = { } ");
    assert!(candidates.contains(&"no".to_owned()), "{candidates:?}");
    let exports = labels(&host, &id, source, "export_data = ");
    assert!(
        exports.contains(&"trigger_value:mod_predicate".to_owned()),
        "{exports:?}"
    );
    let pos = source.find("trigger_value:mod_predicate").unwrap() + "trigger_value:".len();
    assert!(
        definition(&host.snapshot(), &id, pos as u32)
            .iter()
            .any(|location| location.document.as_ref() == Some(&declarations))
    );
    open(
        &mut host,
        "scripted-switch-template",
        "templates = { dynamic_switch = { switch = { on_trigger = $SELECTOR$ $CASE$ = { } } } }",
    );
    let call =
        "country_event = { immediate = { dynamic_switch = { SELECTOR = mod_predicate CASE =  } } }";
    let call_id = open(&mut host, "dynamic-switch-call", call);
    let candidates = labels(&host, &call_id, call, "CASE = ");
    assert!(
        candidates.contains(&"yes".to_owned()) && candidates.contains(&"no".to_owned()),
        "{candidates:?}"
    );
    let invalid = source.replace("yes = { }", "7 = { }");
    host.apply_document_changes(&id, 2, &[engine::TextChange::full(invalid)])
        .unwrap();
    assert!(
        semantic_errors(&host, &id)
            .iter()
            .any(|issue| issue.message.contains('7'))
    );
    host.apply_document_changes(
        &declarations,
        2,
        &[engine::TextChange::full(
            definitions.replace("mod_predicate", "renamed_predicate"),
        )],
    )
    .unwrap();
    host.apply_document_changes(&id, 3, &[engine::TextChange::full(source.to_owned())])
        .unwrap();
    assert!(
        !semantic_errors(&host, &id).is_empty(),
        "renamed scripted member must invalidate uses"
    );
    let candidates = labels(&host, &id, source, "export_data = ");
    assert!(
        candidates.contains(&"trigger_value:renamed_predicate".to_owned()),
        "{candidates:?}"
    );
    assert!(!candidates.contains(&"trigger_value:mod_predicate".to_owned()));
}

#[test]
fn field_queries_report_each_selector_failure_once_without_branch_noise() {
    let mut host = query_host();
    for (index, (selector, expected)) in [
        ("", "missing sibling selector"),
        (
            "on_trigger = always on_trigger = always",
            "duplicate sibling selector",
        ),
        ("on_trigger = { }", "must be a scalar"),
        ("on_trigger = nonexistent", "must be a scalar"),
    ]
    .into_iter()
    .enumerate()
    {
        let source = format!(
            "country_event = {{ immediate = {{ switch = {{ {selector} yes = {{ }} no = {{ }} }} }} }}"
        );
        let id = open(&mut host, &format!("selector-error-{index}"), &source);
        let errors = semantic_errors(&host, &id);
        assert_eq!(
            errors
                .iter()
                .filter(|issue| issue.message.contains(expected))
                .count(),
            1,
            "{errors:?}"
        );
        assert!(
            !errors
                .iter()
                .any(|issue| issue.code == DiagnosticCode::UnknownKey
                    && (issue.message.contains("`yes`") || issue.message.contains("`no`"))),
            "{errors:?}"
        );
    }
}

#[test]
fn field_queries_legacy_switch_control_uses_shared_selector_and_branch_domains() {
    let mut host = super::support::fixture_host(json!({
        "schemas":{
            "trigger":{"fields":{"always":{"value":"bool"},"number":{"value":"int"}}},
            "effect":{"fields":{"choose":{"body":"legacy_switch","control":{"kind":"switch","on":"selector","selector_schema":"trigger"}}}},
            "legacy_switch":{"fields":{"selector":{"value":"scalar"}},"patterns":[{"key":"scalar","body":"effect"}]}
        }
    }));
    let source = "country_event = { immediate = { choose = { selector =  } } }";
    let id = open(&mut host, "legacy-selector", source);
    let candidates = labels(&host, &id, source, "selector = ");
    assert!(candidates.contains(&"always".to_owned()), "{candidates:?}");
    let source = "country_event = { immediate = { choose = { selector = always  } } }";
    let id = open(&mut host, "legacy-branches", source);
    let candidates = labels(&host, &id, source, "selector = always ");
    assert!(
        candidates.contains(&"yes".to_owned()) && candidates.contains(&"no".to_owned()),
        "{candidates:?}"
    );
}

#[test]
fn field_queries_template_selector_edits_change_completion_and_diagnostics() {
    let mut host = query_host();
    open(
        &mut host,
        "edit-cultures",
        "cultures = { mod_culture = yes }",
    );
    let template = open(
        &mut host,
        "edit-template",
        "templates = { choose = { switch = { on_trigger = $SELECTOR$ $CASE$ = { } } } }",
    );
    assert!(
        semantic_errors(&host, &template).is_empty(),
        "unbound query must defer: {:?}",
        semantic_errors(&host, &template)
    );
    let source = "country_event = { immediate = { choose = { SELECTOR = always CASE = no } } }";
    let id = open(&mut host, "edit-call", source);
    assert!(semantic_errors(&host, &id).is_empty());
    let edited = source.replace("SELECTOR = always", "SELECTOR = primary_culture");
    host.apply_document_changes(&id, 2, &[engine::TextChange::full(&edited)])
        .unwrap();
    assert!(!semantic_errors(&host, &id).is_empty());
    let incomplete = edited.replace("CASE = no", "CASE = ");
    host.apply_document_changes(&id, 3, &[engine::TextChange::full(&incomplete)])
        .unwrap();
    let candidates = labels(&host, &id, &incomplete, "CASE = ");
    assert!(
        candidates.contains(&"mod_culture".to_owned()),
        "{candidates:?}"
    );
    assert!(!candidates.contains(&"yes".to_owned()) && !candidates.contains(&"no".to_owned()));
}

#[test]
fn field_queries_pattern_holes_keep_sibling_context_docs_and_navigation() {
    let mut host = query_host();
    let members = open(
        &mut host,
        "pattern-cultures",
        "cultures = { mod_culture = yes }",
    );
    let source = "country_event = { immediate = { switch = { on_trigger = always decorated = pre_yes_post } } }";
    let id = open(&mut host, "pattern-sibling", source);
    assert!(semantic_errors(&host, &id).is_empty());
    let items = complete(
        &host.snapshot(),
        &id,
        (source.find("decorated = ").unwrap() + "decorated = ".len()) as u32,
    )
    .items;
    let item = items
        .iter()
        .find(|item| item.label == "pre_yes_post")
        .unwrap();
    assert_eq!(
        item.documentation.as_deref(),
        Some("Boolean trigger source.")
    );
    let description = hover(
        &host.snapshot(),
        &id,
        source.find("pre_yes_post").unwrap() as u32,
    )
    .unwrap();
    assert!(
        description.contents.contains("Boolean trigger source.")
            && description.contents.contains("Query source:"),
        "{}",
        description.contents
    );
    let edited = source
        .replace("on_trigger = always", "on_trigger = primary_culture")
        .replace("pre_yes_post", "pre_mod_culture_post");
    host.apply_document_changes(&id, 2, &[engine::TextChange::full(&edited)])
        .unwrap();
    assert!(semantic_errors(&host, &id).is_empty());
    let position = edited.find("mod_culture_post").unwrap() as u32;
    assert!(
        definition(&host.snapshot(), &id, position)
            .iter()
            .any(|location| location.document.as_ref() == Some(&members))
    );
    let items = complete(
        &host.snapshot(),
        &id,
        (edited.find("decorated = ").unwrap() + "decorated = ".len()) as u32,
    )
    .items;
    let item = items
        .iter()
        .find(|item| item.label == "pre_mod_culture_post")
        .unwrap();
    assert_eq!(
        item.documentation.as_deref(),
        Some("Culture trigger source.")
    );
    assert!(!items.iter().any(|item| item.label == "pre_yes_post"));
}

#[test]
fn field_queries_template_pattern_holes_preserve_bound_sibling_context() {
    let mut host = query_host();
    open(
        &mut host,
        "pattern-template-members",
        "cultures = { mod_culture = yes }",
    );
    open(
        &mut host,
        "pattern-template",
        "templates = { patterned = { switch = { on_trigger = $SELECTOR$ decorated = $TEXT$ } } }",
    );
    let source = "country_event = { immediate = { patterned = { SELECTOR = always TEXT =  } } }";
    let id = open(&mut host, "pattern-template-call", source);
    let items = complete(
        &host.snapshot(),
        &id,
        (source.find("TEXT = ").unwrap() + "TEXT = ".len()) as u32,
    )
    .items;
    for value in ["pre_yes_post", "pre_no_post"] {
        let item = items
            .iter()
            .find(|item| item.label == value)
            .unwrap_or_else(|| panic!("{items:?}"));
        assert_eq!(
            item.documentation.as_deref(),
            Some("Boolean trigger source.")
        );
    }
    let valid = source.replace("TEXT = ", "TEXT = pre_yes_post");
    host.apply_document_changes(&id, 2, &[engine::TextChange::full(&valid)])
        .unwrap();
    assert!(
        semantic_errors(&host, &id).is_empty(),
        "{:?}",
        semantic_errors(&host, &id)
    );
    let invalid = valid.replace("SELECTOR = always", "SELECTOR = primary_culture");
    host.apply_document_changes(&id, 3, &[engine::TextChange::full(&invalid)])
        .unwrap();
    assert!(!semantic_errors(&host, &id).is_empty());
    let incomplete = invalid.replace("TEXT = pre_yes_post", "TEXT = ");
    host.apply_document_changes(&id, 4, &[engine::TextChange::full(&incomplete)])
        .unwrap();
    let candidates = labels(&host, &id, &incomplete, "TEXT = ");
    assert!(
        candidates.contains(&"pre_mod_culture_post".to_owned()),
        "{candidates:?}"
    );
    assert!(!candidates.contains(&"pre_yes_post".to_owned()));
}

#[test]
fn field_queries_source_schema_replacement_updates_all_consumers_without_rewriting_them() {
    fn source_host(renamed_key: &str, value_type: &str) -> AnalysisHost {
        let mut source = json!({
            "traits":{"Template":{}},
            "types":{"first_domain":{},"second_domain":{},"fixture_template":{"resolution":"replace","impl":{"Template":{"body":"effect"}}}},
            "schemas":{
                "fixture_root":{"fields":{"first":{"body":"first_members"},"second":{"body":"second_members"},"templates":{"body":"template_members"}}},
                "first_members":{"map":{"key":"def<first_domain>","value":"bool"}},
                "second_members":{"map":{"key":"def<second_domain>","value":"bool"}},
                "template_members":{"map":{"key":"def<fixture_template>","body":"effect"}},
                "arguments":{"map":{"key":"scalar","value":"scalar"}},
                "catalog":{"fields":{"typed":{"value":format!("ref<{value_type}>"),"doc":format!("Members of {value_type}.")}}},
                "effect":{"fields":{"choose_key":{"value":"keysof<catalog>"},"branch":{"body":"branches"}},"patterns":[{"key":"ref<fixture_template>","body":"arguments"}]},
                "branches":{"fields":{"selector":{"value":"keysof<catalog,shape=scalar>"}},"patterns":[{"key":"valuesof<catalog,key=sibling<selector>,shape=scalar>","body":"effect"}]}
            }
        });
        source["schemas"]["catalog"]["fields"][renamed_key] = json!({"value":"bool"});
        super::support::fixture_host(source)
    }
    let mut host = source_host("original_key", "first_domain");
    let first = open(
        &mut host,
        "source-first",
        "first = { shared = yes first_only = yes }",
    );
    let second = open(
        &mut host,
        "source-second",
        "second = { shared = yes second_only = yes }",
    );
    open(
        &mut host,
        "source-template",
        "templates = { query_wrapper = { branch = { selector = typed $VALUE$ = { } } } }",
    );
    let key_source = "country_event = { immediate = { choose_key = original_key } }";
    let key_id = open(&mut host, "source-key", key_source);
    let shared_source =
        "country_event = { immediate = { branch = { selector = typed shared = { } } } }";
    let shared_id = open(&mut host, "source-shared", shared_source);
    let value_source = "country_event = { immediate = { branch = { selector = typed first_only = { } } query_wrapper = { VALUE = first_only } } }";
    let value_id = open(&mut host, "source-value", value_source);
    let completion = "country_event = { immediate = { choose_key =  } }";
    let completion_id = open(&mut host, "source-completion", completion);
    let branch_completion = "country_event = { immediate = { branch = { selector = typed  } } }";
    let branch_id = open(&mut host, "source-branch-completion", branch_completion);
    let template_completion = "country_event = { immediate = { query_wrapper = { VALUE =  } } }";
    let template_id = open(&mut host, "source-template-completion", template_completion);
    let shared_position = shared_source.find("shared").unwrap() as u32;
    assert!(semantic_errors(&host, &key_id).is_empty());
    assert!(semantic_errors(&host, &value_id).is_empty());
    assert!(
        definition(&host.snapshot(), &shared_id, shared_position)
            .iter()
            .any(|location| location.document.as_ref() == Some(&first))
    );
    assert!(
        labels(&host, &completion_id, completion, "choose_key = ")
            .contains(&"original_key".to_owned())
    );
    assert!(
        labels(&host, &branch_id, branch_completion, "selector = typed ")
            .contains(&"first_only".to_owned())
    );
    assert!(
        labels(&host, &template_id, template_completion, "VALUE = ")
            .contains(&"first_only".to_owned())
    );
    let before = host.snapshot();
    let updated = source_host("replacement_key", "second_domain");
    let baked = serde_json::to_vec(updated.snapshot().ir()).unwrap();
    host.set_ir(std::sync::Arc::new(
        rules::ir::RulesIr::from_baked(&baked).unwrap(),
    ));
    assert!(!semantic_errors(&host, &key_id).is_empty());
    let errors = semantic_errors(&host, &value_id);
    assert!(
        errors
            .iter()
            .any(|issue| issue.range.start() == value_source.find("first_only").unwrap() as u32),
        "{errors:?}"
    );
    assert!(
        errors
            .iter()
            .any(|issue| issue.message.contains("query_wrapper")),
        "{errors:?}"
    );
    assert!(
        definition(&host.snapshot(), &shared_id, shared_position)
            .iter()
            .any(|location| location.document.as_ref() == Some(&second))
    );
    assert!(
        definition(&before, &shared_id, shared_position)
            .iter()
            .any(|location| location.document.as_ref() == Some(&first))
    );
    let keys = labels(&host, &completion_id, completion, "choose_key = ");
    assert!(
        keys.contains(&"replacement_key".to_owned()) && !keys.contains(&"original_key".to_owned()),
        "{keys:?}"
    );
    for candidates in [
        labels(&host, &branch_id, branch_completion, "selector = typed "),
        labels(&host, &template_id, template_completion, "VALUE = "),
    ] {
        assert!(
            candidates.contains(&"second_only".to_owned())
                && !candidates.contains(&"first_only".to_owned()),
            "{candidates:?}"
        );
    }
}

const CALLABLE_DEFINITIONS: &str = "scripted_triggers = {
    zero_parameters = { always = yes }
    requires_argument = { integer_test = $N$ }
    optional_argument = { [[N] integer_test = $N$ ] }
    defaulted_argument = { [[N] integer_test = $N$ ] [[!N] integer_test = 0 ] }
    active_required_argument = { [[!MODE] integer_test = $N$ ] }
    forwarded_required_argument = { requires_argument = yes }
    bound_argument = { requires_argument = { N = 1 } }
    forwarded_argument = { requires_argument = { N = $X$ } }
    optional_forwarded_argument = { [[X] requires_argument = { N = $X$ } ] }
}";

#[test]
fn callable_queries_share_direct_missing_argument_inference_without_restricting_references() {
    let mut host = query_host();
    let declarations = open(&mut host, "callable-definitions", CALLABLE_DEFINITIONS);
    for (name, eligible) in [
        ("zero_parameters", true),
        ("optional_argument", true),
        ("defaulted_argument", true),
        ("requires_argument", false),
        ("active_required_argument", false),
        ("forwarded_required_argument", false),
        ("bound_argument", true),
        ("forwarded_argument", false),
        ("optional_forwarded_argument", true),
    ] {
        let source = format!(
            "country_event = {{ trigger = {{ {name} = yes }} immediate = {{ export_data = trigger_value:{name} switch = {{ on_trigger = {name} yes = {{ }}  }} ordinary_reference = {name} numeric_selector = {name} }} }}"
        );
        let id = open(&mut host, name, &source);
        let issues = diagnostics(&host.snapshot(), &id);
        assert_eq!(
            issues
                .iter()
                .any(|issue| issue.code == DiagnosticCode::Cardinality
                    && issue.message.contains("parameter")),
            !eligible,
            "direct invocation {name}: {issues:?}"
        );
        let errors = semantic_errors(&host, &id);
        assert_eq!(errors.is_empty(), eligible, "query use {name}: {errors:?}");
        let export_start = source.find("trigger_value:").unwrap() as u32;
        let selector_start = (source.find("on_trigger = ").unwrap() + "on_trigger = ".len()) as u32;
        if !eligible {
            assert!(
                errors
                    .iter()
                    .any(|issue| issue.range.start() <= export_start
                        && export_start < issue.range.end()),
                "{name}: {errors:?}"
            );
            assert!(
                errors
                    .iter()
                    .any(|issue| issue.range.start() <= selector_start
                        && selector_start < issue.range.end()),
                "{name}: {errors:?}"
            );
        }
        for marker in ["ordinary_reference = ", "numeric_selector = "] {
            let position = (source.find(marker).unwrap() + marker.len()) as u32;
            assert!(
                !errors
                    .iter()
                    .any(|issue| issue.range.start() <= position && position < issue.range.end()),
                "ordinary domain {name}: {errors:?}"
            );
            assert!(labels(&host, &id, &source, marker).contains(&name.to_owned()));
        }
        let exports = labels(&host, &id, &source, "export_data = ");
        assert_eq!(
            exports.contains(&format!("trigger_value:{name}")),
            eligible,
            "{name}: {exports:?}"
        );
        let selectors = labels(&host, &id, &source, "on_trigger = ");
        assert_eq!(
            selectors.contains(&name.to_owned()),
            eligible,
            "{name}: {selectors:?}"
        );
        let marker = format!("on_trigger = {name} yes = {{ }} ");
        let cases = labels(&host, &id, &source, &marker);
        assert_eq!(
            cases.contains(&"no".to_owned()),
            eligible,
            "{name}: {cases:?}"
        );
        for position in [export_start + "trigger_value:".len() as u32, selector_start] {
            assert!(
                definition(&host.snapshot(), &id, position)
                    .iter()
                    .any(|target| target.document.as_ref() == Some(&declarations)),
                "{name} must remain navigable at {position}"
            );
            let description = hover(&host.snapshot(), &id, position).unwrap();
            assert!(
                description.contents.contains("Scripted boolean trigger."),
                "{}",
                description.contents
            );
        }
        if eligible {
            let items = complete(&host.snapshot(), &id, export_start).items;
            let item = items
                .iter()
                .find(|item| item.label == format!("trigger_value:{name}"))
                .unwrap();
            assert_eq!(
                completion_resolve(&host.snapshot(), item)
                    .documentation
                    .as_deref(),
                Some("Scripted boolean trigger.")
            );
        }
    }
}

#[test]
fn callable_queries_keep_template_export_and_switch_eligibility_in_sync() {
    let mut host = query_host();
    open(
        &mut host,
        "template-callable-definitions",
        CALLABLE_DEFINITIONS,
    );
    let templates = open(
        &mut host,
        "callable-wrappers",
        "templates = {
        select_helper = { switch = { on_trigger = $SELECTOR$ $CASE$ = { } } }
        export_helper = { export_data = trigger_value:$NAME$ }
    }",
    );
    assert!(
        semantic_errors(&host, &templates).is_empty(),
        "unbound selectors must defer"
    );
    for (name, eligible) in [
        ("zero_parameters", true),
        ("optional_argument", true),
        ("defaulted_argument", true),
        ("requires_argument", false),
        ("active_required_argument", false),
        ("forwarded_required_argument", false),
        ("bound_argument", true),
        ("forwarded_argument", false),
        ("optional_forwarded_argument", true),
    ] {
        let source = format!(
            "country_event = {{ immediate = {{ select_helper = {{ SELECTOR = {name} CASE = yes }} export_helper = {{ NAME = {name} }} }} }}"
        );
        let id = open(&mut host, &format!("wrapped-{name}"), &source);
        let errors = semantic_errors(&host, &id);
        assert_eq!(errors.is_empty(), eligible, "{name}: {errors:?}");
        let completion = source.replace("CASE = yes", "CASE = ");
        host.apply_document_changes(&id, 2, &[engine::TextChange::full(&completion)])
            .unwrap();
        for marker in ["SELECTOR = ", "NAME = "] {
            let candidates = labels(&host, &id, &completion, marker);
            assert_eq!(
                candidates.contains(&name.to_owned()),
                eligible,
                "{name}, {marker}: {candidates:?}"
            );
        }
        let candidates = labels(&host, &id, &completion, "CASE = ");
        assert_eq!(
            candidates.contains(&"no".to_owned()),
            eligible,
            "{name}: {candidates:?}"
        );
    }
}

#[test]
fn callable_queries_recompute_after_requiredness_edits_and_keep_old_snapshots() {
    let mut host = query_host();
    let declarations = open(
        &mut host,
        "editable-callable",
        "scripted_triggers = { changing_helper = { integer_test = $N$ } }",
    );
    open(
        &mut host,
        "editable-callable-wrapper",
        "templates = { select_helper = { switch = { on_trigger = $SELECTOR$ $CASE$ = { } } } }",
    );
    let source = "country_event = { immediate = { export_data = trigger_value:changing_helper switch = { on_trigger = changing_helper yes = { }  } select_helper = { SELECTOR = changing_helper CASE = yes } } }";
    let id = open(&mut host, "editable-callable-use", source);
    let completion = source.replace("CASE = yes", "CASE = ");
    let completion_id = open(&mut host, "editable-callable-completion", &completion);
    let before = host.snapshot();
    for (version, body, eligible) in [
        (
            2,
            "[[N] integer_test = $N$ ] [[!N] integer_test = 0 ]",
            true,
        ),
        (3, "integer_test = $N$", false),
    ] {
        assert_eq!(semantic_errors(&host, &id).is_empty(), !eligible);
        host.apply_document_changes(
            &declarations,
            version,
            &[engine::TextChange::full(format!(
                "scripted_triggers = {{ changing_helper = {{ {body} }} }}"
            ))],
        )
        .unwrap();
        assert_eq!(
            semantic_errors(&host, &id).is_empty(),
            eligible,
            "{:?}",
            semantic_errors(&host, &id)
        );
        let exports = labels(&host, &id, source, "export_data = ");
        assert_eq!(
            exports.contains(&"trigger_value:changing_helper".to_owned()),
            eligible,
            "{exports:?}"
        );
        for marker in ["on_trigger = changing_helper yes = { } ", "CASE = "] {
            let candidates = labels(&host, &completion_id, &completion, marker);
            assert_eq!(
                candidates.contains(&"no".to_owned()),
                eligible,
                "{marker}: {candidates:?}"
            );
        }
        assert!(
            diagnostics(&before, &id)
                .iter()
                .any(|issue| issue.code == DiagnosticCode::InvalidValue)
        );
    }
}

#[test]
fn callable_queries_defer_unavailable_or_recursive_definitions_without_claiming_eligibility() {
    let mut host = query_host();
    open(
        &mut host,
        "ambiguous-callable-one",
        "scripted_triggers = { ambiguous_helper = { always = yes } recursive_helper = { recursive_helper = yes } }",
    );
    open(
        &mut host,
        "ambiguous-callable-two",
        "scripted_triggers = { ambiguous_helper = { integer_test = $N$ } }",
    );
    for name in ["ambiguous_helper", "recursive_helper"] {
        let source = format!(
            "country_event = {{ immediate = {{ export_data = trigger_value:{name} switch = {{ on_trigger = {name} yes = {{ }}  }} ordinary_reference = {name} }} }}"
        );
        let id = open(&mut host, &format!("unknown-{name}"), &source);
        assert!(
            semantic_errors(&host, &id).is_empty(),
            "unknown requiredness must defer: {:?}",
            semantic_errors(&host, &id)
        );
        assert!(
            !labels(&host, &id, &source, "export_data = ")
                .contains(&format!("trigger_value:{name}"))
        );
        assert!(!labels(&host, &id, &source, "on_trigger = ").contains(&name.to_owned()));
        assert!(
            !labels(
                &host,
                &id,
                &source,
                &format!("on_trigger = {name} yes = {{ }} ")
            )
            .contains(&"no".to_owned())
        );
        assert!(labels(&host, &id, &source, "ordinary_reference = ").contains(&name.to_owned()));
    }
    let snapshot = host.snapshot();
    let facts = crate::ir_queries::SnapshotSymbolFacts {
        snapshot: &snapshot,
    };
    let type_id = snapshot.ir().type_by_name("scripted_trigger").unwrap();
    assert_eq!(
        rules::ir::SymbolFacts::template_accepts_no_arguments(
            &facts,
            type_id,
            "recursive_helper",
            &mut || true
        ),
        None
    );
}

#[test]
fn callable_queries_legacy_template_switch_keeps_unknown_selectors_deferred() {
    let mut host = super::support::fixture_host(json!({
        "traits":{"Template":{}},
        "types":{
            "condition":{"impl":{"Template":{"body":"trigger"}}},
            "wrapper":{"impl":{"Template":{"body":"effect"}}}
        },
        "schemas":{
            "fixture_root":{"fields":{"helpers":{"body":"helpers"},"wrappers":{"body":"wrappers"}}},
            "helpers":{"map":{"key":"def<condition>","body":"trigger"}},
            "wrappers":{"map":{"key":"def<wrapper>","body":"effect"}},
            "arguments":{"map":{"key":"scalar","value":"scalar"}},
            "trigger":{"fields":{"always":{"value":"bool"}},"patterns":[{"key":"ref<condition>","value":"bool"}]},
            "effect":{"fields":{"choose":{"body":"legacy_switch","control":{"kind":"switch","on":"selector","selector_schema":"trigger"}}},"patterns":[{"key":"ref<wrapper>","body":"arguments"}]},
            "legacy_switch":{"fields":{"selector":{"value":"scalar"}},"patterns":[{"key":"scalar","body":"effect"}]}
        }
    }));
    open(
        &mut host,
        "legacy-helper-one",
        "helpers = { ambiguous_helper = { always = yes } recursive_helper = { recursive_helper = yes } }",
    );
    open(
        &mut host,
        "legacy-helper-two",
        "helpers = { ambiguous_helper = { always = no } }",
    );
    open(
        &mut host,
        "legacy-wrapper",
        "wrappers = { select_helper = { choose = { selector = $SELECTOR$ $CASE$ = { } } } }",
    );
    for name in ["ambiguous_helper", "recursive_helper"] {
        let source = format!(
            "country_event = {{ immediate = {{ choose = {{ selector = {name} yes = {{ }}  }} select_helper = {{ SELECTOR = {name} CASE = yes }} }} }}"
        );
        let id = open(&mut host, &format!("legacy-unknown-{name}"), &source);
        let errors = semantic_errors(&host, &id);
        assert!(
            errors.is_empty(),
            "legacy unknown selector must defer: {errors:?}"
        );
        let completion = source.replace("CASE = yes", "CASE = ");
        host.apply_document_changes(&id, 2, &[engine::TextChange::full(&completion)])
            .unwrap();
        for marker in [
            format!("selector = {name} yes = {{ }} "),
            "CASE = ".to_owned(),
        ] {
            let candidates = labels(&host, &id, &completion, &marker);
            assert!(
                !candidates
                    .iter()
                    .any(|candidate| matches!(candidate.as_str(), "yes" | "no")),
                "{name}, {marker}: {candidates:?}"
            );
        }
    }
}
