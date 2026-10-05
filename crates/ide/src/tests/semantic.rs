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
