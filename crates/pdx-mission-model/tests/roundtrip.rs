//! Round-trip and validation tests for `pdx-mission-model`.

use pdx_mission_model::{
    MissionTree, apply_tree_edit, detect_style, parse_file, render_tree, validate,
};

const FIXTURE: &str = include_str!("fixtures/sample_missions.txt");

fn load(source: &str) -> pdx_mission_model::MissionFile {
    let loaded = parse_file(source);
    assert!(loaded.syntax_errors.is_empty(), "unexpected syntax errors");
    loaded.file
}

/// Compares two trees ignoring source spans (rendering changes byte offsets).
fn assert_trees_equal(a: &MissionTree, b: &MissionTree) {
    assert_eq!(a.id, b.id);
    assert_eq!(a.slot, b.slot);
    assert_eq!(a.generic, b.generic);
    assert_eq!(a.ai, b.ai);
    assert_eq!(a.has_country_shield, b.has_country_shield);
    assert_eq!(a.potential, b.potential);
    assert_eq!(a.potential_on_load, b.potential_on_load);
    assert_eq!(a.unknown, b.unknown);
    assert_eq!(a.missions.len(), b.missions.len());
    for (x, y) in a.missions.iter().zip(&b.missions) {
        assert_eq!(x.id, y.id);
        assert_eq!(x.icon, y.icon);
        assert_eq!(x.mission_type, y.mission_type);
        assert_eq!(x.provinces_to_highlight, y.provinces_to_highlight);
        assert_eq!(x.required, y.required);
        assert_eq!(x.position, y.position);
        assert_eq!(x.completed_by, y.completed_by);
        assert_eq!(x.trigger, y.trigger);
        assert_eq!(x.effect, y.effect);
        assert_eq!(x.unknown, y.unknown);
    }
}

#[test]
fn loads_tree_and_mission_fields() {
    let file = load(FIXTURE);
    assert_eq!(file.trees.len(), 2);

    let tree = &file.trees[0];
    assert_eq!(tree.id, "sam_main_tree");
    assert_eq!(tree.slot, 2);
    assert!(!tree.generic);
    assert_eq!(tree.ai, Some(true));
    assert_eq!(tree.has_country_shield, Some(true));
    // Block text is preserved verbatim; normalize line endings for the check.
    let norm = |text: &str| text.replace("\r\n", "\n");
    assert_eq!(
        norm(tree.potential_on_load.as_ref().unwrap().text.as_str()),
        "{\n\t\thas_dlc = \"Domination\"\n\t}"
    );
    assert_eq!(
        norm(tree.potential.as_ref().unwrap().text.as_str()),
        "{\n\t\tOR = { tag = SAM tag = SAM2 }\n\t}"
    );
    assert_eq!(tree.unknown.len(), 1);
    assert_eq!(tree.unknown[0].name, "custom_tree_flag");
    assert_eq!(tree.unknown[0].value, "beta_only");

    assert_eq!(tree.missions.len(), 2);
    let first = &tree.missions[0];
    assert_eq!(first.id, "sam_first_mission");
    assert_eq!(first.icon.as_deref(), Some("mission_assemble_an_army"));
    assert!(first.required.is_empty());
    assert_eq!(first.position, Some(1));
    assert_eq!(first.completed_by, None);
    assert!(first.trigger.is_some());
    assert!(first.effect.is_some());

    let second = &tree.missions[1];
    assert_eq!(second.required, vec!["sam_first_mission"]);
    assert_eq!(second.position, Some(2));
    assert_eq!(second.completed_by.as_deref(), Some("1500.1.1"));
    assert_eq!(second.mission_type.as_deref(), Some("conquest"));
    assert!(second.unknown.is_empty());
}

#[test]
fn round_trip_render_then_parse_is_equal() {
    let file = load(FIXTURE);
    let style = detect_style(FIXTURE);
    for tree in &file.trees {
        let rendered = render_tree(tree, &style);
        let reparsed = load(&rendered);
        assert_eq!(reparsed.trees.len(), 1, "render must stay a single tree");
        assert_trees_equal(tree, &reparsed.trees[0]);
    }
}

#[test]
fn rendering_is_idempotent() {
    let file = load(FIXTURE);
    let style = detect_style(FIXTURE);
    for tree in &file.trees {
        let once = render_tree(tree, &style);
        let twice = render_tree(&load(&once).trees[0], &style);
        assert_eq!(once, twice, "second render must be byte-identical");
    }
}

#[test]
fn apply_tree_edit_only_touches_the_target_tree() {
    let file = load(FIXTURE);
    let style = detect_style(FIXTURE);
    let rendered = render_tree(&file.trees[0], &style);

    let span = file.trees[0].span;
    let edited = apply_tree_edit(FIXTURE, span, &rendered);

    // Prefix and suffix around the edited span must be byte-identical.
    assert_eq!(
        &edited[..span.start() as usize],
        &FIXTURE[..span.start() as usize],
        "prefix changed"
    );
    assert_eq!(
        &edited[span.start() as usize..span.start() as usize + rendered.len()],
        rendered.as_str(),
        "replacement text mismatch"
    );
    assert_eq!(
        &edited[span.start() as usize + rendered.len()..],
        &FIXTURE[span.end() as usize..],
        "suffix changed"
    );

    // And the result still parses to the same two trees.
    let after = load(&edited);
    assert_eq!(after.trees.len(), 2);
    assert_trees_equal(&file.trees[0], &after.trees[0]);
    assert_trees_equal(&file.trees[1], &after.trees[1]);
}

