#![allow(unused_imports)]

use super::support::*;

#[test]
fn symbol_hover_does_not_materialize_the_full_workspace() {
    let (host, id) = snapshot("country_event = { id = hover.1 }\nevent = hover.1\n");
    let snapshot = host.snapshot();
    let position = u32::try_from("country_event = { id = hover.1 }\nevent = ".len() + 1)
        .expect("reference offset");

    crate::ALL_SEMANTICS_CALLS.with(|calls| calls.set(0));
    let hover = hover(&snapshot, &id, position).expect("symbol hover");
    assert!(hover.contents.contains("Resolved definition"));
    crate::ALL_SEMANTICS_CALLS.with(|calls| {
        assert_eq!(
            calls.get(),
            0,
            "symbol hover must query the current file and symbol bucket directly"
        );
    });
}

#[test]
fn symbol_hover_explains_active_and_shadowed_source_roots() {
    use pdx_engine::{SourceRoot, SourceRootId, SourceRootKind, WorkspaceChange};
    use std::fs;

    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-analysis-hover-sources-{nonce}"));
    let vanilla = root.join("vanilla");
    let current = root.join("current");
    fs::create_dir_all(vanilla.join("common/events")).expect("Vanilla directory");
    fs::create_dir_all(current.join("common/events")).expect("current directory");
    for source_root in [&vanilla, &current] {
        fs::write(
            source_root.join("common/events/definitions.txt"),
            "country_event = { id = shared.1 }\n",
        )
        .expect("event definition");
    }

    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![
        SourceRoot {
            id: SourceRootId::new(1),
            kind: SourceRootKind::Vanilla,
            path: vanilla,
            order: 0,
            writable: false,
        },
        SourceRoot {
            id: SourceRootId::new(2),
            kind: SourceRootKind::CurrentMod,
            path: current,
            order: 0,
            writable: true,
        },
    ]));
    host.refresh_source_roots().expect("scan source roots");
    let id = DocumentId::new("file:///tmp/events/reference.txt");
    let text = "event = shared.1\n";
    host.open_document(id.clone(), 1, text.to_owned(), None)
        .expect("open reference");
    let position = u32::try_from(text.find("shared.1").expect("reference") + 1).expect("position");
    let hover = hover(&host.snapshot(), &id, position).expect("source hover");
    assert!(
        hover
            .contents
            .contains("#### Resolved definition\n\n- Source root:")
    );
    assert!(hover.contents.contains("Source root: Current Mod"));
    assert!(hover.contents.contains("#### Shadowed definitions:"));
    assert!(hover.contents.contains("Shadowed definitions:"));
    assert!(hover.contents.contains("Vanilla"));
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn semantic_hover_explains_scope_transition() {
    let (host, id) = {
        let mut host =
            eu4_host(pdx_game::eu4::first_party_rules().expect("load first-party rules"));
        let id = DocumentId::new("file:///tmp/events/scope-hover.txt");
        host.open_document(
            id.clone(),
            1,
            "country_event = { immediate = { capital_scope = { } } }\n".to_owned(),
            None,
        )
        .expect("open scope fixture");
        (host, id)
    };
    let text = "country_event = { immediate = { capital_scope = { } } }\n";
    let position =
        u32::try_from(text.find("capital_scope").expect("scope link") + 1).expect("position");
    let hover = hover(&host.snapshot(), &id, position).expect("scope hover");
    assert!(hover.contents.contains("scope transition:"));
    assert!(hover.contents.contains("scope registers: ROOT=`"));
    assert!(hover.contents.contains("scope registers after:"));
    assert!(hover.contents.contains("child context:") || hover.contents.contains("context:"));
}

#[test]
fn hover_ignores_unknown_property_and_plain_text() {
    let (host, id) = semantic_snapshot("trigger = { unknown_property = yes }\n");
    let analysis_snapshot = host.snapshot();
    let property = u32::try_from("trigger = { ".len() + 2).expect("offset");
    assert!(hover(&analysis_snapshot, &id, property).is_none());

    let (host, id) = snapshot("# ordinary comment text\n");
    assert!(
        hover(
            &host.snapshot(),
            &id,
            u32::try_from("# ordinary ".len()).expect("offset")
        )
        .is_none()
    );
}

