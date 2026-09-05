//! Golden snapshots of the user-facing diagnostic surface.
//!
//! Every production family renders its diagnostics (code, severity, certainty,
//! range, message, fixes) into `src/tests/golden/<name>.txt`. The files are the
//! regression contract for message and policy refactors: a changed golden file
//! is a reviewable diff of exactly what users will see.
//!
//! Bless intentional changes with:
//!
//! ```text
//! PDX_UPDATE_GOLDEN=1 cargo test -p pdx-analysis golden
//! ```
//!
//! and review the resulting diff before committing. A missing golden file in a
//! normal run is a failure, never a pass.

use super::support::*;
use crate::{Diagnostic, diagnostics};
use std::path::PathBuf;

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/tests/golden")
}

/// Renders one case deterministically: a header with the input text followed by
/// every diagnostic in collector order.
fn render(name: &str, text: &str, items: &[Diagnostic]) -> String {
    let mut out = String::new();
    out.push_str(&format!("// case: {name}\n"));
    out.push_str("// input:\n");
    for line in text.split('\n') {
        out.push_str(&format!("//   {line}\n"));
    }
    out.push_str("diagnostics:\n");
    if items.is_empty() {
        out.push_str("  (none)\n");
    }
    for diagnostic in items {
        out.push_str(&format!(
            "- code={} severity={:?} certainty={} range={}..{}\n",
            diagnostic.code.as_str(),
            diagnostic.severity,
            diagnostic.certainty.as_str(),
            diagnostic.range.start(),
            diagnostic.range.end(),
        ));
        out.push_str(&format!("  message: {}\n", diagnostic.message));
        if let Some(expected) = diagnostic.expected.as_deref() {
            out.push_str(&format!("  expected: {expected}\n"));
        }
        for note in &diagnostic.notes {
            out.push_str(&format!("  note: {note}\n"));
        }
        for related in &diagnostic.related {
            out.push_str(&format!(
                "  related: {} @ {}:{}..{}\n",
                related.message,
                location_label(&related.location),
                related.location.range.start(),
                related.location.range.end(),
            ));
        }
        for fix in &diagnostic.fixes {
            out.push_str(&format!(
                "  fix: {} [{}..{}] -> {}\n",
                fix.title,
                fix.range.start(),
                fix.range.end(),
                escape(&fix.new_text),
            ));
        }
    }
    out
}

/// Stable one-line identity of a location: the document URI when open, the
/// logical path when indexed, or `?` when neither is known.
fn location_label(location: &crate::types::Location) -> String {
    if let Some(document) = location.document.as_ref() {
        return document.as_str().to_owned();
    }
    if let Some(path) = location.path.as_ref() {
        return path.as_str().to_owned();
    }
    "?".to_owned()
}

fn escape(text: &str) -> String {
    format!("{text:?}")
}

fn assert_golden(name: &str, text: &str, items: &[Diagnostic]) {
    let actual = render(name, text, items);
    let path = golden_dir().join(format!("{name}.txt"));
    if std::env::var_os("PDX_UPDATE_GOLDEN").is_some() {
        std::fs::create_dir_all(golden_dir()).expect("create golden directory");
        std::fs::write(&path, &actual)
            .unwrap_or_else(|error| panic!("write {}: {error}", path.display()));
        eprintln!("updated golden {}", path.display());
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| {
            panic!(
                "missing golden {} ({error}); bless with PDX_UPDATE_GOLDEN=1",
                path.display()
            )
        })
        // Golden files are written with LF and pinned to LF by .gitattributes;
        // normalize defensively so a CRLF checkout still compares cleanly.
        .replace("\r\n", "\n");
    assert_eq!(
        expected, actual,
        "golden mismatch for {name}; expected is on the left, actual on the right"
    );
}

fn analyze_text(host: &AnalysisHost, id: &DocumentId) -> Vec<Diagnostic> {
    diagnostics(&host.snapshot(), id)
}

/// A `RuleShape::Leaf` rule with the fixture defaults used across the corpus.
fn leaf_rule(id: &str, context: &str, key: KeyMatcher, value: ValueMatcher) -> SemanticRule {
    SemanticRule {
        id: id.to_owned(),
        context: context.to_owned(),
        parent_path: Vec::new(),
        key,
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
    }
}

