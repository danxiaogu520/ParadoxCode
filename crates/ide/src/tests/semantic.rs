use super::support::*;
use text::AbsPath;

#[test]
fn query_input_reuses_the_document_hir_handle() {
    let (host, id) = snapshot("country_event = { id = shared.1 }\n");
    let snapshot = host.snapshot();
    let document_hir = snapshot
        .document(&id)
        .expect("document")
        .hir_handle()
        .expect("HIR");
    let input = input_for_document(&snapshot, &id).expect("analysis input");
    let input_hir = input.hir.as_ref().expect("shared analysis HIR");

    assert!(std::sync::Arc::ptr_eq(&document_hir, input_hir));
}

#[test]
fn identity_only_host_does_not_guess_eu4_semantics_from_game_id() {
    let mut host = AnalysisHost::new(game::eu4::bootstrap_rules());
    let id = DocumentId::new("file:///tmp/events/generic.txt");
    host.open_document(
        id.clone(),
        1,
        "country_event = { id = generic.1 scope = country }\n".to_owned(),
        None,
    )
    .expect("open");

    let snapshot = host.snapshot();
    assert!(document_symbols(&snapshot, &id).is_empty());
    assert!(
        diagnostics(&snapshot, &id)
            .iter()
            .all(|item| item.code != DiagnosticCode::InvalidValue)
    );
}

#[test]
fn eu4_profile_supplies_known_scope_spellings() {
    let (host, id) = snapshot("country_event = { scope = country }\n");

    assert!(
        diagnostics(&host.snapshot(), &id)
            .iter()
            .all(|item| !matches!(item.code, DiagnosticCode::InvalidValue))
    );
}

#[test]
fn logical_scope_wrappers_keep_the_trigger_context() {
    let (host, id) = semantic_snapshot("trigger = { OR = { foo = yes } NOT = { foo = no } }\n");
    let results = diagnostics(&host.snapshot(), &id);
    assert!(
        !results
            .iter()
            .any(|item| item.code == DiagnosticCode::UnknownKey)
    );
}

#[test]
fn alias_definition_cardinality_does_not_limit_repeated_effect_commands() {
    let rules = game::eu4::runtime_rules().expect("load first-party rules");
    let mut host = eu4_host(rules);
    let id = DocumentId::new("file:///tmp/events/repeated-tooltip.txt");
    host.open_document(
        id.clone(),
        1,
        "effect = { custom_tooltip = first custom_tooltip = second }\n".to_owned(),
        None,
    )
    .expect("open");
    let results = diagnostics(&host.snapshot(), &id);
    assert!(
        !results
            .iter()
            .any(|item| item.code == DiagnosticCode::Cardinality)
    );
}

#[test]
fn semantic_type_selector_applies_event_rules_to_country_event() {
    let rules = game::eu4::runtime_rules().expect("load first-party rules");
    let mut host = eu4_host(rules);
    let id = DocumentId::new("file:///tmp/events/test.txt");
    host.open_document(
        id.clone(),
        1,
        "country_event = { id = test.1 definitely_not_an_event_key = yes }\n".to_owned(),
        None,
    )
    .expect("open");
    assert!(
        diagnostics(&host.snapshot(), &id)
            .iter()
            .any(|item| item.code == DiagnosticCode::UnknownKey)
    );
}

