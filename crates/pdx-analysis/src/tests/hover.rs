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
fn semantic_hover_descends_into_quoted_script_with_mapped_range() {
    let text = "trigger = { embedded = \"foo = yes\" }\n";
    let (host, id) = quoted_script_snapshot(text);
    let start = u32::try_from(text.find("foo").expect("inner key")).expect("offset");
    let snapshot = host.snapshot();
    let input = input_for_document(&snapshot, &id).expect("input");
    let context = semantic_completion_context(&snapshot, &input, start + 1)
        .unwrap_or_else(|| panic!("missing quoted semantic context"));
    assert_eq!(
        context
            .property
            .as_ref()
            .map(|property| property.key.as_ref()),
        Some("foo"),
        "{context:?}"
    );

    let hover = hover(&snapshot, &id, start + 1).expect("quoted semantic hover");

    assert!(hover.contents.contains("PDX property `foo`"), "{hover:?}");
    assert_eq!(
        hover.range,
        Some(TextRange::new(start, start + 3).expect("range"))
    );
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
    fs::create_dir_all(vanilla.join("events")).expect("Vanilla directory");
    fs::create_dir_all(current.join("events")).expect("current directory");
    for source_root in [&vanilla, &current] {
        fs::write(
            source_root.join("events/definitions.txt"),
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
    assert!(!hover.contents.contains("scope registers:"));
    assert!(!hover.contents.contains("scope registers after:"));
    assert!(!hover.contents.contains("child context:"));
    assert!(!hover.contents.contains("context:"));
}

#[test]
fn decision_hover_skips_type_instance_wrapper() {
    use std::path::PathBuf;

    let text = concat!(
        "country_decisions = {\n",
        "  my_decision = {\n",
        "    potential = { }\n",
        "  }\n",
        "}\n",
    );
    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    let id = DocumentId::new("file:///tmp/common/decisions/hover.txt");
    host.open_document(
        id.clone(),
        1,
        text.to_owned(),
        Some(PathBuf::from("common/decisions/hover.txt")),
    )
    .expect("open decision document");
    let position =
        u32::try_from(text.find("potential").expect("potential key") + 1).expect("position");
    let result = hover(&host.snapshot(), &id, position).expect("decision semantic hover");
    assert!(result.contents.contains("### PDX property `potential`"));
    assert!(result.contents.contains("- at least 1"), "{result:?}");
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
    assert!(hover.contents.contains("#### Possible meanings (2)"));
    assert!(!hover.contents.contains("##### Candidate 1"));
    assert!(hover.contents.contains("value: bool (`yes` / `no`)"));
    assert!(hover.contents.contains("value: integer in [1, 3]"));
}

#[test]
fn semantic_hover_collapses_repeated_first_party_rule_rows() {
    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("load first-party rules"));
    let id = DocumentId::new("file:///tmp/common/on_actions/hover.txt");
    let text = "on_action = { events = { test_event = { } } }\n";
    host.open_document(id.clone(), 1, text.to_owned(), None)
        .expect("open on_action fixture");
    let position =
        u32::try_from(text.find("events").expect("events key") + 1).expect("hover position");
    let hover = hover(&host.snapshot(), &id, position).expect("repeated-rule hover");
    assert_eq!(hover.contents.matches("- value:").count(), 1);
    assert!(!hover.contents.contains("Possible meanings"));
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
fn localisation_values_by_key_resolve_english_preferred_titles() {
    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    host.open_document(
        DocumentId::new("file:///tmp/localisation/l_english/test_l_english.yml"),
        1,
        "l_english:\nmission_one_title:0 \"Mission One Title\"\nmission_two_title:0 \"\"\n"
            .to_owned(),
        Some(std::path::PathBuf::from(
            "/tmp/localisation/l_english/test_l_english.yml",
        )),
    )
    .expect("open english localisation");
    host.open_document(
        DocumentId::new("file:///tmp/localisation/l_french/test_l_french.yml"),
        1,
        "l_french:\nmission_one_title:0 \"Titre Mission Un\"\n".to_owned(),
        Some(std::path::PathBuf::from(
            "/tmp/localisation/l_french/test_l_french.yml",
        )),
    )
    .expect("open french localisation");

    let snapshot = host.snapshot();
    let resolved = crate::localisation_values_by_key(
        &snapshot,
        &["mission_one_title", "mission_two_title", "missing_key"],
        &crate::CancellationToken::new(),
    )
    .expect("resolve titles");
    let (language, value) = resolved
        .get("mission_one_title")
        .expect("active title definition");
    assert_eq!(language.as_deref(), Some("l_english"));
    assert_eq!(value, "Mission One Title");
    // Empty values and unknown keys must not resolve.
    assert!(!resolved.contains_key("mission_two_title"));
    assert!(!resolved.contains_key("missing_key"));
}

#[test]
fn localisation_values_by_key_uses_index_priority_and_english_preference() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-analysis-localisation-values-{nonce}"));
    let vanilla = root.join("vanilla");
    let current = root.join("current");
    std::fs::create_dir_all(vanilla.join("localisation")).expect("Vanilla directory");
    std::fs::create_dir_all(current.join("localisation")).expect("current directory");
    std::fs::write(
        vanilla.join("localisation/l_english.yml"),
        "l_english:\nshared_title:0 \"English Vanilla\"\nvanilla_only_title:0 \"Vanilla Only Title\"\nlang_title:0 \"English Vanilla\"\n",
    )
    .expect("Vanilla English localisation");
    std::fs::write(
        vanilla.join("localisation/l_french.yml"),
        "l_french:\nlang_title:0 \"French Vanilla\"\n",
    )
    .expect("Vanilla French localisation");
    std::fs::write(
        current.join("localisation/l_english.yml"),
        "l_english:\nshared_title:0 \"English Mod\"\ncurrent_only_title:0 \"Current Only Title\"\n",
    )
    .expect("Current Mod localisation");

    // Vanilla runs through the same cache-installed path the LSP uses, which is
    // what retains its localisation previews for the derived text lookup.
    let mut vanilla_host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    vanilla_host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(0),
        SourceRootKind::Vanilla,
        vanilla,
    )]));
    vanilla_host.refresh_source_roots().expect("scan Vanilla");
    let cache = IndexCache::from_snapshot(&vanilla_host.snapshot()).expect("build Vanilla cache");

    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(1),
        SourceRootKind::CurrentMod,
        current.clone(),
    )]));
    host.install_index_cache(cache)
        .expect("install Vanilla cache");
    host.refresh_source_roots().expect("scan Current Mod");

    let snapshot = host.snapshot();
    crate::ALL_SEMANTICS_CALLS.with(|calls| calls.set(0));
    let resolved = crate::localisation_values_by_key(
        &snapshot,
        &[
            "shared_title",
            "vanilla_only_title",
            "current_only_title",
            "lang_title",
            "missing_title",
        ],
        &crate::CancellationToken::new(),
    )
    .expect("resolve title keys");
    crate::ALL_SEMANTICS_CALLS.with(|calls| {
        assert_eq!(
            calls.get(),
            0,
            "title resolution must not rebuild the full workspace semantics"
        );
    });

    // A Current Mod override beats the installed Vanilla definition at higher priority.
    let (_, value) = resolved.get("shared_title").expect("mod override");
    assert_eq!(value, "English Mod");
    // Keys defined only in one root still resolve from the index.
    let (_, value) = resolved
        .get("vanilla_only_title")
        .expect("vanilla-only title");
    assert_eq!(value, "Vanilla Only Title");
    let (_, value) = resolved
        .get("current_only_title")
        .expect("current-only title");
    assert_eq!(value, "Current Only Title");
    // With per-language candidates, the English variant wins.
    let (language, value) = resolved.get("lang_title").expect("language preference");
    assert_eq!(value, "English Vanilla");
    assert_eq!(language.as_deref(), Some("l_english"));
    // Preview retention is decided when a cache is installed, from the host's preference
    // order at that moment — mirroring the LSP, which fixes preferences at initialize
    // before the Vanilla cache install. The French pass therefore installs a second cache
    // into a host that already prefers French.
    let mut french_host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    french_host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(1),
        SourceRootKind::CurrentMod,
        current,
    )]));
    french_host.set_preferred_localisation_languages(vec!["french".to_owned()]);
    french_host
        .install_index_cache(
            IndexCache::from_snapshot(&vanilla_host.snapshot()).expect("rebuild Vanilla cache"),
        )
        .expect("install Vanilla cache with French preference");
    french_host
        .refresh_source_roots()
        .expect("scan Current Mod");
    let preferred = crate::localisation_values_by_key(
        &french_host.snapshot(),
        &["lang_title"],
        &crate::CancellationToken::new(),
    )
    .expect("configured language preference");
    let (language, value) = preferred.get("lang_title").expect("French title");
    assert_eq!(value, "French Vanilla");
    assert_eq!(language.as_deref(), Some("l_french"));
    // Unknown keys remain absent.
    assert!(!resolved.contains_key("missing_title"));
    std::fs::remove_dir_all(root).expect("cleanup");
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
fn typed_symbol_hover_shows_definition_localisation_preview() {
    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(1),
        SourceRootKind::CurrentMod,
        std::path::PathBuf::from("/tmp"),
    )]));
    let localisation = DocumentId::new("file:///tmp/localisation/test.yml");
    host.open_document(
        localisation,
        1,
        "l_english:\nevent_title:0 \"Event Title\"\n".to_owned(),
        Some(std::path::PathBuf::from("/tmp/localisation/test.yml")),
    )
    .expect("open localisation");
    let event = DocumentId::new("file:///tmp/events/test.txt");
    let text = "country_event = { id = test.1 title = event_title }\n";
    host.open_document(
        event.clone(),
        1,
        text.to_owned(),
        Some(std::path::PathBuf::from("/tmp/events/test.txt")),
    )
    .expect("open event");
    let use_id = DocumentId::new("file:///tmp/events/use.txt");
    let use_text = "event = test.1\n";
    host.open_document(
        use_id.clone(),
        1,
        use_text.to_owned(),
        Some(std::path::PathBuf::from("/tmp/events/use.txt")),
    )
    .expect("open event use");
    let position =
        u32::try_from(use_text.find("test.1").expect("event reference") + 1).expect("position");
    let result = hover(&host.snapshot(), &use_id, position).expect("event hover");
    assert!(
        result
            .contents
            .contains("Localisation (l_english): \"Event Title\""),
        "typed symbol hover should include its definition's localisation: {}",
        result.contents
    );
}