#[test]
fn semantic_hover_keeps_multiple_matching_rule_meanings() {
    let mut model = pdx_game::eu4::bootstrap_model();
    for (id, value) in [
        ("fixture:trigger:choice-bool", ValueMatcher::Bool),
        (
            "fixture:trigger:choice-int",
            ValueMatcher::Int {
                min: Some(1),
                max: Some(3),
            },
        ),
    ] {
        model.semantic.rules.push(SemanticRule {
            id: id.to_owned(),
            context: "trigger".to_owned(),
            parent_path: Vec::new(),
            key: KeyMatcher::Exact("choice".to_owned()),
            operator: Some("=".to_owned()),
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
            max_occurs: Some(1),
            source_file: "fixture.semantic".to_owned(),
            line: 1,
        });
    }
    let mut host = eu4_host(RuleSet::from_model(model));
    let id = DocumentId::new("file:///tmp/choice.txt");
    let text = "trigger = { choice = yes }\n";
    host.open_document(id.clone(), 1, text.to_owned(), None)
        .expect("open ambiguous rule fixture");
    let position = u32::try_from(text.find("choice").expect("choice") + 1).expect("position");
    let hover = hover(&host.snapshot(), &id, position).expect("ambiguous rule hover");
    assert!(hover.contents.contains("2 possible semantic meanings"));
    assert!(hover.contents.contains("#### 2 possible semantic meanings"));
    assert!(hover.contents.contains("##### Candidate 1"));
    assert!(hover.contents.contains("value: `bool (`yes` / `no`)`"));
    assert!(hover.contents.contains("value: `integer in [1, 3]`"));
}

#[test]
fn semantic_hover_preserves_rule_detail_line_breaks() {
    let mut model = pdx_game::eu4::bootstrap_model();
    model.semantic.rules.push(SemanticRule {
        id: "fixture:trigger:documented".to_owned(),
        context: "trigger".to_owned(),
        parent_path: Vec::new(),
        key: KeyMatcher::Exact("documented".to_owned()),
        operator: None,
        value: ValueMatcher::Bool,
        shape: RuleShape::Leaf,
        child_context: None,
        alternative_id: None,
        severity: None,
        required: false,
        deprecated: false,
        documentation: vec!["first line".to_owned(), "second line".to_owned()],
        allowed_scopes: Vec::new(),
        push_scope: None,
        replace_scope: Vec::new(),
        min_occurs: None,
        strict_min: true,
        max_occurs: Some(1),
        source_file: "fixture.semantic".to_owned(),
        line: 1,
    });
    let mut host = eu4_host(RuleSet::from_model(model));
    let id = DocumentId::new("file:///tmp/documented.txt");
    let text = "trigger = { documented = yes }\n";
    host.open_document(id.clone(), 1, text.to_owned(), None)
        .expect("open documented fixture");
    let position = u32::try_from(text.find("documented").expect("documented key") + 2)
        .expect("hover position");
    let hover = hover(&host.snapshot(), &id, position).expect("documented hover");
    assert!(
        hover
            .contents
            .contains("#### Documentation\n\nfirst line  \nsecond line")
    );
    assert!(hover.contents.contains("first line  \nsecond line"));
}

#[test]
fn localisation_hover_shows_the_resolved_short_text() {
    let mut host = eu4_host(pdx_game::eu4::bootstrap_rules());
    let id = DocumentId::new("file:///tmp/localisation/test.yml");
    let text = "l_english:\nfoo_name:0 \"Foo\"\n";
    host.open_document(id.clone(), 1, text.to_owned(), None)
        .expect("open localisation");
    let position =
        u32::try_from(text.find("foo_name").expect("localisation key") + 2).expect("position");
    let hover = hover(&host.snapshot(), &id, position).expect("localisation hover");
    assert!(
        hover
            .contents
            .contains("#### Localisation preview\n\n- Localisation")
    );
    assert!(hover.contents.contains("Localisation (l_english): \"Foo\""));
}