#[test]
fn area_scope_transition_keeps_province_trigger_valid() {
    use engine::{SourceRoot, SourceRootId, SourceRootKind, WorkspaceChange};
    use std::fs;

    let rules = game::eu4::runtime_rules().expect("load first-party rules");
    let mut host = eu4_host(rules);
    let root = std::env::temp_dir().join(format!(
        "ide-area-scope-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let area_path = root.join("map/area.txt");
    let event_path = root.join("events/demo_events.txt");
    fs::create_dir_all(area_path.parent().expect("area parent")).expect("area directory");
    fs::create_dir_all(event_path.parent().expect("event parent")).expect("event directory");
    fs::write(&area_path, "tripolitania_area = { 1 2 }\n").expect("area source");
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(1),
        SourceRootKind::Project,
        AbsPath::normalize(&root),
    )]));
    host.refresh_source_roots().expect("index area definitions");
    let id = DocumentId::new("file:///tmp/events/demo_events.txt");
    let text = concat!(
        "country_event = {\n",
        "  immediate = {\n",
        "    tripolitania_area = {\n",
        "      limit = { country_or_non_sovereign_subject_holds = ROOT }\n",
        "    }\n",
        "  }\n",
        "}\n",
    );
    host.open_document(
        id.clone(),
        1,
        text.to_owned(),
        Some(AbsPath::normalize(&event_path)),
    )
    .expect("open");

    let results = diagnostics(&host.snapshot(), &id);
    assert!(
        !results.iter().any(|item| {
            item.code == DiagnosticCode::UnknownKey && item.message.contains("`tripolitania_area`")
        }),
        "area name was not accepted as a dynamic area key: {results:?}"
    );
    assert!(
        !results.iter().any(|item| {
            item.code == DiagnosticCode::WrongScope
                && item
                    .message
                    .contains("`country_or_non_sovereign_subject_holds`")
        }),
        "province trigger was diagnosed in the parent country scope: {results:?}"
    );
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn eu4_normal_type_selector_applies_mission_rules_to_custom_root_names() {
    use engine::{SourceRoot, SourceRootId, SourceRootKind, WorkspaceChange};
    use std::fs;

    let root = std::env::temp_dir().join(format!(
        "ide-cwt-missions-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    fs::create_dir_all(root.join("missions")).expect("missions directory");
    let rules = game::eu4::runtime_rules().expect("load first-party rules");
    let mut host = eu4_host(rules);
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(1),
        SourceRootKind::Project,
        AbsPath::normalize(&root),
    )]));
    let path = root.join("missions/DEMO_Bavarian_Missions.txt");
    let source = "DEMO_Bavarian_Missions = { slot = 1 generic = no ai = yes has_country_shield = yes potential = { } demo_bav_claim = { required_missions = { potential } } }\n";
    fs::write(&path, source).expect("write mission document");
    host.refresh_source_roots().expect("index mission document");
    let id = DocumentId::new("file:///tmp/DEMO_Bavarian_Missions.txt");
    host.open_document(
        id.clone(),
        1,
        source.to_owned(),
        Some(AbsPath::normalize(&path)),
    )
    .expect("open mission document");
    let results = diagnostics(&host.snapshot(), &id);
    assert!(
        !results.iter().any(|item| {
            item.code == DiagnosticCode::UnknownKey
                && [
                    "slot",
                    "generic",
                    "ai",
                    "has_country_shield",
                    "demo_bav_claim",
                ]
                .iter()
                .any(|key| item.message.contains(&format!("`{key}`")))
        }),
        "mission fields were not selected by the path-based type: {results:?}"
    );
    assert!(
        results.iter().any(|item| {
            item.code == DiagnosticCode::InvalidDependency
                && item.message.contains("unknown mission `potential`")
        }),
        "negative type_key_filter was not applied to <mission>: {results:?}"
    );
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn eu4_starts_with_type_selector_applies_on_action_rules() {
    let rules = game::eu4::runtime_rules().expect("load first-party rules");
    let mut host = eu4_host(rules);
    let id = DocumentId::new("file:///tmp/common/on_actions/test.txt");
    host.open_document(
        id.clone(),
        1,
        "on_harmonized_religiongroup = { definitely_not_an_on_action_key = yes }\n".to_owned(),
        None,
    )
    .expect("open");
    assert!(
        diagnostics(&host.snapshot(), &id)
            .iter()
            .any(|item| item.code == DiagnosticCode::UnknownKey)
    );
}

