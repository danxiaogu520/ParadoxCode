#[test]
fn truncate_localisation_preview_keeps_long_event_texts() {
    let event_text = "x".repeat(600);
    assert_eq!(
        crate::support::truncate_localisation_preview(&event_text),
        event_text
    );
    let pathological = "y".repeat(1001);
    let truncated = crate::support::truncate_localisation_preview(&pathological);
    assert_eq!(truncated.chars().count(), 1001);
    assert!(truncated.ends_with('…'));
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