/// Creates an isolated CurrentMod source root under the system temp directory.
fn temp_root(tag: &str) -> PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-golden-{tag}-{nonce}"));
    std::fs::create_dir_all(&root).expect("golden temp root");
    root
}

fn first_party_host(root: &std::path::Path) -> AnalysisHost {
    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(1),
        SourceRootKind::CurrentMod,
        root.to_path_buf(),
    )]));
    host.refresh_source_roots().expect("scan golden root");
    host
}

#[test]
fn golden_syntax_errors() {
    let text = "trigger = {\n  missing =\n";
    let (host, id) = snapshot(text);
    assert_golden("syntax_errors", text, &analyze_text(&host, &id));
}

#[test]
fn golden_semantic_leaf_unknown_key_cardinality() {
    let text = "trigger = { foo = maybe unknown = yes foo = no }\n";
    let (host, id) = semantic_snapshot(text);
    assert_golden(
        "semantic_leaf_unknown_key_cardinality",
        text,
        &analyze_text(&host, &id),
    );
}

#[test]
fn golden_enum_did_you_mean() {
    let text = "trigger = { mode = histori }\n";
    let mut model = pdx_game::eu4::bootstrap_model();
    model.semantic.enum_values.insert(
        "fixture_modes".to_owned(),
        vec!["historic".to_owned(), "dynamic".to_owned()],
    );
    model.semantic.rules.push(leaf_rule(
        "fixture:trigger:mode",
        "trigger",
        KeyMatcher::Exact("mode".to_owned()),
        ValueMatcher::Enum("fixture_modes".to_owned()),
    ));
    let mut host = eu4_host(RuleSet::from_model(model));
    let id = DocumentId::new("file:///tmp/common/events/golden-enum.txt");
    host.open_document(id.clone(), 1, text.to_owned(), None)
        .expect("open golden enum fixture");
    assert_golden("enum_did_you_mean", text, &analyze_text(&host, &id));
}

#[test]
fn golden_value_clause_bare_values() {
    let text = "terrain = { color = { 1 2 300 } }\n";
    let mut model = pdx_game::eu4::bootstrap_model();
    let mut color = leaf_rule(
        "fixture:terrain:color",
        "terrain",
        KeyMatcher::Exact("color".to_owned()),
        ValueMatcher::AnyScalar,
    );
    color.shape = RuleShape::ValueClause;
    color.operator = Some("=".to_owned());
    model.semantic.rules.push(color);
    model.semantic.rules.push(SemanticRule {
        id: "fixture:terrain:color:int".to_owned(),
        context: "terrain".to_owned(),
        parent_path: vec!["color".to_owned()],
        key: KeyMatcher::AnyScalar,
        operator: None,
        value: ValueMatcher::Int {
            min: Some(0),
            max: Some(255),
        },
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
        min_occurs: Some(3),
        strict_min: true,
        max_occurs: Some(3),
        source_file: "fixture.semantic".to_owned(),
        line: 2,
    });
    let mut host = eu4_host(RuleSet::from_model(model));
    let id = DocumentId::new("file:///tmp/common/terrain/golden.txt");
    host.open_document(id.clone(), 1, text.to_owned(), None)
        .expect("open golden terrain fixture");
    assert_golden("value_clause_bare_values", text, &analyze_text(&host, &id));
}

#[test]
fn golden_scope_target_failures() {
    let text = "trigger = { target = NOWHERE scope = NOWHERE target = capital }\n";
    let mut model = pdx_game::eu4::bootstrap_model();
    model.semantic.rules.push(leaf_rule(
        "fixture:trigger:target",
        "trigger",
        KeyMatcher::Exact("target".to_owned()),
        ValueMatcher::Scope(Some("country".to_owned())),
    ));
    model.semantic.rules.push(leaf_rule(
        "fixture:trigger:scope-command",
        "trigger",
        KeyMatcher::Exact("scope".to_owned()),
        ValueMatcher::Scope(Some("country".to_owned())),
    ));
    let mut host = eu4_host(RuleSet::from_model(model));
    let id = DocumentId::new("file:///tmp/common/events/golden-scope-targets.txt");
    host.open_document(id.clone(), 1, text.to_owned(), None)
        .expect("open golden scope-target fixture");
    assert_golden("scope_target_failures", text, &analyze_text(&host, &id));
}

