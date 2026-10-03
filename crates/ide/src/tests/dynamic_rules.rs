use super::support::*;
use crate::dynamic_contracts::ScopeContract;
use text::AbsPath;

/// Opens one scripted-effects file as a project workspace and returns the
/// snapshot to derive dynamic rule rows from.
fn definitions_snapshot(body: &str) -> engine::AnalysisHost {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("ide-dynamic-rules-{nonce}"));
    let effects = root.join("common/scripted_effects");
    std::fs::create_dir_all(&effects).expect("scripted effects directory");
    std::fs::write(effects.join("00_definitions.txt"), body).expect("definitions");
    let mut host = eu4_host(game::eu4::runtime_rules().expect("first-party rules"));
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(1),
        SourceRootKind::Project,
        AbsPath::normalize(&root),
    )]));
    host.refresh_source_roots().expect("scan definitions");
    host
}

fn ir_sites(
    snapshot: &AnalysisSnapshot,
    owner: &str,
    parameter: &str,
) -> hir::analysis::Analysis<Vec<crate::ir_template::ParameterSite>> {
    crate::ir_template::definition_parameter_sites(
        snapshot,
        "scripted_effect",
        owner,
        parameter,
        &CancellationToken::new(),
    )
    .unwrap()
}

fn ir_findings(snapshot: &AnalysisSnapshot) -> Vec<crate::Diagnostic> {
    snapshot
        .source_files()
        .keys()
        .flat_map(|id| {
            crate::source_file_diagnostics_with_cancellation(
                snapshot,
                *id,
                &CancellationToken::new(),
            )
            .unwrap()
        })
        .collect()
}

#[test]
fn dynamic_rows_derive_contract_signature_and_value_constraints() {
    let host =
        definitions_snapshot("scale_works = { add_stability = $AMT$ add_manpower = $MP$ }\n");
    let snapshot = host.snapshot();

    let summary =
        crate::semantic::dynamic_definition_summary(&snapshot, "scripted_effect", "scale_works")
            .unwrap();
    assert_eq!(summary.parameters.len(), 2);
    assert!(
        summary
            .parameters
            .iter()
            .all(|parameter| parameter.required)
    );
    assert_eq!(
        crate::dynamic_contracts::dynamic_contract(&snapshot, "scripted_effect", "scale_works"),
        Some(ScopeContract::Scopes(vec!["country".into()]))
    );
    for (parameter, valid, invalid) in [("AMT", "2", "1.5"), ("MP", "1.5", "1000")] {
        let sites = ir_sites(&snapshot, "scale_works", parameter);
        assert_eq!(sites.len(), 1);
        assert!(sites[0].accepts(&snapshot, parameter, valid));
        assert!(!sites[0].accepts(&snapshot, parameter, invalid));
        assert!(!sites[0].accepts(&snapshot, parameter, "nope"));
    }
    assert!(ir_findings(&snapshot).iter().all(|item| !matches!(
        item.code,
        DiagnosticCode::UnknownKey | DiagnosticCode::WrongScope
    )));
}

#[test]
fn dynamic_rows_locate_push_container_scope_contradictions() {
    // `capital` enters in country scope and pushes province; `add_prestige`
    // only runs in country scope, so the nested statement can never execute
    // and the definition itself must be rejected with a located finding.
    let host = definitions_snapshot("bad_push = { capital = { add_prestige = 1 } }\n");
    let snapshot = host.snapshot();

    assert_eq!(
        crate::dynamic_contracts::dynamic_contract(&snapshot, "scripted_effect", "bad_push"),
        Some(ScopeContract::Scopes(vec!["country".into()]))
    );
    let findings = ir_findings(&snapshot);
    assert!(
        findings
            .iter()
            .any(|item| item.code == DiagnosticCode::WrongScope
                && item.message.contains("add_prestige")
                && item.message.contains("province")
                && item.expected.as_deref() == Some("`country`")),
        "{findings:?}"
    );
}

#[test]
fn dynamic_rows_descend_opaque_entries_into_scope_switches() {
    // A dynamic scope link (`ROOT`) leaves the entry unknown, yet the
    // scope-switching container inside it still re-targets its children, so
    // the contradiction stays findable while the contract stays open.
    let host =
        definitions_snapshot("opaque_entry = { ROOT = { capital = { add_prestige = 1 } } }\n");
    let snapshot = host.snapshot();

    assert_eq!(
        crate::dynamic_contracts::dynamic_contract(&snapshot, "scripted_effect", "opaque_entry"),
        Some(ScopeContract::Unconstrained)
    );
    let findings = ir_findings(&snapshot);
    assert!(
        findings
            .iter()
            .any(|item| item.code == DiagnosticCode::WrongScope
                && item.message.contains("add_prestige")
                && item.message.contains("province")),
        "{findings:?}"
    );
}