#[test]
fn optional_type_localisation_hover_shows_existing_preview() {
    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(1),
        SourceRootKind::CurrentMod,
        std::path::PathBuf::from("/tmp"),
    )]));
    host.open_document(
        DocumentId::new("file:///tmp/localisation/test.yml"),
        1,
        "l_english:\nregion_one:0 \"Region One\"\n".to_owned(),
        Some(std::path::PathBuf::from("/tmp/localisation/test.yml")),
    )
    .expect("open localisation");
    let definition = DocumentId::new("file:///tmp/common/colonial_regions/test.txt");
    let source = "region_one = { }\n";
    host.open_document(
        definition.clone(),
        1,
        source.to_owned(),
        Some(std::path::PathBuf::from(
            "/tmp/common/colonial_regions/test.txt",
        )),
    )
    .expect("open colonial region");
    let position =
        u32::try_from(source.find("region_one").expect("region name") + 1).expect("position");
    let result = hover(&host.snapshot(), &definition, position).expect("region hover");
    assert!(
        result
            .contents
            .contains("Localisation (l_english): \"Region One\""),
        "optional type mappings should contribute existing localisation previews: {}",
        result.contents
    );
}

#[test]
fn same_name_type_localisation_hover_shows_existing_preview() {
    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(1),
        SourceRootKind::CurrentMod,
        std::path::PathBuf::from("/tmp"),
    )]));
    host.open_document(
        DocumentId::new("file:///tmp/localisation/test.yml"),
        1,
        "l_english:\neurope:0 \"Europe\"\n".to_owned(),
        Some(std::path::PathBuf::from("/tmp/localisation/test.yml")),
    )
    .expect("open localisation");
    let definition = DocumentId::new("file:///tmp/map/continent.txt");
    let source = "europe = { }\n";
    host.open_document(
        definition.clone(),
        1,
        source.to_owned(),
        Some(std::path::PathBuf::from("/tmp/map/continent.txt")),
    )
    .expect("open continent");
    let position =
        u32::try_from(source.find("europe").expect("continent name") + 1).expect("position");
    let result = hover(&host.snapshot(), &definition, position).expect("continent hover");
    assert!(
        result
            .contents
            .contains("Localisation (l_english): \"Europe\""),
        "same-name type localisations should contribute existing previews: {}",
        result.contents
    );
}

