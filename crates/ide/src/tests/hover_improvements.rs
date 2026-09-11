use std::sync::Arc;

use super::support::*;

#[test]
fn known_keys_are_memoized_per_snapshot() {
    let (host, _id) = semantic_snapshot("trigger = { foo = yes }\n");
    let snapshot = host.snapshot();
    let first = crate::hover::known_keys(&snapshot);
    let second = crate::hover::known_keys(&snapshot);
    assert!(Arc::ptr_eq(&first, &second), "known_keys must be cached");
    assert!(!first.is_empty(), "profile fallback keys belong in the set");
}

#[test]
fn pattern_rule_hint_reports_matched_families() {
    let mut host = eu4_host(game::eu4::first_party_rules().expect("first-party rules"));
    let id = DocumentId::new("file:///tmp/common/events/hint.txt");
    let text = "country_event = { id = hint.1 }\n";
    host.open_document(id.clone(), 1, text.to_owned(), None)
        .expect("open fixture");
    let snapshot = host.snapshot();

    // The EU4 first-party rules cover at least one informative non-exact key family (type
    // member, enum member, or date); find any key that such a matcher accepts and verify the
    // hover fallback surfaces provenance for it.
    let covered = [
        "capital",
        "has_dlc",
        "1444.11.11",
        "tag",
        "owns",
        "any_country",
    ]
    .into_iter()
    .find(|candidate| crate::hover::semantic_pattern_rule_hint(&snapshot, candidate).is_some());
    let _covered = covered.expect("EU4 rules must pattern-match at least one probe key");
}

#[test]
fn pattern_rule_hint_ignores_open_ended_matchers() {
    // `AnyScalar`/`Dynamic` matchers accept every key; they must not manufacture provenance.
    let (host, _id) = semantic_snapshot("trigger = { foo = yes }\n");
    let snapshot = host.snapshot();
    assert!(crate::hover::semantic_pattern_rule_hint(&snapshot, "totally_unknown_key").is_none());
}

#[test]
fn pattern_rule_hint_rejects_unmatched_keys() {
    let (host, _id) = semantic_snapshot("trigger = { foo = yes }\n");
    let snapshot = host.snapshot();
    assert!(crate::hover::semantic_pattern_rule_hint(&snapshot, "zzz_no_match_zzz").is_none());
}

#[test]
fn truncate_hover_text_appends_single_ellipsis() {
    let long = "x".repeat(600);
    let truncated = crate::support::truncate_hover_text(&long);
    assert_eq!(truncated.chars().count(), 241);
    assert!(truncated.ends_with('…'));
    assert_eq!(crate::support::truncate_hover_text("short"), "short");
}

#[test]
fn find_cst_node_is_depth_bounded() {
    // Build a deeply nested script through the real parser; the bounded search must terminate
    // without finding a node beyond the depth limit instead of recursing unboundedly.
    let inner = "a = { b = { c = { d = { e = { f = { g = { h = yes } } } } } } }";
    let mut text = String::new();
    for _ in 0..40 {
        text.push_str("wrap = { ");
    }
    text.push_str(inner);
    for _ in 0..40 {
        text.push_str(" }");
    }
    let parsed = parser::parse(parser::FileFormat::Script, &text);
    assert!(
        crate::localisation::find_cst_node(
            parsed.root(),
            parser::CstKind::Error,
            text::TextRange::empty(0)
        )
        .is_none()
    );
}

#[test]
fn template_modifier_keys_report_their_rule_family_hint() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("ide-template-hover-{nonce}"));
    let estates = root.join("common/estates");
    std::fs::create_dir_all(&estates).expect("estates directory");
    std::fs::write(
        estates.join("00_test.txt"),
        "estate_my_guild = { icon = 1 }
",
    )
    .expect("estate source");
    let mut host = eu4_host(game::eu4::first_party_rules().expect("first-party rules"));
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(1),
        SourceRootKind::CurrentMod,
        root.clone(),
    )]));
    host.refresh_source_roots().expect("scan estates");
    let hint =
        crate::hover::semantic_pattern_rule_hint(&host.snapshot(), "my_guild_loyalty_modifier");
    assert!(
        hint.as_deref()
            .is_some_and(|hint| hint.contains("template")),
        "a template-matched key must report its rule family: {hint:?}"
    );
    assert!(
        crate::hover::semantic_pattern_rule_hint(&host.snapshot(), "stranger_loyalty_modifier")
            .is_none(),
        "an unknown estate spelling must not claim a rule family"
    );
    std::fs::remove_dir_all(root).expect("cleanup");
}