#[test]
fn dynamic_rows_flag_param_key_dispatch() {
    let host = definitions_snapshot("dispatcher = { $action$ = yes }\n");
    let snapshot = host.snapshot();
    let sites = ir_sites(&snapshot, "dispatcher", "action");
    assert_eq!(sites.len(), 1);
    assert!(matches!(
        sites[0].domain,
        crate::ir_template::Domain::Key { .. }
    ));
    assert_eq!(
        crate::dynamic_contracts::dynamic_contract(&snapshot, "scripted_effect", "dispatcher"),
        Some(ScopeContract::Unconstrained)
    );
    assert!(sites[0].accepts(&snapshot, "action", "add_prestige"));
    assert!(!sites[0].accepts(&snapshot, "action", "missing_command"));
}

#[test]
fn dynamic_rows_locate_nested_call_contract_mismatches() {
    // `controller` enters in province scope and pushes country; the callee
    // requires province, so the nested call can never run.
    let host = definitions_snapshot(
        "province_helper = { change_province_name = \"Y\" }\n\
         nested_bad = { controller = { province_helper = yes } }\n",
    );
    let snapshot = host.snapshot();

    assert_eq!(
        crate::dynamic_contracts::dynamic_contract(&snapshot, "scripted_effect", "province_helper"),
        Some(ScopeContract::Scopes(vec!["province".into()]))
    );
    let findings = ir_findings(&snapshot);
    assert!(
        findings
            .iter()
            .any(|item| item.code == DiagnosticCode::WrongScope
                && item.message.contains("province_helper")
                && item.message.contains("country")),
        "{findings:?}"
    );
}

#[test]
fn dynamic_rows_keep_conditional_parameters_optional() {
    let host = definitions_snapshot(
        "guarded = { add_stability = $AMT$ [[EXTRA] add_manpower = $EXTRA$ ] }\n",
    );
    let snapshot = host.snapshot();

    let summary =
        crate::semantic::dynamic_definition_summary(&snapshot, "scripted_effect", "guarded")
            .unwrap();
    assert!(
        summary
            .parameters
            .iter()
            .find(|parameter| parameter.name == "AMT")
            .unwrap()
            .required
    );
    assert!(
        !summary
            .parameters
            .iter()
            .find(|parameter| parameter.name == "EXTRA")
            .unwrap()
            .required
    );
    assert_eq!(ir_sites(&snapshot, "guarded", "EXTRA").len(), 1);
}

#[test]
fn dynamic_rows_mark_cycle_participants() {
    let host = definitions_snapshot("loop_a = { loop_b = yes }\nloop_b = { loop_a = yes }\n");
    let snapshot = host.snapshot();
    let report =
        crate::dynamic_cycles::dynamic_cycle_report(&snapshot, &CancellationToken::new()).unwrap();
    for name in ["loop_a", "loop_b"] {
        assert!(report.message("scripted_effect", name).is_some(), "{name}");
    }
}

#[test]
fn site_rows_record_value_site_position_transitions_and_scopes() {
    // `capital` pushes province and switches to a fresh effect namespace, so
    // the row's chain carries the fan-out's gate before the push and the
    // site sits at the child context with an empty parent path. The fan-out
    // is gated, so a fallback twin is walked too, but the structural path
    // `capital.add_core` resolves no leaf rows and the twin stays empty —
    // exactly the sites the symbolic walker's structural descent saw.
    let host = definitions_snapshot("take_core = { capital = { add_core = $PROV$ } }\n");
    let snapshot = host.snapshot();

    let sites = ir_sites(&snapshot, "take_core", "PROV");
    assert_eq!(sites.len(), 1);
    assert_eq!(
        sites[0].state.current.first(),
        Some(&hir::ScopeValue::known_single("province"))
    );
    let crate::ir_template::Domain::Value(matchers) = &sites[0].domain else {
        panic!("value domain")
    };
    assert!(matchers.iter().any(|matcher| matches!(
        snapshot.ir().matcher(*matcher),
        rules::ir::Matcher::Union(_) | rules::ir::Matcher::Scope(_) | rules::ir::Matcher::Ref(_)
    )));
    assert!(!sites[0].accepts(&snapshot, "PROV", "not_a_country"));
}

