use super::support::*;

#[test]
fn scope_hints_use_rule_proven_transitions_and_skip_ambient_blocks() {
    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
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
