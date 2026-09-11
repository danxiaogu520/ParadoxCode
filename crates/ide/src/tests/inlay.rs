use super::support::*;

#[test]
fn scope_hints_use_rule_proven_transitions_and_skip_ambient_blocks() {
    let mut host = eu4_host(game::eu4::first_party_rules().expect("first-party rules"));
    let id = DocumentId::new("file:///tmp/events/inlay.txt");
    host.open_document(
        id.clone(),
        1,
        "country_event = { immediate = { capital_scope = { add_base_tax = 1 } } }\n".to_owned(),
        None,
    )
    .expect("open");
    let hints =
        scope_inlay_hints_with_cancellation(&host.snapshot(), &id, None, &CancellationToken::new())
            .expect("scope hints");
    assert!(
        hints.iter().any(|hint| hint.scope == "province"),
        "hints: {hints:?}"
    );
    assert!(hints.iter().all(|hint| hint.scope != "country"));
}

#[test]
fn country_history_nested_blocks_get_scope_hints_from_inherited_context() {
    let path = "/tmp/history/countries/ZZZ - Inlay.txt";
    let mut host = eu4_host(game::eu4::first_party_rules().expect("first-party rules"));
    let id = DocumentId::new(format!("file://{path}"));
    host.open_document(
        id.clone(),
        1,
        "if = {\n\tlimit = { always = yes }\n\trandom_owned_province = {\n\t\tadd_core = ZZZ\n\t}\n}\n".to_owned(),
        Some(std::path::PathBuf::from(path)),
    )
    .expect("open country history");
    let hints =
        scope_inlay_hints_with_cancellation(&host.snapshot(), &id, None, &CancellationToken::new())
            .expect("scope hints");
    assert!(
        hints.iter().any(|hint| hint.scope == "province"),
        "country history inherits effect, so nested scope transitions must yield hints: {hints:?}"
    );
}

#[test]
fn province_history_nested_blocks_get_scope_hints_from_inherited_context() {
    let path = "/tmp/history/provinces/-1 - Inlay.txt";
    let mut host = eu4_host(game::eu4::first_party_rules().expect("first-party rules"));
    let id = DocumentId::new(format!("file://{path}"));
    host.open_document(
        id.clone(),
        1,
        "if = {\n\tlimit = { always = yes }\n\towner = {\n\t\tadd_treasury = 10\n\t}\n}\n"
            .to_owned(),
        Some(std::path::PathBuf::from(path)),
    )
    .expect("open province history");
    let hints =
        scope_inlay_hints_with_cancellation(&host.snapshot(), &id, None, &CancellationToken::new())
            .expect("scope hints");
    assert!(
        hints.iter().any(|hint| hint.scope == "country"),
        "province history inherits effect and starts in province scope, so owner must yield a country hint: {hints:?}"
    );
}

#[test]
fn on_action_effect_bodies_get_scope_hints_from_inherited_context() {
    let path = "/tmp/common/on_actions/00_inlay.txt";
    let mut host = eu4_host(game::eu4::first_party_rules().expect("first-party rules"));
    let id = DocumentId::new(format!("file://{path}"));
    host.open_document(
        id.clone(),
        1,
        "consort_on_shipwreck = {\n\trandom_owned_province = {\n\t\tadd_core = ZZZ\n\t}\n}\n"
            .to_owned(),
        Some(std::path::PathBuf::from(path)),
    )
    .expect("open on_action");
    let hints =
        scope_inlay_hints_with_cancellation(&host.snapshot(), &id, None, &CancellationToken::new())
            .expect("scope hints");
    assert!(
        hints.iter().any(|hint| hint.scope == "province"),
        "on_action bodies inherit effect context, so scope transitions must yield hints: {hints:?}"
    );
}
