use super::support::*;
use crate::dynamic_rules::{DynamicBodyFindingKind, dynamic_rule_row};
use crate::macro_contracts::ScopeContract;
use pdx_rules::ValueMatcher;

/// Opens one scripted-effects file as a current-mod workspace and returns the
/// snapshot to derive dynamic rule rows from.
fn definitions_snapshot(body: &str) -> pdx_engine::AnalysisHost {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-analysis-dynamic-rules-{nonce}"));
    let effects = root.join("common/scripted_effects");
    std::fs::create_dir_all(&effects).expect("scripted effects directory");
    std::fs::write(effects.join("00_definitions.txt"), body).expect("definitions");
    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(1),
        SourceRootKind::CurrentMod,
        root.clone(),
    )]));
    host.refresh_source_roots().expect("scan definitions");
    host
}

#[test]
fn dynamic_rows_derive_contract_signature_and_value_constraints() {
    let host =
        definitions_snapshot("scale_works = { add_stability = $AMT$ add_manpower = $MP$ }\n");
    let snapshot = host.snapshot();
    let row = dynamic_rule_row(&snapshot, "scripted_effect", "scale_works").expect("row");

    assert_eq!(row.context, "effect");
    assert!(!row.cyclic);
    assert!(!row.dispatches_dynamically);
    assert!(row.body_findings.is_empty());
    assert_eq!(
        row.contract,
        ScopeContract::Scopes(vec!["country".to_owned()])
    );

    assert_eq!(row.parameters.len(), 2);
    let amount = row
        .parameters
        .iter()
        .find(|parameter| parameter.name == "AMT")
        .expect("AMT");
    assert!(amount.required);
    assert!(!amount.quoted_script && !amount.used_in_key);
    assert_eq!(
        amount.sites,
        vec![vec![ValueMatcher::Int {
            min: None,
            max: None
        }]]
    );
    let manpower = row
        .parameters
        .iter()
        .find(|parameter| parameter.name == "MP")
        .expect("MP");
    assert!(manpower.required);
    assert_eq!(
        manpower.sites,
        vec![vec![ValueMatcher::Float {
            min: Some("-999".to_owned()),
            max: Some("999".to_owned())
        }]]
    );
}

#[test]
fn dynamic_rows_locate_push_container_scope_contradictions() {
    // `capital` enters in country scope and pushes province; `add_prestige`
    // only runs in country scope, so the nested statement can never execute
    // and the definition itself must be rejected with a located finding.
    let host = definitions_snapshot("bad_push = { capital = { add_prestige = 1 } }\n");
    let snapshot = host.snapshot();
    let row = dynamic_rule_row(&snapshot, "scripted_effect", "bad_push").expect("row");

    assert_eq!(
        row.contract,
        ScopeContract::Scopes(vec!["country".to_owned()])
    );
    let finding = row
        .body_findings
        .iter()
        .find(|finding| finding.statement == "add_prestige")
        .expect("located contradiction");
    assert_eq!(finding.kind, DynamicBodyFindingKind::ScopeContradiction);
    assert_eq!(finding.reachable_scopes, vec!["province".to_owned()]);
    assert_eq!(finding.required_scopes, vec!["country".to_owned()]);
}

#[test]
fn dynamic_rows_descend_opaque_entries_into_scope_switches() {
    // A dynamic scope link (`ROOT`) leaves the entry unknown, yet the
    // scope-switching container inside it still re-targets its children, so
    // the contradiction stays findable while the contract stays open.
    let host =
        definitions_snapshot("opaque_entry = { ROOT = { capital = { add_prestige = 1 } } }\n");
    let snapshot = host.snapshot();
    let row = dynamic_rule_row(&snapshot, "scripted_effect", "opaque_entry").expect("row");

    assert_eq!(row.contract, ScopeContract::Unconstrained);
    let finding = row
        .body_findings
        .iter()
        .find(|finding| finding.statement == "add_prestige")
        .expect("contradiction behind the opaque link");
    assert_eq!(finding.reachable_scopes, vec!["province".to_owned()]);
}

#[test]
fn dynamic_rows_flag_param_key_dispatch() {
    let host = definitions_snapshot("dispatcher = { $action$ = yes }\n");
    let snapshot = host.snapshot();
    let row = dynamic_rule_row(&snapshot, "scripted_effect", "dispatcher").expect("row");

    assert!(row.dispatches_dynamically);
    assert_eq!(row.contract, ScopeContract::Unconstrained);
    let action = row
        .parameters
        .iter()
        .find(|parameter| parameter.name == "action")
        .expect("action parameter");
    assert!(action.used_in_key);
    assert!(action.sites.is_empty());
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
    let row = dynamic_rule_row(&snapshot, "scripted_effect", "nested_bad").expect("row");
    let helper = dynamic_rule_row(&snapshot, "scripted_effect", "province_helper").expect("helper");

    assert_eq!(
        helper.contract,
        ScopeContract::Scopes(vec!["province".to_owned()])
    );
    let finding = row
        .body_findings
        .iter()
        .find(|finding| finding.statement == "province_helper")
        .expect("nested call mismatch");
    assert_eq!(finding.kind, DynamicBodyFindingKind::NestedCallMismatch);
    assert_eq!(finding.reachable_scopes, vec!["country".to_owned()]);
    assert_eq!(finding.required_scopes, vec!["province".to_owned()]);
}

#[test]
fn dynamic_rows_keep_conditional_parameters_optional() {
    let host = definitions_snapshot(
        "guarded = { add_stability = $AMT$ [[EXTRA] add_manpower = $EXTRA$ ] }\n",
    );
    let snapshot = host.snapshot();
    let row = dynamic_rule_row(&snapshot, "scripted_effect", "guarded").expect("row");

    let amount = row
        .parameters
        .iter()
        .find(|parameter| parameter.name == "AMT")
        .expect("AMT");
    assert!(amount.required);
    let extra = row
        .parameters
        .iter()
        .find(|parameter| parameter.name == "EXTRA")
        .expect("EXTRA");
    assert!(!extra.required);
    // The guarded branch is still walked for constraint inference.
    assert_eq!(extra.sites.len(), 1);
}

#[test]
fn dynamic_rows_mark_cycle_participants() {
    let host = definitions_snapshot("loop_a = { loop_b = yes }\nloop_b = { loop_a = yes }\n");
    let snapshot = host.snapshot();
    for name in ["loop_a", "loop_b"] {
        let row = dynamic_rule_row(&snapshot, "scripted_effect", name).expect("row");
        assert!(row.cyclic, "{name} must be marked cyclic");
    }
}
