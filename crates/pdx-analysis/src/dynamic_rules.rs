//! First-class dynamic rule rows derived from workspace scripted definitions.
//!
//! Phase 1 of the dynamic-rules refactor: every scripted effect/trigger
//! definition is derived once per revision into a [`DynamicRuleRow`] that
//! mirrors what a static [`pdx_rules::SemanticRule`] provides for a command:
//! a semantic context, an entry-scope contract, a parameter signature with
//! inferred value constraints, and definition-site body findings. Call sites
//! will validate against these rows through the ordinary rule-matching path,
//! which replaces the runtime expansion machinery.
//!
//! The derivation walks the lowered definition template with a *scope set*
//! instead of a single scope. The entry contract may admit several scopes,
//! and scope-switching containers re-target their children, so every
//! statement is checked against the set of scopes that can reach it; a
//! statement that no reachable scope satisfies is recorded as a
//! [`DynamicBodyFinding`] anchored at its definition-side key range. Scope
//! contradictions are thereby rejected at the definition site with a concrete
//! location, not as an anonymous empty contract at every call site.
//!
//! Regions the definition cannot know statically stay conservative. A
//! `$param$` in key position dispatches dynamically, so the statement is
//! opaque for scope purposes while its subtree still collects parameter
//! constraints under an unknown scope; dynamic scope links (`ROOT`, `PREV`,
//! `FROM`, event targets) re-target scopes the caller decides at runtime and
//! are descended under an unknown scope for the same reason. Phase 1 is a
//! shadow layer: findings are derived and cached but not yet published as
//! diagnostics.

// Call-site validation (P2) consumes rows, parameters, and findings data;
// body findings publication, hover/completion, and the report accessors
// arrive in P3 and read the remaining fields.
#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use pdx_engine::hir::{
    TemplateFragment, TemplateItem, TemplateProperty, TemplateToken, TemplateValue,
};
use pdx_engine::{AnalysisSnapshot, CacheDomain, DocumentSource, DynamicParameterSignature};
use pdx_rules::{GameProfile, RuleShape, ValueMatcher};
use pdx_text::TextRange;

use crate::dynamic_contracts::{ScopeContract, dynamic_contract};
use crate::dynamic_cycles::dynamic_cycle_report;
use crate::semantic::{
    dynamic_definition_type, probe_query_cache, resolve_dynamic_definition,
    semantic_rule_key_matches, semantic_rules_for_container_key,
};
use crate::types::{CancellationToken, Cancelled, uncancelled};

/// Cache key for the workspace-wide dynamic rule report inside the query cache.
const DYNAMIC_RULES_CACHE_KEY: &str = "dynamic-rule-rows";

/// One parameter of a dynamic rule, combining the indexed call signature with
/// the value constraints inferred from every usage site in the definition
/// body. `sites` holds one matcher set per usage site: the argument must
/// satisfy every site (each site's matchers are alternatives from
/// `alternative_id` rule rows). Parameters forwarded into a nested call carry
/// the edge so call-site validation can borrow the callee's constraints.
#[derive(Clone, Debug)]
pub(crate) struct DynamicParameterRow {
    pub(crate) name: String,
    /// Whether the indexed signature requires the caller to supply it.
    pub(crate) required: bool,
    /// One entry per usage site; each entry is the alternative matchers of
    /// the rule rows accepting the value at that site.
    pub(crate) sites: Vec<Vec<ValueMatcher>>,
    /// Value-position sites whose token embeds the parameter between literal
    /// affixes (`type = $RT$_rebels`): the argument is validated by splicing
    /// it between the affixes and matching the rendered value, so the call
    /// site constrains the bare argument without enumerating members.
    pub(crate) affixed_sites: Vec<AffixedValueSite>,
    /// The parameter is rendered inside a quoted script payload.
    pub(crate) quoted_script: bool,
    /// The parameter participates in a dynamic key dispatch.
    pub(crate) used_in_key: bool,
    /// `(callee kind, callee name, callee parameter)` edges for arguments
    /// forwarded as `callee = { PARAM = $THIS$ }`. `parameter` is `None`
    /// when the callee parameter name itself is rendered from a `$param$`
    /// key (`callee = { $WHICH$ = $THIS$ }`), so the binding may land on any
    /// of the callee's parameters.
    pub(crate) forwarded_to: Vec<ForwardedParameter>,
}

