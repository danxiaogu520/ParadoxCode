use super::support::*;
use crate::dynamic_contracts::ScopeContract;
use crate::dynamic_rules::{
    DynamicAffixSegment, DynamicBodyFindingKind, DynamicScopeStep, DynamicScopeTransition,
    DynamicSiteGate, DynamicSiteZone, dynamic_rule_row,
};
use rules::ValueMatcher;

fn province_push() -> DynamicScopeTransition {
    DynamicScopeTransition {
        push: Some("province".to_owned()),
        replaces: Vec::new(),
    }
}

fn gate(scopes: &[&str], group: u64) -> DynamicScopeStep {
    DynamicScopeStep::Gate(DynamicSiteGate {
        allowed_scopes: scopes.iter().map(|scope| (*scope).to_owned()).collect(),
        group: Some(group),
    })
}

fn lit(literal: &str) -> DynamicAffixSegment {
    DynamicAffixSegment::Literal(literal.to_owned())
}

/// Opens one scripted-effects file as a current-mod workspace and returns the
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
    let mut host = eu4_host(game::eu4::first_party_rules().expect("first-party rules"));
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
    let row = dynamic_rule_row(&snapshot, "scripted_effect", "take_core").expect("row");

    let prov = row
        .parameters
        .iter()
        .find(|parameter| parameter.name == "PROV")
        .expect("PROV");
    assert_eq!(prov.value_sites.len(), 1);
    let site = &prov.value_sites[0];
    assert_eq!(site.zone, DynamicSiteZone::Normal);
    assert_eq!(site.context, "effect");
    assert!(site.parent_path.is_empty());
    assert_eq!(
        site.chain,
        vec![
            gate(&["country"], 0),
            DynamicScopeStep::Transition(province_push())
        ]
    );
    assert_eq!(site.operator.as_deref(), Some("="));
    // Every alternative stays with its own allowed scopes; consumer policy
    // prunes, the row never does.
    assert_eq!(
        site.matchers,
        vec![
            ValueMatcher::Scope(Some("country".to_owned())),
            ValueMatcher::Enum("country_tags".to_owned()),
            ValueMatcher::Scope(Some("province".to_owned())),
            ValueMatcher::Type("province_id".to_owned()),
        ]
    );
    assert_eq!(
        site.matcher_scopes,
        vec![
            vec!["country".to_owned(), "province".to_owned()],
            vec!["country".to_owned(), "province".to_owned()],
            vec!["country".to_owned(), "province".to_owned()],
            vec!["country".to_owned(), "province".to_owned()],
        ]
    );
    assert!(site.fallback_groups.is_empty());
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
    let row = dynamic_rule_row(&snapshot, "scripted_effect", "sweep").expect("row");

    let who = row
        .parameters
        .iter()
        .find(|parameter| parameter.name == "WHO")
        .expect("WHO");
    assert!(who.sites.is_empty());
    assert_eq!(who.value_sites.len(), 2);
    let branch = who
        .value_sites
        .iter()
        .find(|site| site.fallback_groups.is_empty())
        .expect("branch row");
    assert_eq!(branch.zone, DynamicSiteZone::Structural);
    assert_eq!(branch.context, "trigger");
    assert!(branch.parent_path.is_empty());
    assert_eq!(
        branch.chain,
        vec![
            gate(&["country", "province"], 0),
            DynamicScopeStep::Transition(province_push()),
            gate(&[], 1),
        ]
    );
    // The scope-excluded twin reaches `limit` through its own fan-out group.
    let fallback = who
        .value_sites
        .iter()
        .find(|site| !site.fallback_groups.is_empty())
        .expect("fallback twin");
    assert_eq!(fallback.zone, DynamicSiteZone::Structural);
    assert_eq!(fallback.context, "trigger");
    assert_eq!(fallback.chain, vec![gate(&[], 2)]);
    assert_eq!(fallback.fallback_groups, vec![0]);

    let prov = row
        .parameters
        .iter()
        .find(|parameter| parameter.name == "PROV")
        .expect("PROV");
    assert_eq!(prov.value_sites.len(), 1);
    assert_eq!(prov.value_sites[0].zone, DynamicSiteZone::Normal);
    assert_eq!(prov.value_sites[0].context, "effect");
}