#[test]
fn eu4_starts_with_type_selector_still_requires_a_matching_path() {
    let rules = game::eu4::runtime_rules().expect("load first-party rules");
    let host = eu4_host(rules);
    let valid_path =
        LogicalPath::parse("common/on_actions/test.txt").expect("valid on-action path");
    let unrelated_path = LogicalPath::parse("events/test.txt").expect("valid unrelated path");

    let mut host = host;
    for (path, expected) in [(valid_path, true), (unrelated_path, false)] {
        let id = DocumentId::new(format!("file:///tmp/{path}"));
        host.open_document(
            id.clone(),
            1,
            "on_harmonized_religiongroup = {}".into(),
            None,
        )
        .unwrap();
        let snapshot = host.snapshot();
        let input = input_for_document(&snapshot, &id).unwrap();
        let hir = input.hir.as_ref().unwrap();
        assert_eq!(
            hir.definitions()
                .iter()
                .any(|definition| definition.kind.as_ref() == "on_action"
                    && definition.name == "on_harmonized_religiongroup"),
            expected
        );
        if !expected {
            assert!(
                diagnostics(&snapshot, &id)
                    .iter()
                    .any(|diagnostic| diagnostic.code == DiagnosticCode::UnknownKey)
            );
        }
    }
}

#[test]
fn eu4_alias_alternatives_do_not_cross_report_cardinality() {
    let rules = game::eu4::runtime_rules().expect("load first-party rules");
    let mut host = eu4_host(rules);
    let id = DocumentId::new("file:///tmp/events/alternatives.txt");
    host.open_document(
        id.clone(),
        1,
        "effect = { multiply_variable = { which = $foo$ value = 1 } }\n".to_owned(),
        None,
    )
    .expect("open");
    let results = diagnostics(&host.snapshot(), &id);
    assert!(
        !results
            .iter()
            .any(|item| item.code == DiagnosticCode::Cardinality),
        "unexpected diagnostics: {results:?}"
    );
}