/// One forwarding edge from an owner parameter into a nested call.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ForwardedParameter {
    pub(crate) kind: String,
    pub(crate) name: String,
    pub(crate) parameter: Option<String>,
}

/// One value-position render site whose token embeds the parameter between
/// literal affixes (`type = $RT$_rebels`): the caller's bare argument is
/// validated by splicing it between the affixes and matching the rendered
/// value against the site's matchers, which keeps the check consistent with
/// how the same matcher accepts a directly written value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AffixedValueSite {
    pub(crate) prefix: String,
    pub(crate) suffix: String,
    pub(crate) matchers: Vec<ValueMatcher>,
}

/// Why a body statement can never run.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DynamicBodyFindingKind {
    /// No scope that reaches the statement is accepted by its rule rows.
    ScopeContradiction,
    /// A nested dynamic-rule call requires entry scopes disjoint from the
    /// scopes that reach the call.
    NestedCallMismatch,
    /// A literal statement key matches no rule row in its context. Data-only
    /// until the P3 arbitration calibrates the definition-site walk against
    /// vanilla; it is the replacement for expansion-time body checking of
    /// cache-only (closed) files.
    UnknownStatement,
}

/// One statement in a definition body that no reachable scope can execute.
#[derive(Clone, Debug)]
pub(crate) struct DynamicBodyFinding {
    pub(crate) kind: DynamicBodyFindingKind,
    /// Definition-side range of the offending statement key.
    pub(crate) key_range: TextRange,
    /// The offending statement key as written.
    pub(crate) statement: String,
    /// Scopes that can reach the statement.
    pub(crate) reachable_scopes: Vec<String>,
    /// Scopes the statement (or the callee) requires.
    pub(crate) required_scopes: Vec<String>,
}

/// The dynamic counterpart of one static command rule: derived once per
/// scripted definition, consumed by call-site validation, hover, and
/// completion through the same fields a static row provides.
#[derive(Clone, Debug)]
pub(crate) struct DynamicRuleRow {
    /// Dynamic symbol kind, such as `scripted_effect`.
    pub(crate) kind: String,
    pub(crate) name: String,
    /// Semantic context the body executes in (`effect` / `trigger`).
    pub(crate) context: String,
    /// Inferred entry-scope contract; the analogue of `allowed_scopes`.
    pub(crate) contract: ScopeContract,
    /// The definition participates in a definition cycle.
    pub(crate) cyclic: bool,
    /// Some statement key contains a `$param$`, so call sites dispatch
    /// dynamically and need rendered-key checks.
    pub(crate) dispatches_dynamically: bool,
    pub(crate) parameters: Vec<DynamicParameterRow>,
    /// Statements no reachable scope can execute, in body order.
    pub(crate) body_findings: Vec<DynamicBodyFinding>,
}

/// Workspace-wide derivation result for every live scripted definition.
#[derive(Clone, Debug, Default)]
pub(crate) struct DynamicRuleReport {
    rows: BTreeMap<(String, String), DynamicRuleRow>,
}

impl DynamicRuleReport {
    pub(crate) fn row(&self, kind: &str, name: &str) -> Option<&DynamicRuleRow> {
        self.rows
            .get(&(kind.to_ascii_lowercase(), name.to_ascii_lowercase()))
    }

    /// The row for a definition whose kind the caller does not know; names
    /// are unique across dynamic kinds in practice.
    pub(crate) fn row_by_name(&self, name: &str) -> Option<&DynamicRuleRow> {
        self.rows
            .values()
            .find(|row| row.name.eq_ignore_ascii_case(name))
    }