#[test]
fn detects_style_from_source() {
    let style = detect_style(FIXTURE);
    assert_eq!(style.indent, pdx_mission_model::Indent::Tab);
    // Fixture line endings depend on the platform; the style must match whatever
    // the file actually uses.
    let lf = FIXTURE.matches('\n').count();
    let crlf = FIXTURE.matches("\r\n").count();
    assert_eq!(style.newline, if crlf > lf - crlf { "\r\n" } else { "\n" });

    let crlf_spaces = "tree_a = {\n\r\n    slot = 1\r\n}\r\n";
    let style = detect_style(crlf_spaces);
    assert_eq!(style.indent, pdx_mission_model::Indent::Spaces(4));
    assert_eq!(style.newline, "\r\n");
}

#[test]
fn load_is_loss_aware_on_syntax_errors() {
    // Missing closing brace: the parser still recovers the first tree.
    let broken = "broken_tree = {\n\tslot = 1\n\tbroken_mission = {\n\t\ticon = mission_x\n";
    let loaded = parse_file(broken);
    assert!(!loaded.syntax_errors.is_empty(), "expected syntax errors");
    assert_eq!(loaded.file.trees.len(), 1);
    assert_eq!(loaded.file.trees[0].id, "broken_tree");
    assert_eq!(loaded.file.trees[0].missions.len(), 1);
}

// --- validation ------------------------------------------------------------

#[test]
fn validates_fixture_cleanly() {
    let file = load(FIXTURE);
    // The fixture's cross-tree reference is a legitimate 1.35+ branching mission,
    // so the file must be free of diagnostics.
    let diagnostics = validate(&file);
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}

#[test]
fn reports_duplicate_ids() {
    let source = "\
tree_a = {
\tslot = 1
\tgeneric = no
\tdup_mission = { icon = mission_x }
\tdup_mission = { icon = mission_y }
}
tree_a = {
\tslot = 2
\tgeneric = no
}
";
    let file = load(source);
    let diagnostics = validate(&file);
    let codes: Vec<&str> = diagnostics.iter().map(|d| d.code).collect();
    assert!(codes.contains(&"duplicate-tree-id"), "{codes:?}");
    assert!(codes.contains(&"duplicate-mission-id"), "{codes:?}");
}

#[test]
fn reports_dangling_references_but_not_cross_tree() {
    let source = "\
tree_a = {
\tslot = 1
\tgeneric = no
\tmission_a = {
\t	required_missions = { ghost_mission }
\t}
}
tree_b = {
\tslot = 2
\tgeneric = no
\tmission_c = {
\t	required_missions = { mission_a }
	}
}
";
    let file = load(source);
    let diagnostics = validate(&file);
    assert!(
        diagnostics
            .iter()
            .any(|d| d.code == "dangling-required" && d.message.contains("ghost_mission")),
        "{diagnostics:?}"
    );
    assert!(
        diagnostics.iter().all(|d| d.code != "cross-tree-required"),
        "cross-tree references are legal in 1.35+: {diagnostics:?}"
    );
}

#[test]
fn reports_cross_tree_cycles() {
    let source = "\
tree_a = {
\tslot = 1
\tgeneric = no
\tmission_a = {
\t\trequired_missions = { mission_b }
\t}
}
tree_b = {
\tslot = 2
\tgeneric = no
\tmission_b = {
\t\trequired_missions = { mission_a }
\t}
}
";
    let file = load(source);
    let diagnostics = validate(&file);
    assert!(
        diagnostics.iter().any(|d| d.code == "dependency-cycle"),
        "{diagnostics:?}"
    );
}

#[test]
fn reports_dependency_cycles() {
    let source = "\
tree_a = {
\tslot = 1
\tgeneric = no
\tmission_a = {
\t\trequired_missions = { mission_b }
\t}
\tmission_b = {
\t\trequired_missions = { mission_c }
\t}
\tmission_c = {
\t\trequired_missions = { mission_a }
\t}
}
";
    let file = load(source);
    let diagnostics = validate(&file);
    let cycles: Vec<&str> = diagnostics
        .iter()
        .filter(|d| d.code == "dependency-cycle")
        .map(|d| d.message.as_str())
        .collect();
    assert!(!cycles.is_empty(), "{diagnostics:?}");
    assert!(
        cycles
            .iter()
            .any(|c| c.contains("mission_a") && c.contains("mission_c")),
        "cycle must mention all members: {cycles:?}"
    );
}

#[test]
fn warns_on_zero_position() {
    let source = "\
tree_a = {
\tslot = 1
\tgeneric = no
\tmission_a = {
\t\tposition = 0
\t}
}
";
    let file = load(source);
    let diagnostics = validate(&file);
    assert!(
        diagnostics.iter().any(|d| d.code == "zero-position"),
        "{diagnostics:?}"
    );
}