#[test]
fn cache_only_optional_type_hover_shows_existing_preview() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-analysis-optional-hover-{nonce}"));
    let vanilla = root.join("vanilla");
    let current = root.join("current");
    std::fs::create_dir_all(vanilla.join("common/colonial_regions")).expect("Vanilla directory");
    std::fs::create_dir_all(vanilla.join("localisation")).expect("Vanilla localisation directory");
    std::fs::create_dir_all(current.join("events")).expect("Current directory");
    std::fs::write(
        vanilla.join("common/colonial_regions/test.txt"),
        "region_one = { }\n",
    )
    .expect("Vanilla colonial region");
    std::fs::write(
        vanilla.join("localisation/test_l_english.yml"),
        "l_english:\nregion_one:0 \"Region One\"\n",
    )
    .expect("Vanilla localisation");

    let rules = pdx_game::eu4::first_party_rules().expect("first-party rules");
    let mut profile = pdx_game::eu4::profile();
    profile.references.insert(
        0,
        pdx_rules::ProfileReferenceRule {
            key: ProfileTextMatcher::insensitive(ProfileMatchMode::Exact, "custom_region"),
            kind: "colonial_region".to_owned(),
            excluded_keys: Vec::new(),
            excluded_paths: Vec::new(),
        },
    );
    let mut vanilla_host = AnalysisHost::with_profile(rules.clone(), profile.clone());
    vanilla_host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(0),
        SourceRootKind::Vanilla,
        vanilla.clone(),
    )]));
    vanilla_host
        .refresh_source_roots()
        .expect("scan Vanilla for cache");
    let cache = IndexCache::from_snapshot(&vanilla_host.snapshot()).expect("build Vanilla cache");

    let mut host = AnalysisHost::with_profile(rules, profile);
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(1),
        SourceRootKind::CurrentMod,
        current.clone(),
    )]));
    host.install_index_cache(cache)
        .expect("install Vanilla cache");
    let document = DocumentId::new("file:///current/events/use.txt");
    let text = "trigger = { custom_region = region_one }\n";
    host.open_document(
        document.clone(),
        1,
        text.to_owned(),
        Some(current.join("events/use.txt")),
    )
    .expect("open use");
    let position =
        u32::try_from(text.find("region_one").expect("region reference") + 1).expect("position");
    let result = hover(&host.snapshot(), &document, position).expect("cached hover");
    assert!(
        result
            .contents
            .contains("Localisation (l_english): \"Region One\""),
        "cache-only optional mappings should contribute existing previews: {}",
        result.contents
    );
    std::fs::remove_dir_all(root).expect("cleanup");
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
    let cache = IndexCache::from_snapshot(&vanilla_host.snapshot()).expect("build Vanilla cache");
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
    host.install_index_cache(cache)
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