    pub(crate) fn len(&self) -> usize {
        self.rows.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}

/// Returns one derived row, computing the report when the per-revision cache
/// is cold. Shadow-phase entry point shared by tests and upcoming consumers.
pub(crate) fn dynamic_rule_row(
    snapshot: &AnalysisSnapshot,
    kind: &str,
    name: &str,
) -> Option<DynamicRuleRow> {
    let cancellation = CancellationToken::new();
    let report = uncancelled(dynamic_rule_report(snapshot, &cancellation));
    report.row(kind, name).cloned()
}

/// Snapshot-level lookup by definition name alone, for callers that only
/// hold the owner's spelling.
pub(crate) fn dynamic_rule_row_by_name(
    snapshot: &AnalysisSnapshot,
    name: &str,
) -> Option<DynamicRuleRow> {
    let cancellation = CancellationToken::new();
    let report = uncancelled(dynamic_rule_report(snapshot, &cancellation));
    report.row_by_name(name).cloned()
}

fn dynamic_rule_report(
    snapshot: &AnalysisSnapshot,
    cancellation: &CancellationToken,
) -> Result<Arc<DynamicRuleReport>, Cancelled> {
    let revision = snapshot.revision();
    if let Some(cached) =
        probe_query_cache::<DynamicRuleReport>(snapshot, revision, &[DYNAMIC_RULES_CACHE_KEY])
    {
        return Ok(cached);
    }
    cancellation.checkpoint()?;
    let report = build_dynamic_rule_report(snapshot, cancellation)?;
    if std::env::var("PDX_DEBUG_DYNAMIC_RULES").is_ok_and(|value| !value.is_empty()) {
        let findings: usize = report
            .rows
            .values()
            .map(|row| row.body_findings.len())
            .sum();
        eprintln!(
            "dynamic rules: {} rows, {} dynamic dispatch, {} cyclic, {} body findings",
            report.rows.len(),
            report
                .rows
                .values()
                .filter(|row| row.dispatches_dynamically)
                .count(),
            report.rows.values().filter(|row| row.cyclic).count(),
            findings,
        );
    }
    let report = Arc::new(report);
    snapshot.query_cache().insert(
        revision,
        CacheDomain::Documents,
        DYNAMIC_RULES_CACHE_KEY.to_owned(),
        report.clone(),
    );
    Ok(report)
}

fn build_dynamic_rule_report(
    snapshot: &AnalysisSnapshot,
    cancellation: &CancellationToken,
) -> Result<DynamicRuleReport, Cancelled> {
    let profile = snapshot.game_profile();
    let mut candidates: Vec<(String, String)> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for definition in snapshot.index().definitions_iter() {
        if !dynamic_definition_type(snapshot, &definition.kind) {
            continue;
        }
        if seen.insert((
            definition.kind.to_ascii_lowercase(),
            definition.name.to_ascii_lowercase(),
        )) {
            candidates.push((definition.kind.to_string(), definition.name.to_string()));
        }
    }
    for document in snapshot
        .documents()
        .values()
        .filter(|document| document.source() == DocumentSource::Overlay)
    {
        let Some(hir) = document.hir_handle() else {
            continue;
        };
        for definition in hir.definitions() {
            if !dynamic_definition_type(snapshot, &definition.kind) {
                continue;
            }
            if seen.insert((
                definition.kind.to_ascii_lowercase(),
                definition.name.to_ascii_lowercase(),
            )) {
                candidates.push((definition.kind.clone(), definition.name.clone()));
            }
        }
    }
    let mut rows = BTreeMap::new();
    // The definition-site cycle analysis is computed here so shadow rows carry
    // the flag even before any diagnostics pass warms the cache.
    let cycles = dynamic_cycle_report(snapshot, cancellation)?;
    for (kind, name) in &candidates {
        cancellation.checkpoint()?;
        let Some(resolved) = resolve_dynamic_definition(snapshot, kind, name) else {
            continue;
        };
        let key = (
            resolved.summary.kind.to_ascii_lowercase(),
            resolved.summary.name.to_ascii_lowercase(),
        );
        // The contract report is memoized workspace-wide and already resolves
        // nested callees; reuse it instead of re-inferring here. Empty
        // contracts are a definition error on their own and would turn every
        // body statement into a finding, so their bodies walk as unknown.
        let contract = dynamic_contract(snapshot, &resolved.summary.kind, &resolved.summary.name)
            .unwrap_or(ScopeContract::Unknown);
        let entry_flow = match &contract {
            ScopeContract::Scopes(scopes) => {
                ScopeFlow::Known(scopes.iter().map(|s| s.to_ascii_lowercase()).collect())
            }
            ScopeContract::Unconstrained | ScopeContract::Empty | ScopeContract::Unknown => {
                ScopeFlow::Unknown
            }
        };
        let mut derivation = Derivation::new(snapshot, profile);
        if let Some(template) = resolved.summary.template.as_ref() {
            derivation.walk_items(&template.items, &resolved.body_context, &[], entry_flow);
        }
        let parameters = merge_parameter_rows(&resolved.summary.parameters, &derivation);
        let row = DynamicRuleRow {
            kind: resolved.summary.kind.clone(),
            name: resolved.summary.name.clone(),
            context: resolved.body_context.clone(),
            contract,
            cyclic: cycles
                .message(&resolved.summary.kind, &resolved.summary.name)
                .is_some(),
            dispatches_dynamically: derivation.dynamic_dispatch,
            parameters,
            body_findings: derivation.findings,
        };
        rows.insert(key, row);
    }
    Ok(DynamicRuleReport { rows })
}

/// Merges the walk's parameter usage with the indexed call signature, keeping
/// the signature's first-use order and required flags.
fn merge_parameter_rows(
    signature: &[DynamicParameterSignature],
    derivation: &Derivation<'_>,
) -> Vec<DynamicParameterRow> {
    let mut rows = Vec::with_capacity(signature.len());
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for parameter in signature {
        seen.insert(parameter.name.to_ascii_lowercase());
        let usage = derivation
            .parameters
            .get(&parameter.name.to_ascii_lowercase());
        rows.push(DynamicParameterRow {
            name: parameter.name.clone(),
            required: parameter.required,
            sites: usage.map_or_else(Vec::new, |usage| usage.sites.clone()),
            affixed_sites: usage.map_or_else(Vec::new, |usage| usage.affixed_sites.clone()),
            quoted_script: usage.is_some_and(|usage| usage.quoted_script),
            used_in_key: usage.is_some_and(|usage| usage.used_in_key),
            forwarded_to: usage.map_or_else(Vec::new, |usage| usage.forwarded_to.clone()),
        });
    }
    // Usage the signature missed (guarded template-only forms) still rows as
    // optional so completion and call-site checks can see it.
    for (name, usage) in &derivation.parameters {
        if seen.insert(name.clone()) {
            rows.push(DynamicParameterRow {
                name: name.clone(),
                required: false,
                sites: usage.sites.clone(),
                affixed_sites: usage.affixed_sites.clone(),
                quoted_script: usage.quoted_script,
                used_in_key: usage.used_in_key,
                forwarded_to: usage.forwarded_to.clone(),
            });
        }
    }
    rows
}

/// The set of scopes a statement can execute in. `Unknown` means the
/// derivation cannot pin the scope (unconstrained entry, dynamic scope link,
/// or opaque dispatch): every scope check passes and scope-switching
/// containers still re-target their children.
#[derive(Clone, Debug)]
enum ScopeFlow {
    Known(BTreeSet<String>),
    Unknown,
}

impl ScopeFlow {
    fn scopes(&self) -> Option<&BTreeSet<String>> {
        match self {
            Self::Known(scopes) => Some(scopes),
            Self::Unknown => None,
        }
    }
}

#[derive(Default)]
struct ParameterUsage {
    sites: Vec<Vec<ValueMatcher>>,
    affixed_sites: Vec<AffixedValueSite>,
    quoted_script: bool,
    used_in_key: bool,
    forwarded_to: Vec<ForwardedParameter>,
}

/// One definition's derivation walk: collects parameter usage and records
/// statements no reachable scope can execute.
struct Derivation<'a> {
    snapshot: &'a AnalysisSnapshot,
    profile: &'a GameProfile,
    parameters: BTreeMap<String, ParameterUsage>,
    dynamic_dispatch: bool,
    findings: Vec<DynamicBodyFinding>,
}

