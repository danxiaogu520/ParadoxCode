use text::AbsPath;

use super::support::*;

#[test]
fn scope_hints_use_rule_proven_transitions_and_skip_ambient_blocks() {
    let mut host = eu4_host(game::eu4::runtime_rules().expect("first-party rules"));
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
fn nested_history_and_on_action_blocks_get_scope_hints_from_inherited_context() {
    for (path, script, expected) in [
        (
            "/tmp/history/countries/ZZZ - Inlay.txt",
            "if = {\n\tlimit = { always = yes }\n\trandom_owned_province = {\n\t\tadd_core = ZZZ\n\t}\n}\n",
            "province",
        ),
        (
            "/tmp/history/provinces/-1 - Inlay.txt",
            "if = {\n\tlimit = { always = yes }\n\towner = {\n\t\tadd_treasury = 10\n\t}\n}\n",
            "country",
        ),
        (
            "/tmp/common/on_actions/00_inlay.txt",
            "consort_on_shipwreck = {\n\trandom_owned_province = {\n\t\tadd_core = ZZZ\n\t}\n}\n",
            "province",
        ),
    ] {
        let mut host = eu4_host(game::eu4::runtime_rules().expect("first-party rules"));
        let id = DocumentId::new(format!("file://{path}"));
        host.open_document(
            id.clone(),
            1,
            script.to_owned(),
            Some(AbsPath::normalize(&std::path::PathBuf::from(path))),
        )
        .expect("open inherited-context fixture");
        let hints = scope_inlay_hints_with_cancellation(
            &host.snapshot(),
            &id,
            None,
            &CancellationToken::new(),
        )
        .expect("scope hints");
        assert!(
            hints.iter().any(|hint| hint.scope == expected),
            "{path} inherits effect context, so scope transitions must yield hints: {hints:?}"
        );
    }
}

#[test]
fn consumed_script_scope_hints_share_the_parent_instance() {
    let text = "country_event = { immediate = { run_hint = { BODY = \"capital_scope = { add_base_tax = 1 }\" TEXT = \"capital_scope = { add_base_tax = 1 }\" } } }";
    let (mut host, id) = snapshot(text);
    host.open_document(
        DocumentId::new("file:///tmp/common/scripted_effects/hints.txt"),
        1,
        "run_hint = { $BODY$ log = $TEXT$ }".to_owned(),
        None,
    )
    .unwrap();
    let hints =
        scope_inlay_hints_with_cancellation(&host.snapshot(), &id, None, &CancellationToken::new())
            .unwrap();
    let first = text.find("capital_scope = {").unwrap() as u32 + "capital_scope = ".len() as u32;
    let last = text.rfind("capital_scope = {").unwrap() as u32 + "capital_scope = ".len() as u32;
    assert!(
        hints
            .iter()
            .any(|hint| hint.position == first && hint.scope == "province"),
        "{hints:?}"
    );
    assert!(!hints.iter().any(|hint| hint.position == last));
}