#[test]
fn golden_inline_unknown_scope() {
    let text = "trigger = { unknown_key = yes scope = nowhere }\n";
    let (host, id) = semantic_snapshot(text);
    assert_golden("inline_unknown_scope", text, &analyze_text(&host, &id));
}

#[test]
fn golden_duplicate_definitions() {
    let text = concat!(
        "country_event = { id = dup.1 title = dup.1.t option = { name = dup.1.a } }\n",
        "country_event = { id = dup.1 title = dup.1.t option = { name = dup.1.b } }\n",
        "event = dup.1\n",
    );
    let (host, id) = snapshot(text);
    assert_golden("duplicate_definitions", text, &analyze_text(&host, &id));
}

#[test]
fn golden_lints_degenerate_shapes() {
    let text = concat!(
        "country_event = { id = lint.1\n",
        "  trigger = {\n",
        "    NOT = { always = yes is_year = 1500 }\n",
        "    OR = { has_country_flag = lint_flag }\n",
        "    AND = { }\n",
        "  }\n",
        "  option = { name = lint.1.a\n",
        "    if = { limit = { always = yes } add_prestige = 1\n",
        "      else = { add_prestige = 4 }\n",
        "    }\n",
        "    else = { add_prestige = 2 }\n",
        "    ai_chance = { factor = 0 }\n",
        "  }\n",
        "  option = { name = lint.1.b\n",
        "    else = { add_prestige = 3 }\n",
        "    has_country_flag = lint_flag\n",
        "  }\n",
        "}\n",
        "country_event = { id = lint.2 trigger = { add_prestige = 1 } option = { name = lint.2.a } }\n",
    );
    let root = temp_root("lints");
    let mut host = first_party_host(&root);
    let id = DocumentId::new("file:///tmp/events/golden_lint_probe.txt");
    host.open_document(
        id.clone(),
        1,
        text.to_owned(),
        Some(root.join("events/golden_lint_probe.txt")),
    )
    .expect("open golden lint probe");
    assert_golden("lints_degenerate_shapes", text, &analyze_text(&host, &id));
    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn golden_missing_limit_and_empty_block() {
    let text = concat!(
        "country_event = { id = mle.1 option = { name = mle.1.a\n",
        "  if = { add_prestige = 1 }\n",
        "  if = { limit = { always = yes } }\n",
        "  if = { }\n",
        "} }\n",
    );
    let root = temp_root("mle");
    let mut host = first_party_host(&root);
    let id = DocumentId::new("file:///tmp/events/golden_mle.txt");
    host.open_document(
        id.clone(),
        1,
        text.to_owned(),
        Some(root.join("events/golden_mle.txt")),
    )
    .expect("open golden mle probe");
    assert_golden(
        "missing_limit_and_empty_block",
        text,
        &analyze_text(&host, &id),
    );
    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn golden_rule_wrong_scope() {
    let text =
        "province_event = { id = rws.1 title = rws.1.t option = { name = a add_prestige = 1 } }\n";
    let root = temp_root("rws");
    let mut host = first_party_host(&root);
    let id = DocumentId::new("file:///tmp/events/golden_rws.txt");
    host.open_document(
        id.clone(),
        1,
        text.to_owned(),
        Some(root.join("events/golden_rws.txt")),
    )
    .expect("open golden rws probe");
    assert_golden("rule_wrong_scope", text, &analyze_text(&host, &id));
    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn golden_dynamic_cycles() {
    let text = concat!(
        "ping = { pong = yes }\n",
        "pong = { ping = yes }\n",
        "loop_self = { loop_self = yes }\n",
        "honest = { add_prestige = 1 }\n",
    );
    let root = temp_root("cycles");
    let effects = root.join("common/scripted_effects");
    std::fs::create_dir_all(&effects).expect("effects directory");
    std::fs::write(effects.join("00_cycles.txt"), text).expect("write definitions");
    let mut host = first_party_host(&root);
    let id = DocumentId::new("file:///tmp/common/scripted_effects/00_cycles.txt");
    host.open_document(
        id.clone(),
        1,
        text.to_owned(),
        Some(effects.join("00_cycles.txt")),
    )
    .expect("open golden cycles");
    assert_golden("dynamic_cycles", text, &analyze_text(&host, &id));
    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn golden_dynamic_scope_contracts() {
    let text = concat!(
        "clash = { add_prestige = 1 add_province_modifier = { name = clash_mod duration = 1 } }\n",
        "via_callee = { helper_province = yes add_prestige = 1 }\n",
        "helper_province = { change_province_name = \"X\" }\n",
        "fine = { add_prestige = 1 add_manpower = 1000 }\n",
        "dual_ok = { add_prestige = 1 add_core = FRA }\n",
        "scoped_ok = { any_country = { change_province_name = \"Y\" } }\n",
        "nested_ok = { if = { limit = { always = yes } add_prestige = 1 } }\n",
        "dynamic_dispatch = { $action$ = yes }\n",
        "root_opaque = { add_prestige = 1 ROOT = { change_province_name = \"Z\" } }\n",
        "this_opaque = { add_prestige = 1 THIS = { change_province_name = \"W\" } }\n",
        "or_union = { if = { limit = { OR = { is_capital = yes has_estate_privilege = some_priv } } add_prestige = 1 } }\n",
        "or_open = { if = { limit = { OR = { unknown_branch_key = yes is_capital = yes } } add_prestige = 1 } }\n",
    );
    let root = temp_root("contracts");
    let effects = root.join("common/scripted_effects");
    std::fs::create_dir_all(&effects).expect("effects directory");
    std::fs::write(effects.join("00_contracts.txt"), text).expect("write definitions");
    let mut host = first_party_host(&root);
    let id = DocumentId::new("file:///tmp/common/scripted_effects/00_contracts.txt");
    host.open_document(
        id.clone(),
        1,
        text.to_owned(),
        Some(effects.join("00_contracts.txt")),
    )
    .expect("open golden contracts");
    assert_golden("dynamic_scope_contracts", text, &analyze_text(&host, &id));
    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn golden_dynamic_call_scope_mismatch() {
    let effects = concat!(
        "country_helper = { add_prestige = 1 }\n",
        "province_helper = { change_province_name = \"X\" }\n",
        "unconstrained_helper = { custom_tooltip = some_tip }\n",
    );
    let text = concat!(
        "country_event = {\n",
        "    id = call_site_test.1\n",
        "    title = call_site_test.1.t\n",
        "    option = { name = ok_country\n",
        "        country_helper = yes\n",
        "    }\n",
        "}\n",
        "country_event = {\n",
        "    id = call_site_test.2\n",
        "    title = call_site_test.2.t\n",
        "    option = { name = bad_province_dynamic\n",
        "        province_helper = yes\n",
        "    }\n",
        "}\n",
        "province_event = {\n",
        "    id = call_site_test.5\n",
        "    title = call_site_test.5.t\n",
        "    option = { name = bad_country_dynamic\n",
        "        country_helper = yes\n",
        "    }\n",
        "}\n",
    );
    let root = temp_root("call-sites");
    let effects_dir = root.join("common/scripted_effects");
    let events_dir = root.join("events");
    std::fs::create_dir_all(&effects_dir).expect("effects directory");
    std::fs::create_dir_all(&events_dir).expect("events directory");
    std::fs::write(effects_dir.join("00_effects.txt"), effects).expect("write effects");
    std::fs::write(events_dir.join("golden_call_sites.txt"), text).expect("write events");
    let mut host = first_party_host(&root);
    let id = DocumentId::new("file:///tmp/events/golden_call_sites.txt");
    host.open_document(
        id.clone(),
        1,
        text.to_owned(),
        Some(events_dir.join("golden_call_sites.txt")),
    )
    .expect("open golden call sites");
    assert_golden(
        "dynamic_call_scope_mismatch",
        text,
        &analyze_text(&host, &id),
    );
    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn golden_modifier_scope_mismatch() {
    let modifiers = concat!(
        "country_mod = { global_tax_modifier = 0.1 discipline = 0.05 }\n",
        "province_mod = { local_unrest = -1 local_defensiveness = 0.2 }\n",
        "mixed_mod = { global_tax_modifier = 0.1 local_unrest = -1 }\n",
        "unscoped_mod = { monthly_militarized_society = 0.1 }\n",
        "unit_mod = { land_morale_constant = 0.5 }\n",
    );
    let text = concat!(
        "country_event = {\n",
        "    id = test_event.1\n",
        "    title = test_event.1.t\n",
        "    option = { name = opt_bad_country\n",
        "        add_province_modifier = { name = country_mod duration = 100 }\n",
        "    }\n",
        "    option = { name = opt_bad_province\n",
        "        add_country_modifier = { name = province_mod duration = 100 }\n",
        "    }\n",
        "    option = { name = opt_ok\n",
        "        add_country_modifier = { name = country_mod duration = 100 }\n",
        "    }\n",
        "    option = { name = opt_mixed\n",
        "        add_permanent_province_modifier = { name = mixed_mod duration = -1 }\n",
        "    }\n",
        "    option = { name = opt_unit_country\n",
        "        add_country_modifier = { name = unit_mod duration = 100 }\n",
        "    }\n",
        "}\n",
    );
    let root = temp_root("modifier-scope");
    let modifiers_dir = root.join("common/event_modifiers");
    let events_dir = root.join("events");
    std::fs::create_dir_all(&modifiers_dir).expect("modifier directory");
    std::fs::create_dir_all(&events_dir).expect("events directory");
    std::fs::write(modifiers_dir.join("00_mods.txt"), modifiers).expect("write modifiers");
    std::fs::write(events_dir.join("golden_events.txt"), text).expect("write events");
    let mut host = first_party_host(&root);
    let id = DocumentId::new("file:///tmp/events/golden_events.txt");
    host.open_document(
        id.clone(),
        1,
        text.to_owned(),
        Some(events_dir.join("golden_events.txt")),
    )
    .expect("open golden modifiers");
    assert_golden("modifier_scope_mismatch", text, &analyze_text(&host, &id));
    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn golden_localisation_derived_keys() {
    let text = "series = { mission_one = { potential = { always = yes } } }\n";
    let root = temp_root("loc");
    std::fs::create_dir_all(root.join("missions")).expect("missions directory");
    let mut host = first_party_host(&root);
    let id = DocumentId::new("file:///tmp/missions/golden.txt");
    host.open_document(
        id.clone(),
        1,
        text.to_owned(),
        Some(root.join("missions/golden.txt")),
    )
    .expect("open golden missions");
    assert_golden("localisation_derived_keys", text, &analyze_text(&host, &id));
    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn golden_mission_trees() {
    let text = concat!(
        "golden_main_tree = {\n",
        "\tslot = 1\n",
        "\tpotential = { always = yes }\n",
        "\tgm_root = {\n",
        "\t\ticon = mission_conquest\n",
        "\t\ttrigger = { always = yes }\n",
        "\t\teffect = { add_prestige = 1 }\n",
        "\t\tposition = 1\n",
        "\t}\n",
        "\tgm_second = {\n",
        "\t\ticon = mission_conquest\n",
        "\t\ttrigger = { always = yes }\n",
        "\t\teffect = { add_prestige = 1 }\n",
        "\t\tposition = 2\n",
        "\t\trequired_missions = { gm_root }\n",
        "\t}\n",
        "\tgm_zero = {\n",
        "\t\ticon = mission_conquest\n",
        "\t\ttrigger = { always = yes }\n",
        "\t\teffect = { add_prestige = 1 }\n",
        "\t\tposition = 0\n",
        "\t}\n",
        "\tgm_dup = {\n",
        "\t\ticon = mission_conquest\n",
        "\t\ttrigger = { always = yes }\n",
        "\t\teffect = { add_prestige = 1 }\n",
        "\t\tposition = 3\n",
        "\t}\n",
        "\tgm_dup = {\n",
        "\t\ticon = mission_conquest\n",
        "\t\ttrigger = { always = yes }\n",
        "\t\teffect = { add_prestige = 1 }\n",
        "\t\tposition = 4\n",
        "\t}\n",
        "}\n",
        "golden_branch_tree = {\n",
        "\tslot = 2\n",
        "\tpotential = { always = yes }\n",
        "\tgb_far = {\n",
        "\t\ticon = mission_conquest\n",
        "\t\ttrigger = { always = yes }\n",
        "\t\teffect = { add_prestige = 1 }\n",
        "\t\tposition = 3\n",
        "\t\trequired_missions = { gm_root gm_external gm_ghost }\n",
        "\t}\n",
        "}\n",
        "golden_cycle_tree = {\n",
        "\tslot = 3\n",
        "\tpotential = { always = yes }\n",
        "\tgt_c1 = {\n",
        "\t\ticon = mission_conquest\n",
        "\t\ttrigger = { always = yes }\n",
        "\t\teffect = { add_prestige = 1 }\n",
        "\t\tposition = 1\n",
        "\t\trequired_missions = { gt_c2 }\n",
        "\t}\n",
        "\tgt_c2 = {\n",
        "\t\ticon = mission_conquest\n",
        "\t\ttrigger = { always = yes }\n",
        "\t\teffect = { add_prestige = 1 }\n",
        "\t\tposition = 2\n",
        "\t\trequired_missions = { gt_c1 }\n",
        "\t}\n",
        "}\n",
    );
    let root = temp_root("missions");
    let missions_dir = root.join("missions");
    let loc_dir = root.join("localisation");
    let interface_dir = root.join("interface");
    std::fs::create_dir_all(&missions_dir).expect("missions directory");
    std::fs::create_dir_all(&loc_dir).expect("localisation directory");
    std::fs::create_dir_all(&interface_dir).expect("interface directory");
    // Mission icons are sprite references; declaring the sprite keeps the
    // golden focused on the mission dependency family.
    std::fs::write(
        interface_dir.join("golden.gfx"),
        "spriteTypes = { spriteType = { name = \"mission_conquest\" texturefile = \"gfx/none.dds\" } }\n",
    )
    .expect("write gfx sprite");
    // A second mission file: cross-file prerequisites resolve through the
    // workspace mission universe (EU4 1.35+ branching missions).
    std::fs::write(
        missions_dir.join("golden_other.txt"),
        "golden_other_tree = {\n\
         \tslot = 4\n\
         \tpotential = { always = yes }\n\
         \tgm_external = {\n\
         \t\ticon = mission_conquest\n\
         \t\ttrigger = { always = yes }\n\
         \t\teffect = { add_prestige = 1 }\n\
         \t\tposition = 1\n\
         \t}\n\
         }\n",
    )
    .expect("write other missions file");
    // Every mission renders `{id}_title`/`{id}_desc`; providing them keeps the
    // golden focused on the mission dependency family.
    let mut loc = String::from("l_english:\n");
    for id in [
        "gm_root",
        "gm_second",
        "gm_zero",
        "gm_dup",
        "gb_far",
        "gt_c1",
        "gt_c2",
    ] {
        loc.push_str(&format!(" {id}_title:0 \"Title\"\n {id}_desc:0 \"Desc\"\n"));
    }
    std::fs::write(loc_dir.join("golden_missions_l_english.yml"), loc).expect("write localisation");
    let mut host = first_party_host(&root);
    let focus = DocumentId::new("file:///tmp/missions/golden_main.txt");
    host.open_document(
        focus.clone(),
        1,
        text.to_owned(),
        Some(missions_dir.join("golden_main.txt")),
    )
    .expect("open golden mission file");
    assert_golden("mission_trees", text, &analyze_text(&host, &focus));
    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn golden_quoted_script_syntax() {
    let text = "trigger = { embedded = \"\n foo = maybe\n broken = {\n\" }\n";
    let (host, id) = quoted_script_snapshot(text);
    assert_golden("quoted_script_syntax", text, &analyze_text(&host, &id));
}