impl<'a> Derivation<'a> {
    fn new(snapshot: &'a AnalysisSnapshot, profile: &'a GameProfile) -> Self {
        Self {
            snapshot,
            profile,
            parameters: BTreeMap::new(),
            dynamic_dispatch: false,
            findings: Vec::new(),
        }
    }

    fn walk_items(
        &mut self,
        items: &[TemplateItem],
        context: &str,
        path: &[Arc<str>],
        flow: ScopeFlow,
    ) {
        for item in items {
            match item {
                TemplateItem::Property(property) => {
                    self.walk_property(property, context, path, flow.clone());
                }
                TemplateItem::Conditional(conditional) => {
                    // A conditional branch is active whenever its parameter is
                    // supplied, so it must stand on its own.
                    self.walk_items(&conditional.items, context, path, flow.clone());
                }
                TemplateItem::BareValue(token) => {
                    // A bare `$PARAM$` is a payload injection: the caller
                    // supplies a (usually quoted) script fragment that the
                    // payload path validates in P3, never scalar matching.
                    for name in token_parameters(token) {
                        self.parameter(name).quoted_script = true;
                    }
                }
            }
        }
    }

    fn walk_property(
        &mut self,
        property: &TemplateProperty,
        context: &str,
        path: &[Arc<str>],
        flow: ScopeFlow,
    ) {
        if token_has_parameter(&property.key) {
            self.dynamic_dispatch = true;
            for name in token_parameters(&property.key) {
                self.parameter(name).used_in_key = true;
            }
            // The rendered key is unknowable at definition time; the subtree
            // is still walked under an unknown scope so parameter usage and
            // inner scope switches are not lost.
            if let TemplateValue::Block { items, .. } = &property.value {
                self.walk_items(items, context, path, ScopeFlow::Unknown);
            }
            return;
        }
        let Some(key) = single_literal(&property.key).map(str::trim) else {
            return;
        };
        if key.is_empty() {
            return;
        }
        let lowered = key.to_ascii_lowercase();
        if let Some(child_flow) = dynamic_scope_link_flow(&lowered, context, &flow) {
            // `ROOT`/`PREV`/`FROM`/event targets re-target a scope the caller
            // decides at runtime: opaque for findings, but their subtrees may
            // still contain scope-switching containers worth walking. In
            // trigger context `THIS` denotes the entry scope itself and keeps
            // the current flow.
            if let TemplateValue::Block { items, .. } = &property.value {
                // The re-targeted subtree starts a fresh rule namespace so
                // inner statements keep matching root-level rows.
                self.walk_items(items, context, &[], child_flow);
            }
            return;
        }
        // Structural trigger sub-blocks of effect containers (`limit` and
        // friends) validate in a different context against the pre-push
        // scope; the shadow walk skips them rather than mis-validating.
        // Parameter usage inside them still matters: a bare `$P$` there is a
        // quoted payload rendered at the sub-block's site, so the items are
        // walked for usage only — no scalar sites, no findings.
        if is_structural_sub_block(&lowered) {
            if let TemplateValue::Block { items, .. } = &property.value {
                self.walk_structural_usage(items);
            }
            return;
        }
        let wants_block = matches!(property.value, TemplateValue::Block { .. });
        let matching: Vec<&pdx_rules::SemanticRule> =
            semantic_rules_for_container_key(self.snapshot, context, path, key)
                .into_iter()
                .filter(|rule| {
                    semantic_rule_key_matches(self.snapshot, rule, path, key)
                        && rule_has_shape(rule, wants_block)
                })
                .collect();
        self.record_parameter_sites(property, path, &matching);

        // Scope gate: which reachable scopes execute this statement?
        let accepted = self.accepted_scopes(&flow, &matching);
        if let (Some(current), Some(accepted)) = (flow.scopes(), accepted.as_ref())
            && !matching.is_empty()
            && !current.is_empty()
            && accepted.is_empty()
        {
            self.findings.push(DynamicBodyFinding {
                kind: DynamicBodyFindingKind::ScopeContradiction,
                key_range: property.key.range,
                statement: key.to_owned(),
                reachable_scopes: current.iter().cloned().collect(),
                required_scopes: required_scopes_of_rows(&matching),
            });
            // The statement cannot run, but its subtree may still carry
            // parameter usage; walk it opaquely to keep collecting.
            if let TemplateValue::Block { items, .. } = &property.value {
                self.walk_items(items, context, &[], ScopeFlow::Unknown);
            }
            return;
        }
        // A literal statement matching no rule row in its context is the
        // definition-side replacement for expansion-time body checking of
        // closed (cache-only) files. Boolean containers are structural and
        // validated by the lint layer, not by rule rows alone.
        if matching.is_empty() && !crate::lints::is_boolean_container_key(&lowered) {
            self.findings.push(DynamicBodyFinding {
                kind: DynamicBodyFindingKind::UnknownStatement,
                key_range: property.key.range,
                statement: key.to_owned(),
                reachable_scopes: Vec::new(),
                required_scopes: Vec::new(),
            });
            if let TemplateValue::Block { items, .. } = &property.value {
                self.walk_items(items, context, &[], ScopeFlow::Unknown);
            }
            return;
        }
        // Nested dynamic-rule calls gate on the callee's own contract.
        if let Some(callee) = dynamic_kind_for_context(self.snapshot, context)
            .and_then(|kind| resolve_dynamic_definition(self.snapshot, &kind, key))
        {
            let callee_contract =
                dynamic_contract(self.snapshot, &callee.summary.kind, &callee.summary.name);
            if let (Some(current), Some(ScopeContract::Scopes(required))) =
                (flow.scopes(), callee_contract.as_ref())
            {
                let required: BTreeSet<String> = required
                    .iter()
                    .map(|scope| scope.to_ascii_lowercase())
                    .collect();
                if !current.is_empty()
                    && !required.is_empty()
                    && !current.iter().any(|scope| {
                        required
                            .iter()
                            .any(|expected| self.profile.scopes_compatible(scope, expected))
                    })
                {
                    self.findings.push(DynamicBodyFinding {
                        kind: DynamicBodyFindingKind::NestedCallMismatch,
                        key_range: property.key.range,
                        statement: key.to_owned(),
                        reachable_scopes: current.iter().cloned().collect(),
                        required_scopes: required.into_iter().collect(),
                    });
                }
            }
            // Argument blocks are parameter assignments, not effect
            // statements: record forwarding edges for scalar-bound arguments
            // and stop.
            if let TemplateValue::Block { items, .. } = &property.value {
                self.record_forwarded_arguments(&callee.summary.kind, &callee.summary.name, items);
            }
            return;
        }

        if let TemplateValue::Block { items, .. } = &property.value {
            // A child_context switch starts a fresh rule namespace; only
            // same-context accumulation extends the path (mirroring
            // `semantic_transition_destination`).
            let child_path_vec = if matching.iter().any(|rule| rule.child_context.is_some()) {
                Vec::new()
            } else {
                child_path(path, key)
            };
            self.walk_block_children(items, context, &child_path_vec, &matching, flow);
        }
    }