#[test]
fn hover_prefers_nonempty_localisation_preview_over_empty_sibling() {
    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(1),
        SourceRootKind::CurrentMod,
        std::path::PathBuf::from("/tmp"),
    )]));
    let localisation = DocumentId::new("file:///tmp/localisation/test.yml");
    host.open_document(
        localisation.clone(),
        1,
        "l_english:\nmission_one_title:0 \"Mission One Title\"\nmission_one_desc:0 \"\"\n"
            .to_owned(),
        Some(std::path::PathBuf::from("/tmp/localisation/test.yml")),
    )
    .expect("open localisation");
    let mission = DocumentId::new("file:///tmp/missions/test.txt");
    let source = "series = { mission_one = { potential = { always = yes } } }\n";
    host.open_document(
        mission.clone(),
        1,
        source.to_owned(),
        Some(std::path::PathBuf::from("/tmp/missions/test.txt")),
    )
    .expect("open mission");

    let position =
        u32::try_from(source.find("mission_one").expect("mission name") + 4).expect("position");
    let hover = hover(&host.snapshot(), &mission, position).expect("mission hover");
    assert!(
        hover
            .contents
            .contains("Localisation (l_english): \"Mission One Title\""),
        "hover should prefer the non-empty title preview: {}",
        hover.contents
    );
    assert!(!hover.contents.contains("mission_one_desc"));
}

#[test]
fn custom_tooltip_hover_shows_localisation_preview_inside_mission_effects() {
    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(1),
        SourceRootKind::CurrentMod,
        std::path::PathBuf::from("/tmp"),
    )]));
    let localisation = DocumentId::new("file:///tmp/localisation/test.yml");
    host.open_document(
        localisation.clone(),
        1,
        "l_english:\nEDG_TEST_TT:0 \"My tooltip text\"\n".to_owned(),
        Some(std::path::PathBuf::from("/tmp/localisation/test.yml")),
    )
    .expect("open localisation");
    let mission = DocumentId::new("file:///tmp/missions/test.txt");
    let source = "series = { mission_one = { effect = { custom_tooltip = EDG_TEST_TT } } }\n";
    host.open_document(
        mission.clone(),
        1,
        source.to_owned(),
        Some(std::path::PathBuf::from("/tmp/missions/test.txt")),
    )
    .expect("open mission");

    let position =
        u32::try_from(source.find("EDG_TEST_TT").expect("tooltip key") + 4).expect("position");
    let hover = hover(&host.snapshot(), &mission, position).expect("tooltip hover");
    assert!(
        hover
            .contents
            .contains("Localisation (l_english): \"My tooltip text\""),
        "custom_tooltip inside a mission effect should resolve to the localisation preview: {}",
        hover.contents
    );
}

#[test]
fn vanilla_cache_localisation_hover_shows_derived_text_without_source_state() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-analysis-vanilla-hover-{nonce}"));
    let vanilla = root.join("vanilla");
    let current = root.join("current");
    std::fs::create_dir_all(vanilla.join("localisation/nested")).expect("Vanilla directory");
    std::fs::create_dir_all(current.join("events")).expect("current directory");
    std::fs::write(
        vanilla.join("localisation/nested/test_l_english.yml"),
        "l_english:\ncached_name:0 \"Cached Vanilla text\"\n",
    )
    .expect("Vanilla localisation");

    let mut vanilla_host = eu4_host(pdx_game::eu4::bootstrap_rules());
    vanilla_host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(0),
        SourceRootKind::Vanilla,
        vanilla.clone(),
    )]));
    vanilla_host
        .refresh_source_roots()
        .expect("scan Vanilla for cache");
    let cache =
        VanillaIndexCache::from_snapshot(&vanilla_host.snapshot()).expect("build Vanilla cache");
    let localisation_file = cache
        .index()
        .active_definition("localisation", "cached_name")
        .expect("cached localisation definition")
        .file_id;

    let mut host = eu4_host(pdx_game::eu4::bootstrap_rules());
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(1),
        SourceRootKind::CurrentMod,
        current.clone(),
    )]));
    host.install_vanilla_cache(cache)
        .expect("install Vanilla cache");
    let document = DocumentId::new("file:///current/events/hover.txt");
    let text = "country_event = { title = cached_name }\n";
    host.open_document(
        document.clone(),
        1,
        text.to_owned(),
        Some(current.join("events/hover.txt")),
    )
    .expect("open current script");
    let position =
        u32::try_from(text.find("cached_name").expect("localisation reference")).expect("position");
    let hover = hover(&host.snapshot(), &document, position).expect("cached localisation hover");
    assert!(
        hover
            .contents
            .contains("Localisation (l_english): \"Cached Vanilla text\"")
    );
    assert!(host.snapshot().file_state(localisation_file).is_none());
    std::fs::remove_dir_all(root).expect("cleanup");
}