#[test]
fn eu4_dynamic_culture_definition_is_used_by_semantic_type_matcher() {
    use engine::{SourceRoot, SourceRootId, SourceRootKind, WorkspaceChange};
    use std::fs;

    let root = std::env::temp_dir().join(format!("ide-cwt-dynamic-{}", std::process::id()));
    fs::create_dir_all(root.join("common/cultures")).expect("culture directory");
    fs::write(
        root.join("common/cultures/00_test.txt"),
        "latin = { french = { } }\n",
    )
    .expect("culture definition");
    let rules = game::eu4::runtime_rules().expect("load first-party rules");
    let mut host = eu4_host(rules);
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot {
        id: SourceRootId::new(1),
        kind: SourceRootKind::Project,
        path: AbsPath::normalize(&root),
        order: 0,
        writable: true,
    }]));
    host.refresh_source_roots()
        .expect("scan culture definition");
    let id = DocumentId::new("file:///tmp/events/culture.txt");
    host.open_document(
        id.clone(),
        1,
        "country_event = { trigger = { culture = french } }\n".to_owned(),
        None,
    )
    .expect("open");
    let results = diagnostics(&host.snapshot(), &id);
    assert!(
        results
            .iter()
            .all(|item| item.code != DiagnosticCode::InvalidValue)
    );
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn eu4_country_tag_definition_feeds_dynamic_enum_matcher() {
    use engine::{SourceRoot, SourceRootId, SourceRootKind, WorkspaceChange};
    use std::fs;

    let root = std::env::temp_dir().join(format!("ide-cwt-tags-{}", std::process::id()));
    fs::create_dir_all(root.join("common/country_tags")).expect("country tag directory");
    fs::write(
        root.join("common/country_tags/00_test.txt"),
        "FRA = \"countries/France.txt\"\n",
    )
    .expect("country tag definition");
    let rules = game::eu4::runtime_rules().expect("load first-party rules");
    let mut host = eu4_host(rules);
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot {
        id: SourceRootId::new(1),
        kind: SourceRootKind::Project,
        path: AbsPath::normalize(&root),
        order: 0,
        writable: true,
    }]));
    host.refresh_source_roots()
        .expect("scan country tag definition");
    let id = DocumentId::new("file:///tmp/events/tag.txt");
    host.open_document(
        id.clone(),
        1,
        "country_event = { immediate = { change_tag = FRA } }\n".to_owned(),
        None,
    )
    .expect("open");
    assert!(
        diagnostics(&host.snapshot(), &id)
            .iter()
            .all(|item| item.code != DiagnosticCode::InvalidValue)
    );
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn eu4_flag_definition_feeds_dynamic_value_matcher() {
    use engine::{SourceRoot, SourceRootId, SourceRootKind, WorkspaceChange};
    use std::fs;

    let root = std::env::temp_dir().join(format!("ide-cwt-flags-{}", std::process::id()));
    fs::create_dir_all(root.join("events")).expect("event directory");
    fs::write(
        root.join("events/00_flags.txt"),
        "country_event = { immediate = { set_country_flag = known_flag } }\n",
    )
    .expect("flag definition");
    let rules = game::eu4::runtime_rules().expect("load first-party rules");
    let mut host = eu4_host(rules);
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot {
        id: SourceRootId::new(1),
        kind: SourceRootKind::Project,
        path: AbsPath::normalize(&root),
        order: 0,
        writable: true,
    }]));
    host.refresh_source_roots().expect("scan flag definition");
    let id = DocumentId::new("file:///tmp/events/flag-use.txt");
    host.open_document(
        id.clone(),
        1,
        "country_event = { immediate = { clr_country_flag = known_flag } }\n".to_owned(),
        None,
    )
    .expect("open");
    assert!(
        diagnostics(&host.snapshot(), &id)
            .iter()
            .all(|item| item.code != DiagnosticCode::InvalidValue)
    );
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn eu4_scripted_effect_params_are_owner_qualified() {
    use engine::{SourceRoot, SourceRootId, SourceRootKind, WorkspaceChange};
    use std::fs;

    let root = std::env::temp_dir().join(format!("ide-cwt-params-{}", std::process::id()));
    fs::create_dir_all(root.join("common/scripted_effects")).expect("scripted effect directory");
    let definition_path = root.join("common/scripted_effects/00_test.txt");
    fs::write(
        &definition_path,
        concat!(
            "apply = { add_prestige = $amount$ [[optional] add_stability = 1 ] }\n",
            "other_effect = { add_prestige = $other$ }\n",
        ),
    )
    .expect("scripted effect definition");
    let rules = game::eu4::runtime_rules().expect("load first-party rules");
    let mut host = eu4_host(rules);
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot {
        id: SourceRootId::new(1),
        kind: SourceRootKind::Project,
        path: AbsPath::normalize(&root),
        order: 0,
        writable: true,
    }]));
    host.refresh_source_roots()
        .expect("scan scripted effect definition");
    let id = DocumentId::new("file:///tmp/events/params.txt");
    let invocation = "country_event = { immediate = { apply = { amount = 1 optional = yes } } }\n";
    host.open_document(id.clone(), 1, invocation.to_owned(), None)
        .expect("open");
    let snapshot = host.snapshot();
    assert_eq!(
        snapshot
            .index()
            .definitions("scripted_effect", "apply")
            .len(),
        1
    );
    assert_eq!(
        crate::semantic::dynamic_definition_summary(&snapshot, "scripted_effect", "apply")
            .expect("resolved owner parameters")
            .parameters
            .iter()
            .map(|p| p.name.as_str())
            .collect::<Vec<_>>(),
        ["amount", "optional"]
    );
    assert!(diagnostics(&snapshot, &id).iter().all(|item| !matches!(
        item.code,
        DiagnosticCode::InvalidValue | DiagnosticCode::UnknownKey
    )));
    let completion_position =
        u32::try_from(invocation.find("apply = { ").expect("invocation") + "apply = { ".len() - 1)
            .expect("position");
    let labels = complete(&snapshot, &id, completion_position)
        .items
        .into_iter()
        .map(|item| item.label)
        .collect::<Vec<_>>();
    assert!(
        labels
            .iter()
            .any(|label| label.eq_ignore_ascii_case("amount")),
        "{labels:?}"
    );
    assert!(
        labels
            .iter()
            .any(|label| label.eq_ignore_ascii_case("optional")),
        "{labels:?}"
    );
    assert!(
        !labels
            .iter()
            .any(|label| label.eq_ignore_ascii_case("other"))
    );

    let wrong_id = DocumentId::new("file:///tmp/events/wrong-params.txt");
    host.open_document(
        wrong_id.clone(),
        1,
        "country_event = { immediate = { apply = { other = 1 } } }\n".to_owned(),
        None,
    )
    .expect("open wrong invocation");
    assert!(diagnostics(&host.snapshot(), &wrong_id).iter().any(|item| {
        item.code == DiagnosticCode::UnknownKey && item.message.contains("`other`")
    }));

    let overlay_id = DocumentId::new("file:///tmp/common/scripted_effects/00_test.txt");
    host.open_document(
        overlay_id,
        1,
        "apply = { add_prestige = $overlay_only$ }\n".to_owned(),
        Some(AbsPath::normalize(&definition_path)),
    )
    .expect("open scripted effect overlay");
    let overlay_call = DocumentId::new("file:///tmp/events/overlay-params.txt");
    host.open_document(
        overlay_call.clone(),
        1,
        "country_event = { immediate = { apply = { overlay_only = 1 amount = 2 } } }\n".to_owned(),
        None,
    )
    .expect("open overlay invocation");
    let overlay_results = diagnostics(&host.snapshot(), &overlay_call);
    assert!(!overlay_results.iter().any(|item| {
        item.code == DiagnosticCode::UnknownKey && item.message.contains("`overlay_only`")
    }));
    assert!(overlay_results.iter().any(|item| {
        item.code == DiagnosticCode::UnknownKey && item.message.contains("`amount`")
    }));
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn unresolved_dynamic_signature_keeps_parameter_blocks_open_world() {
    use engine::{SourceRoot, SourceRootId, SourceRootKind, WorkspaceChange};
    use std::fs;

    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("ide-open-dynamic-params-{nonce}"));
    let definitions = root.join("common/scripted_effects");
    fs::create_dir_all(&definitions).expect("scripted effect directory");
    fs::write(
        definitions.join("00_first.txt"),
        "ambiguous_effect = { value = $first$ }\n",
    )
    .expect("first definition");
    fs::write(
        definitions.join("01_second.txt"),
        "ambiguous_effect = { value = $second$ }\n",
    )
    .expect("second definition");

    let mut host = eu4_host(game::eu4::runtime_rules().expect("first-party rules"));
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(1),
        SourceRootKind::Project,
        AbsPath::normalize(&root),
    )]));
    host.refresh_source_roots()
        .expect("scan ambiguous definitions");
    let id = DocumentId::new("file:///tmp/events/open-dynamic-params.txt");
    let invocation =
        "country_event = { immediate = { ambiguous_effect = { foreign_parameter = 1 } } }\n";
    host.open_document(id.clone(), 1, invocation.to_owned(), None)
        .expect("open invocation");
    let snapshot = host.snapshot();
    assert!(
        crate::semantic::dynamic_definition_summary(
            &snapshot,
            "scripted_effect",
            "ambiguous_effect"
        )
        .is_none()
    );
    let results = diagnostics(&snapshot, &id);
    assert!(!results.iter().any(|item| {
        item.code == DiagnosticCode::UnknownKey && item.message.contains("`foreign_parameter`")
    }));
    assert!(!results.iter().any(|item| {
        item.code == DiagnosticCode::Cardinality
            && item.message.contains("missing required parameter")
    }));

    let completion_position = u32::try_from(
        invocation
            .find("ambiguous_effect = { ")
            .expect("parameter block")
            + "ambiguous_effect = { ".len()
            - 1,
    )
    .expect("completion position");
    let labels = complete(&snapshot, &id, completion_position)
        .items
        .into_iter()
        .map(|item| item.label)
        .collect::<Vec<_>>();
    assert!(
        !labels
            .iter()
            .any(|label| label.eq_ignore_ascii_case("scaled_skill")),
        "an unresolved owner must not fall back to the static parameter enum: {labels:?}"
    );

    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn eu4_legacy_governments_use_eu4_reform_semantics() {
    use engine::{SourceRoot, SourceRootId, SourceRootKind, WorkspaceChange};
    use std::fs;

    let root = std::env::temp_dir().join(format!("ide-cwt-legacy-{}", std::process::id()));
    fs::create_dir_all(root.join("common/government_reforms")).expect("reform directory");
    fs::write(
        root.join("common/government_reforms/00_test.txt"),
        "reform_a = { legacy_government = yes }\n",
    )
    .expect("legacy reform definition");
    let rules = game::eu4::runtime_rules().expect("load first-party rules");
    let mut host = eu4_host(rules);
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot {
        id: SourceRootId::new(1),
        kind: SourceRootKind::Project,
        path: AbsPath::normalize(&root),
        order: 0,
        writable: true,
    }]));
    host.refresh_source_roots()
        .expect("scan legacy reform definition");
    let id = DocumentId::new("file:///tmp/events/legacy.txt");
    host.open_document(
        id.clone(),
        1,
        "country_event = { immediate = { set_legacy_government = reform_a } }\n".to_owned(),
        None,
    )
    .expect("open");
    assert!(
        diagnostics(&host.snapshot(), &id)
            .iter()
            .all(|item| item.code != DiagnosticCode::InvalidValue)
    );
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn membership_caches_do_not_leak_across_hosts_with_equal_revisions() {
    // 两个 host 各自的 revision 计数独立, 恰好在相同 revision 上查询时,
    // thread-local 快路径必须按 host 身份区分, 否则第二个 host 会读到
    // 第一个 host 的成员视图, 把自己的定义误报为不存在。
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let make_host = |suffix: &str, definitions: &str| {
        let root = std::env::temp_dir().join(format!("ide-host-leak-{suffix}-{nonce}"));
        let effects = root.join("common/scripted_effects");
        std::fs::create_dir_all(&effects).expect("definitions directory");
        std::fs::write(effects.join("00_definitions.txt"), definitions).expect("definitions");
        let mut host = eu4_host(game::eu4::runtime_rules().expect("first-party rules"));
        host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
            SourceRootId::new(1),
            SourceRootKind::Project,
            AbsPath::normalize(&root),
        )]));
        host.refresh_source_roots().expect("scan definitions");
        host
    };

    // Host A: 在某个 revision 上做一次完整诊断, 填满成员快路径。
    let mut host_a = make_host("a", "stable = { add_stability = $AMT$ }\n");
    let id_a = DocumentId::new("file:///tmp/events/host-leak-a.txt");
    host_a
        .open_document(
            id_a.clone(),
            1,
            "country_event = { id = fixture.1 immediate = { stable = { AMT = 1 } } }\n".to_owned(),
            None,
        )
        .expect("open call a");
    let diagnostics_a = diagnostics(&host_a.snapshot(), &id_a);
    assert!(
        diagnostics_a
            .iter()
            .all(|diagnostic| diagnostic.code != DiagnosticCode::UnknownKey),
        "host_a baseline must be clean: {diagnostics_a:?}"
    );

    // Host B: 新 host, 其调用恰好落在与 host_a 相同的 revision 上。
    let mut host_b = make_host(
        "b",
        "guarded = { add_stability = $AMT$ [[EXTRA] add_manpower = $EXTRA$ ] }\n",
    );
    let id_b = DocumentId::new("file:///tmp/events/host-leak-b.txt");
    host_b
        .open_document(
            id_b.clone(),
            1,
            "country_event = { id = fixture.1 immediate = { guarded = { AMT = 1 EXTRA = 2 } } }\n"
                .to_owned(),
            None,
        )
        .expect("open call b");
    let snapshot_b = host_b.snapshot();
    assert!(
        (snapshot_b.host_identity(), snapshot_b.revision())
            != (
                host_a.snapshot().host_identity(),
                host_a.snapshot().revision()
            )
            || snapshot_b.host_identity() != host_a.snapshot().host_identity(),
        "the fixture must keep the two hosts distinguishable"
    );
    let diagnostics_b = diagnostics(&snapshot_b, &id_b);
    let unknown = diagnostics_b
        .iter()
        .filter(|diagnostic| diagnostic.code == DiagnosticCode::UnknownKey)
        .collect::<Vec<_>>();
    assert!(
        unknown.is_empty(),
        "host_b's own definitions must resolve even when another host \
         reached the same revision first: {unknown:?}"
    );
}