    /// Walks a structural sub-block's items for parameter usage only: payload
    /// parameters must be marked so their quoted arguments validate at the
    /// sub-block's render site, and key renders must stay visible — but rows
    /// there validate in a different context against the pre-push scope, so
    /// no value sites or body findings are recorded from this walk.
    fn walk_structural_usage(&mut self, items: &[TemplateItem]) {
        for item in items {
            match item {
                TemplateItem::Property(property) => {
                    if token_has_parameter(&property.key) {
                        self.dynamic_dispatch = true;
                        for name in token_parameters(&property.key) {
                            self.parameter(name).used_in_key = true;
                        }
                    }
                    if let TemplateValue::Block { items, .. } = &property.value {
                        self.walk_structural_usage(items);
                    }
                }
                TemplateItem::Conditional(conditional) => {
                    self.walk_structural_usage(&conditional.items);
                }
                TemplateItem::BareValue(token) => {
                    for name in token_parameters(token) {
                        self.parameter(name).quoted_script = true;
                    }
                }
            }
        }
    }

    /// Chooses the child flow for one block statement: same-scope containers
    /// keep the accepted flow, scope-switching containers re-target to their
    /// pushed scope, and anything unknown walks opaquely.
    fn walk_block_children(
        &mut self,
        items: &[TemplateItem],
        context: &str,
        path: &[Arc<str>],
        matching: &[&pdx_rules::SemanticRule],
        flow: ScopeFlow,
    ) {
        let child_context = matching
            .iter()
            .find_map(|rule| rule.child_context.as_deref())
            .unwrap_or(context)
            .to_owned();
        let containers: Vec<&pdx_rules::SemanticRule> = matching
            .iter()
            .copied()
            .filter(|rule| matches!(rule.shape, RuleShape::Node | RuleShape::QuotedScript))
            .collect();
        let same_scope = !containers.is_empty()
            && containers.iter().all(|rule| {
                rule.push_scope.is_none()
                    && rule.replace_scope.is_empty()
                    && rule
                        .child_context
                        .as_deref()
                        .is_none_or(|child| child.eq_ignore_ascii_case(context))
            });
        if same_scope {
            let child_flow = match self.accepted_scopes(&flow, matching) {
                Some(accepted) if !accepted.is_empty() => ScopeFlow::Known(accepted),
                _ => flow.clone(),
            };
            self.walk_items(items, &child_context, path, child_flow);
            return;
        }
        let pushed: BTreeSet<String> = containers
            .iter()
            .filter_map(|rule| rule.push_scope.as_deref())
            .filter(|scope| !scope.eq_ignore_ascii_case("any"))
            .map(|scope| scope.to_ascii_lowercase())
            .collect();
        let register_retargeting = containers.iter().any(|rule| !rule.replace_scope.is_empty());
        if pushed.is_empty() || register_retargeting {
            // No single pushed scope (unknown statement, quoted script, or a
            // register rewrite): opaque subtree.
            self.walk_items(items, &child_context, path, ScopeFlow::Unknown);
            return;
        }
        self.walk_items(items, &child_context, path, ScopeFlow::Known(pushed));
    }

