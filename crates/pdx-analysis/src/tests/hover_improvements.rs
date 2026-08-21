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
    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
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
    let truncated = crate::hover::truncate_hover_text(&long);
    assert_eq!(truncated.chars().count(), 241);
    assert!(truncated.ends_with('…'));
    assert_eq!(crate::hover::truncate_hover_text("short"), "short");
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
    let parsed = pdx_parser::parse(pdx_parser::FileFormat::Script, &text);
    assert!(
        crate::hover::find_cst_node(
            parsed.root(),
            pdx_parser::CstKind::Error,
            pdx_text::TextRange::empty(0)
        )
        .is_none()
    );
}