#[test]
fn site_rows_record_key_render_affixes() {
    let host = definitions_snapshot("dispatcher = { $CMD$ = yes set_$KIND$_policy = 1 }\n");
    let snapshot = host.snapshot();
    let row = dynamic_rule_row(&snapshot, "scripted_effect", "dispatcher").expect("row");

    assert!(row.dispatches_dynamically);
    let cmd = row
        .parameters
        .iter()
        .find(|parameter| parameter.name == "CMD")
        .expect("CMD");
    assert_eq!(cmd.key_render_sites.len(), 1);
    let site = &cmd.key_render_sites[0];
    assert!(site.prefix_segments.is_empty() && site.suffix_segments.is_empty());
    assert_eq!(site.zone, DynamicSiteZone::Normal);
    assert_eq!(site.context, "effect");
    assert!(site.parent_path.is_empty());
    assert!(site.chain.is_empty());
    assert!(site.guards.is_empty());

    let kind = row
        .parameters
        .iter()
        .find(|parameter| parameter.name == "KIND")
        .expect("KIND");
    assert_eq!(kind.key_render_sites.len(), 1);
    let site = &kind.key_render_sites[0];
    assert_eq!(
        (
            site.prefix_segments.as_slice(),
            site.suffix_segments.as_slice()
        ),
        ([lit("set_")].as_slice(), [lit("_policy")].as_slice())
    );
}

#[test]
fn site_rows_record_bare_payload_quoted_sites() {
    let host = definitions_snapshot("injector = { $PAYLOAD$ add_stability = 1 }\n");
    let snapshot = host.snapshot();
    let row = dynamic_rule_row(&snapshot, "scripted_effect", "injector").expect("row");

    let payload = row
        .parameters
        .iter()
        .find(|parameter| parameter.name == "PAYLOAD")
        .expect("PAYLOAD");
    assert!(payload.quoted_script);
    assert_eq!(payload.quoted_sites.len(), 1);
    let site = &payload.quoted_sites[0];
    assert_eq!(site.zone, DynamicSiteZone::Normal);
    assert_eq!(site.context, "effect");
    assert!(site.parent_path.is_empty());
    assert!(site.chain.is_empty());
}

#[test]
fn site_rows_stop_at_nested_dynamic_calls_with_forwarding_edges() {
    // The callee's own rows carry its parameter sites; the caller's parameter
    // records only the forwarding edge, never the callee's derived sites.
    let host = definitions_snapshot(
        "helper = { add_stability = $AMT$ }\nwrapper = { helper = { AMT = $A$ } }\n",
    );
    let snapshot = host.snapshot();
    let row = dynamic_rule_row(&snapshot, "scripted_effect", "wrapper").expect("row");

    let a = row
        .parameters
        .iter()
        .find(|parameter| parameter.name == "A")
        .expect("A");
    assert!(a.value_sites.is_empty());
    assert!(a.affixed_value_sites.is_empty());
    assert_eq!(
        a.forwarded_to,
        vec![crate::dynamic_rules::ForwardedParameter {
            kind: "scripted_effect".to_owned(),
            name: "helper".to_owned(),
            parameter: Some("AMT".to_owned()),
        }]
    );
    // The forward row carries the call statement's replay inputs so the
    // consumer can fold into the callee's entry scope.
    assert_eq!(a.forward_sites.len(), 1);
    let forward = &a.forward_sites[0];
    assert_eq!(
        (forward.kind.as_str(), forward.name.as_str()),
        ("scripted_effect", "helper")
    );
    assert_eq!(forward.parameter.as_deref(), Some("AMT"));
    assert_eq!(forward.context, "effect");
    assert!(forward.chain.is_empty());
    assert!(forward.guards.is_empty());
}

#[test]
fn site_rows_stamp_conditional_guards() {
    // Sites inside a `[[PARAM]]` branch record the guard so consumers prune
    // them when the invocation leaves the parameter unbound.
    let host = definitions_snapshot(
        "guarded = { add_stability = $AMT$ [[SCALE] add_manpower = $EXTRA$ ] }\n",
    );
    let snapshot = host.snapshot();
    let row = dynamic_rule_row(&snapshot, "scripted_effect", "guarded").expect("row");

    let amount = row
        .parameters
        .iter()
        .find(|parameter| parameter.name == "AMT")
        .expect("AMT");
    assert_eq!(amount.value_sites.len(), 1);
    assert!(amount.value_sites[0].guards.is_empty());

    let extra = row
        .parameters
        .iter()
        .find(|parameter| parameter.name == "EXTRA")
        .expect("EXTRA");
    assert_eq!(extra.value_sites.len(), 1);
    assert_eq!(
        extra.value_sites[0]
            .guards
            .iter()
            .map(|guard| (guard.name.as_str(), guard.negated))
            .collect::<Vec<_>>(),
        vec![("scale", false)]
    );
}