    /// Scopes in `flow` the statement's rows accept; `None` when the flow is
    /// unknown (everything passes).
    fn accepted_scopes(
        &self,
        flow: &ScopeFlow,
        matching: &[&pdx_rules::SemanticRule],
    ) -> Option<BTreeSet<String>> {
        let current = flow.scopes()?;
        let accepted: BTreeSet<String> = current
            .iter()
            .filter(|scope| {
                matching.iter().any(|rule| {
                    rule.allowed_scopes.is_empty()
                        || rule
                            .allowed_scopes
                            .iter()
                            .any(|expected| self.profile.scopes_compatible(scope, expected))
                })
            })
            .cloned()
            .collect();
        Some(accepted)
    }

    fn record_parameter_sites(
        &mut self,
        property: &TemplateProperty,
        _path: &[Arc<str>],
        matching: &[&pdx_rules::SemanticRule],
    ) {
        let TemplateValue::Scalar(token) = &property.value else {
            return;
        };
        let names: Vec<String> = token_parameters(token).map(str::to_owned).collect();
        if names.is_empty() {
            return;
        }
        let matchers: Vec<ValueMatcher> = matching.iter().map(|rule| rule.value.clone()).collect();
        let payload_site = matching
            .iter()
            .any(|rule| matches!(rule.shape, RuleShape::QuotedScript));
        for name in names {
            let usage = self.parameter(&name);
            // A quoted scalar template (`name = "$name$"`) is a string
            // substitution: the caller's quoted argument validates as a
            // scalar against the site's matchers. Only quoted-script-shaped
            // rows mark payloads (bare value-position injections are marked
            // by the walk).
            if payload_site {
                usage.quoted_script = true;
            }
            // Only whole-token parameters constrain the bare argument
            // directly. Parameters embedded with literal affixes
            // (`$RT$_rebels`) render to a derived runtime value: the site
            // keeps the affixes so validation can splice the argument back
            // in and match the rendered value against the same matchers
            // that accept a directly written one. Sites whose matchers are
            // all open reference types (`name = $X$_loyal` rendering a
            // freshly generated modifier name) cannot prove a closed value
            // domain and stay unconstrained.
            if is_bare_parameter(token, &name) && !matchers.is_empty() {
                usage.sites.push(matchers.clone());
            } else if !matchers.is_empty()
                && matchers.iter().any(|matcher| {
                    matches!(matcher, ValueMatcher::Exact(_) | ValueMatcher::Enum(_))
                })
                && let Some((prefix, suffix)) = literal_token_affixes(token, &name)
            {
                usage.affixed_sites.push(AffixedValueSite {
                    prefix,
                    suffix,
                    matchers: matchers.clone(),
                });
            }
        }
    }