#[test]
fn site_rows_mark_structural_sub_blocks_and_keep_them_out_of_legacy_sites() {
    // `limit` validates in trigger context against the pre-push scope: the
    // legacy walk records no scalar sites there, while the site rows record
    // them with the Structural zone so the staged arbitration can move them
    // into validation without re-derivation.
    let host = definitions_snapshot(
        "sweep = { every_owned_province = { limit = { owned_by = $WHO$ } add_core = $PROV$ } }\n",
    );
    let snapshot = host.snapshot();

    for parameter in ["WHO", "PROV"] {
        let sites = ir_sites(&snapshot, "sweep", parameter);
        assert_eq!(sites.len(), 1, "{parameter}: {sites:?}");
        assert_eq!(
            sites[0].state.current.first(),
            Some(&hir::ScopeValue::known_single("province"))
        );
        assert!(matches!(
            sites[0].domain,
            crate::ir_template::Domain::Value(_)
        ));
        assert!(!sites[0].accepts(&snapshot, parameter, "missing_country"));
    }
}

#[test]
fn site_rows_record_key_render_affixes() {
    let host = definitions_snapshot("dispatcher = { $CMD$ = yes set_$KIND$_policy = 1 }\n");
    let snapshot = host.snapshot();
    let sites = ir_sites(&snapshot, "dispatcher", "CMD");
    assert_eq!(sites.len(), 1);
    assert!(matches!(
        sites[0].domain,
        crate::ir_template::Domain::Key { .. }
    ));
    assert_eq!(
        sites[0].rendered_value("CMD", "add_prestige").as_deref(),
        Some("add_prestige")
    );
    let sites = ir_sites(&snapshot, "dispatcher", "KIND");
    assert_eq!(sites.len(), 1);
    assert_eq!(
        sites[0].rendered_value("KIND", "trade").as_deref(),
        Some("set_trade_policy")
    );
}

#[test]
fn site_rows_record_bare_payload_quoted_sites() {
    let host = definitions_snapshot("injector = { $PAYLOAD$ add_stability = 1 }\n");
    let snapshot = host.snapshot();
    let sites = ir_sites(&snapshot, "injector", "PAYLOAD");
    assert_eq!(sites.len(), 1);
    assert!(matches!(
        sites[0].domain,
        crate::ir_template::Domain::Payload { .. }
    ));
}

#[test]
fn site_rows_stop_at_nested_dynamic_calls_with_forwarding_edges() {
    // The callee's own rows carry its parameter sites; the caller's parameter
    // records only the forwarding edge, never the callee's derived sites.
    let host = definitions_snapshot(
        "helper = { add_stability = $AMT$ }\nwrapper = { helper = { AMT = $A$ } }\n",
    );
    let snapshot = host.snapshot();

    let summary =
        crate::semantic::dynamic_definition_summary(&snapshot, "scripted_effect", "wrapper")
            .unwrap();
    let template = summary.template.as_ref().unwrap();
    let hir::TemplateItem::Property(call) = &template.items[0] else {
        panic!("call")
    };
    assert_eq!(
        call.key.fragments,
        vec![hir::TemplateFragment::Literal("helper".into())]
    );
    let sites = ir_sites(&snapshot, "wrapper", "A");
    assert_eq!(sites.len(), 1);
    assert!(sites[0].accepts(&snapshot, "A", "1"));
    assert!(!sites[0].accepts(&snapshot, "A", "nope"));
}

#[test]
fn site_rows_stamp_conditional_guards() {
    // Sites inside a `[[PARAM]]` branch record the guard so consumers prune
    // them when the invocation leaves the parameter unbound.
    let host = definitions_snapshot(
        "guarded = { add_stability = $AMT$ [[SCALE] add_manpower = $EXTRA$ ] }\n",
    );
    let snapshot = host.snapshot();

    let summary =
        crate::semantic::dynamic_definition_summary(&snapshot, "scripted_effect", "guarded")
            .unwrap();
    let template = summary.template.as_ref().unwrap();
    let hir::TemplateItem::Conditional(conditional) = &template.items[1] else {
        panic!("guard")
    };
    assert!(conditional.name.eq_ignore_ascii_case("SCALE"));
    assert!(!conditional.negated);
    let state = hir::ScopeState {
        root: hir::ScopeValue::known_single("country"),
        current: vec![hir::ScopeValue::known_single("country")],
        from: vec![],
        previous: vec![],
    };
    let absent = crate::ir_template::parameter_sites(
        &snapshot,
        "scripted_effect",
        "guarded",
        "EXTRA",
        &Default::default(),
        state.clone(),
        &CancellationToken::new(),
    )
    .unwrap();
    assert!(absent.is_empty());
    let bound = std::collections::BTreeMap::from([("SCALE".into(), "yes".into())]);
    let present = crate::ir_template::parameter_sites(
        &snapshot,
        "scripted_effect",
        "guarded",
        "EXTRA",
        &bound,
        state,
        &CancellationToken::new(),
    )
    .unwrap();
    assert_eq!(present.len(), 1);
    assert!(present[0].accepts(&snapshot, "EXTRA", "0.5"));
    assert!(!present[0].accepts(&snapshot, "EXTRA", "1000"));
}