    /// Records forwarding edges for arguments bound as
    /// `callee = { PARAM = $OWNER_PARAM$ }` so call-site validation can borrow
    /// the callee parameter's own constraints. Keys that render the callee
    /// parameter name from a `$param$` fragment record an edge with no fixed
    /// parameter target.
    fn record_forwarded_arguments(
        &mut self,
        callee_kind: &str,
        callee_name: &str,
        items: &[TemplateItem],
    ) {
        for item in items {
            let TemplateItem::Property(property) = item else {
                continue;
            };
            if let TemplateValue::Scalar(token) = &property.value {
                let parameter = single_literal(&property.key)
                    .map(str::trim)
                    .filter(|key| !key.is_empty())
                    .map(str::to_owned);
                for name in token_parameters(token) {
                    self.parameter(name).forwarded_to.push(ForwardedParameter {
                        kind: callee_kind.to_owned(),
                        name: callee_name.to_owned(),
                        parameter: parameter.clone(),
                    });
                }
            }
        }
    }

    fn parameter(&mut self, name: &str) -> &mut ParameterUsage {
        self.parameters
            .entry(name.to_ascii_lowercase())
            .or_default()
    }
}

/// The parent path of a block property's children.
fn child_path(path: &[Arc<str>], key: &str) -> Vec<Arc<str>> {
    let mut child = Vec::with_capacity(path.len() + 1);
    child.extend(path.iter().cloned());
    child.push(Arc::from(key));
    child
}

/// True for scope-link keys whose target the caller decides at runtime;
/// returns the flow their subtree walks under.
fn dynamic_scope_link_flow(lowered: &str, context: &str, flow: &ScopeFlow) -> Option<ScopeFlow> {
    match lowered {
        "this" if context.eq_ignore_ascii_case("trigger") => Some(flow.clone()),
        "this" | "root" | "prev" | "from" | "fromfrom" | "fromfromfrom" => Some(ScopeFlow::Unknown),
        _ if lowered.starts_with("event_target:") => Some(ScopeFlow::Unknown),
        _ => None,
    }
}

/// Sub-blocks that validate in a different context than their parent effect
/// container; skipped by the shadow walk.
pub(crate) fn is_structural_sub_block(lowered: &str) -> bool {
    matches!(
        lowered,
        "limit" | "trigger" | "mtth" | "mean_time_to_happen"
    )
}

fn required_scopes_of_rows(rows: &[&pdx_rules::SemanticRule]) -> Vec<String> {
    let mut scopes: BTreeSet<String> = BTreeSet::new();
    for rule in rows {
        for scope in &rule.allowed_scopes {
            scopes.insert(scope.to_ascii_lowercase());
        }
    }
    scopes.into_iter().collect()
}

/// Resolves the dynamic-definition kind whose descriptor declares `context`
/// as its body context (e.g. `effect` -> `scripted_effect`), from rule data
/// only.
pub(crate) fn dynamic_kind_for_context(
    snapshot: &AnalysisSnapshot,
    context: &str,
) -> Option<String> {
    snapshot
        .rules()
        .model()
        .semantic
        .type_descriptors
        .iter()
        .find(|(_, descriptor)| {
            descriptor
                .dynamic_definition
                .as_ref()
                .is_some_and(|dynamic_descriptor| {
                    dynamic_descriptor.enabled
                        && dynamic_descriptor
                            .body_context
                            .eq_ignore_ascii_case(context)
                })
        })
        .map(|(kind, _)| kind.clone())
}

fn token_has_parameter(token: &TemplateToken) -> bool {
    token
        .fragments
        .iter()
        .any(|fragment| matches!(fragment, TemplateFragment::Parameter { .. }))
}

/// True when the parameter is the token's entire content; embedded
/// parameters (`$param$_suffix`, `$prefix$_$param$`) render to derived
/// runtime names.
fn is_bare_parameter(token: &TemplateToken, name: &str) -> bool {
    matches!(
        token.fragments.as_slice(),
        [TemplateFragment::Parameter { name: parameter, .. }]
            if parameter.eq_ignore_ascii_case(name)
    )
}

/// The literal affixes around the parameter's single occurrence, when every
/// other fragment of the token is a literal: `$RT$_rebels` yields
/// `("", "_rebels")`. Tokens embedding other parameters cannot attribute
/// their affixes to this one and return `None`.
fn literal_token_affixes(token: &TemplateToken, name: &str) -> Option<(String, String)> {
    let mut prefix = String::new();
    let mut suffix = String::new();
    let mut seen = false;
    for fragment in &token.fragments {
        match fragment {
            TemplateFragment::Literal(literal) => {
                if seen {
                    suffix.push_str(literal);
                } else {
                    prefix.push_str(literal);
                }
            }
            TemplateFragment::Parameter {
                name: parameter, ..
            } => {
                if seen || !parameter.eq_ignore_ascii_case(name) {
                    return None;
                }
                seen = true;
            }
        }
    }
    seen.then_some((prefix, suffix))
}

fn token_parameters(token: &TemplateToken) -> impl Iterator<Item = &str> {
    token
        .fragments
        .iter()
        .filter_map(|fragment| match fragment {
            TemplateFragment::Parameter { name, .. } => Some(name.as_str()),
            TemplateFragment::Literal(_) => None,
        })
}

fn single_literal(token: &TemplateToken) -> Option<&str> {
    match token.fragments.as_slice() {
        [TemplateFragment::Literal(text)] => Some(text),
        _ => None,
    }
}

fn rule_has_shape(rule: &pdx_rules::SemanticRule, wants_block: bool) -> bool {
    match rule.shape {
        RuleShape::Node | RuleShape::QuotedScript => wants_block,
        RuleShape::Leaf | RuleShape::LeafValue => !wants_block,
        RuleShape::ValueClause => false,
    }
}
