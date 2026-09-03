//! Definition-site entry-scope contracts for dynamic definitions.
//!
//! A dynamic definition's body executes in the caller's scope, so the definition is
//! only usable where every statement in its body is valid. This module infers
//! that entry contract once per definition: it walks the lowered template,
//! collects the `allowed_scopes` of each body statement's matching rules, and
//! keeps the scopes that satisfy all of them (compatibility-aware, mirroring
//! `semantic_scope_allows`). Scope-switching containers (`any_country`, …)
//! constrain the entry only through their own rule row; same-scope containers
//! (`AND`/`OR`/`NOT`, `if`, `limit`) are descended so their children constrain
//! the entry too. `OR` branches union — one satisfiable branch is enough —
//! while every other descended statement intersects. Dynamic scope links
//! (`ROOT`/`PREV`/`FROM`/`THIS`, event targets) re-target a scope decided by
//! the caller's runtime context, so their bodies are opaque to the entry
//! contract. An empty intersection is a definition that can never run
//! correctly and is reported at the definition site — the Rust principle:
//! reject the definition rather than every call site.
//!
//! Calls with a `$param$` in key position dispatch dynamically; their targets
//! are unknowable at definition time, so they do not narrow the contract (the
//! flag is kept for reporting and hover). Nested dynamic-definition calls
//! contribute the callee's own inferred contract.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use pdx_engine::hir::{
    ScopeValue, TemplateFragment, TemplateItem, TemplateProperty, TemplateToken, TemplateValue,
};
use pdx_engine::{AnalysisSnapshot, DocumentSource};
use pdx_rules::{GameProfile, RuleShape};

use crate::semantic::{
    ResolvedDynamicDefinition, dynamic_definition_type, probe_query_cache,
    resolve_dynamic_definition,
};
use crate::support::ParsedInput;
use crate::types::{CancellationToken, Cancelled, Diagnostic, DiagnosticCode, uncancelled};

/// Cache key for the workspace-wide contract report inside the query cache.
const CONTRACT_CACHE_KEY: &str = "dynamic-scope-contracts";

/// One inferred entry contract.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ScopeContract {
    /// No body statement constrains the entry scope: usable anywhere.
    Unconstrained,
    /// Entry scope must be compatible with at least one listed scope.
    Scopes(Vec<String>),
    /// No scope satisfies every body statement: the definition can never run.
    Empty,
    /// Inference could not finish (unresolvable or cyclic definition).
    Unknown,
}

impl ScopeContract {
    /// True when `scope` may enter a definition carrying this contract.
    pub(crate) fn accepts(&self, profile: &GameProfile, scope: &str) -> bool {
        match self {
            Self::Unconstrained | Self::Unknown => true,
            Self::Scopes(scopes) => scopes
                .iter()
                .any(|expected| profile.scopes_compatible(scope, expected)),
            Self::Empty => false,
        }
    }

    fn display(&self) -> String {
        match self {
            Self::Unconstrained => "any".to_owned(),
            Self::Scopes(scopes) => scopes.join(", "),
            Self::Empty => "none (definition can never run)".to_owned(),
            Self::Unknown => "unknown".to_owned(),
        }
    }
}

/// Workspace-wide inference result for every live dynamic definition.
#[derive(Clone, Debug, Default)]
pub(crate) struct DynamicContractReport {
    contracts: BTreeMap<(String, String), ScopeContract>,
    /// Definitions whose template dispatches through a `$param$` key.
    dynamic: BTreeSet<(String, String)>,
}

impl DynamicContractReport {
    pub(crate) fn contract(&self, kind: &str, name: &str) -> Option<&ScopeContract> {
        self.contracts
            .get(&(kind.to_ascii_lowercase(), name.to_ascii_lowercase()))
    }

    pub(crate) fn is_dynamic(&self, kind: &str, name: &str) -> bool {
        self.dynamic
            .contains(&(kind.to_ascii_lowercase(), name.to_ascii_lowercase()))
    }
}

/// Returns definition-site diagnostics for dynamic definitions in `input` whose
/// inferred entry contract is empty.
pub(crate) fn dynamic_contract_diagnostics(
    snapshot: &AnalysisSnapshot,
    input: &ParsedInput,
    cancellation: &CancellationToken,
) -> Result<Vec<Diagnostic>, Cancelled> {
    cancellation.checkpoint()?;
    if input.format != pdx_parser::FileFormat::Script {
        return Ok(Vec::new());
    }
    let Some(hir) = input.hir.as_deref() else {
        return Ok(Vec::new());
    };
    let report = dynamic_contract_report(snapshot, cancellation)?;
    if report.contracts.is_empty() {
        return Ok(Vec::new());
    }
    let mut diagnostics = Vec::new();
    for definition in hir.definitions() {
        if !dynamic_definition_type(snapshot, &definition.kind) {
            continue;
        }
        let Some(ScopeContract::Empty) = report.contract(&definition.kind, &definition.name) else {
            continue;
        };
        diagnostics.push(Diagnostic::new(
            DiagnosticCode::EmptyScopeContract,
            DiagnosticCode::EmptyScopeContract.severity(),
            definition.selection_range,
            format!(
                "dynamic definition `{}` has an empty inferred entry scope: no scope satisfies every statement in its body",
                definition.name
            ),
        ));
    }
    Ok(diagnostics)
}

/// Returns diagnostics for dynamic call sites whose ambient scope cannot enter
/// the callee's inferred contract.
///
/// A definition body runs in the caller's scope, so a call in an incompatible
/// scope executes the body where its statements do not apply. Only pure dynamic
/// calls participate: a key that also matches a builtin rule row may be the
/// builtin (the contract's `Any` case), and keys with no rows stay with the
/// unknown-key lint. Empty contracts are already reported at their definition
/// site and are not repeated per call site.
pub(crate) fn dynamic_call_site_diagnostics(
    snapshot: &AnalysisSnapshot,
    input: &ParsedInput,
    cancellation: &CancellationToken,
) -> Result<Vec<Diagnostic>, Cancelled> {
    cancellation.checkpoint()?;
    if input.format != pdx_parser::FileFormat::Script {
        return Ok(Vec::new());
    }
    let Some(hir) = input.hir.as_deref() else {
        return Ok(Vec::new());
    };
    let report = dynamic_contract_report(snapshot, cancellation)?;
    if report.contracts.is_empty() {
        return Ok(Vec::new());
    }
    let profile = snapshot.game_profile();
    let mut diagnostics = Vec::new();
    for property in hir.properties() {
        cancellation.checkpoint()?;
        let Some(fact) = hir.scope_fact_at(property.key_range) else {
            continue;
        };
        // The callee body executes in the scope active before this property;
        // an ambiguous or unknown ambient is not evidence of misuse.
        let Some(ScopeValue::Known(scopes)) = fact.state.current.first() else {
            continue;
        };
        if scopes.len() != 1 {
            continue;
        }
        let ambient = scopes[0].to_string();
        let parent_path: Vec<Arc<str>> = fact
            .parent_path
            .iter()
            .map(|segment| Arc::<str>::from(segment.as_str()))
            .collect();
        let mut dynamic_kind: Option<String> = None;
        let mut builtin = false;
        for rule in crate::semantic::semantic_rules_for_container_key(
            snapshot,
            &fact.context,
            &parent_path,
            &property.key,
        ) {
            if !crate::semantic::semantic_rule_key_matches(
                snapshot,
                rule,
                &parent_path,
                &property.key,
            ) {
                continue;
            }
            match &rule.key {
                pdx_rules::KeyMatcher::Type(kind) | pdx_rules::KeyMatcher::Dynamic(kind)
                    if crate::semantic::dynamic_definition_type(snapshot, kind) =>
                {
                    dynamic_kind.get_or_insert_with(|| kind.clone());
                }
                _ => {
                    builtin = true;
                    break;
                }
            }
        }
        if builtin {
            continue;
        }
        let Some(dynamic_kind) = dynamic_kind else {
            continue;
        };
        let Some(contract) = report.contract(&dynamic_kind, &property.key) else {
            continue;
        };
        // Empty contracts are already reported at the definition site;
        // unconstrained and unknown contracts accept every scope.
        let ScopeContract::Scopes(expected) = contract else {
            continue;
        };
        if contract.accepts(profile, &ambient) {
            continue;
        }
        diagnostics.push(Diagnostic::new(
            DiagnosticCode::DynamicCallScopeMismatch,
            DiagnosticCode::DynamicCallScopeMismatch.severity(),
            property.key_range,
            format!(
                "dynamic definition `{}` requires entry scope {} but is called in `{}` scope",
                property.key,
                expected.join(", "),
                ambient
            ),
        ));
    }
    Ok(diagnostics)
}

/// Returns the workspace contract for one definition, computing the report when the
/// per-revision cache is cold. Hover and call-site validation share this view.
pub(crate) fn dynamic_contract(
    snapshot: &AnalysisSnapshot,
    kind: &str,
    name: &str,
) -> Option<ScopeContract> {
    let cancellation = CancellationToken::new();
    let report = uncancelled(dynamic_contract_report(snapshot, &cancellation));
    report.contract(kind, name).cloned()
}

/// One-line hover summary of a definition's inferred contract.
pub(crate) fn contract_hover_line(snapshot: &AnalysisSnapshot, kind: &str, name: &str) -> String {
    let contract = dynamic_contract(snapshot, kind, name);
    let scope = contract.map_or_else(|| "unknown".to_owned(), |contract| contract.display());
    let dispatch = if contract_is_dynamic(snapshot, kind, name) {
        " (dynamic `$param$` dispatch: not narrowed)"
    } else {
        ""
    };
    format!("- Inferred entry scope: {scope}{dispatch}")
}

fn contract_is_dynamic(snapshot: &AnalysisSnapshot, kind: &str, name: &str) -> bool {
    let cancellation = CancellationToken::new();
    let report = uncancelled(dynamic_contract_report(snapshot, &cancellation));
    report.is_dynamic(kind, name)
}

fn dynamic_contract_report(
    snapshot: &AnalysisSnapshot,
    cancellation: &CancellationToken,
) -> Result<Arc<DynamicContractReport>, Cancelled> {
    let revision = snapshot.revision();
    if let Some(cached) =
        probe_query_cache::<DynamicContractReport>(snapshot, revision, &[CONTRACT_CACHE_KEY])
    {
        return Ok(cached);
    }
    cancellation.checkpoint()?;
    let report = build_contract_report(snapshot, cancellation)?;
    if std::env::var("PDX_DEBUG_DYNAMIC_CONTRACTS").is_ok_and(|value| !value.is_empty()) {
        let count = |predicate: &dyn Fn(&ScopeContract) -> bool| {
            report
                .contracts
                .values()
                .filter(|contract| predicate(contract))
                .count()
        };
        eprintln!(
            "dynamic contracts: {} definitions, {} constrained, {} unconstrained, {} dynamic, {} empty",
            report.contracts.len(),
            count(&|contract| matches!(contract, ScopeContract::Scopes(_))),
            count(&|contract| matches!(contract, ScopeContract::Unconstrained)),
            report.dynamic.len(),
            count(&|contract| matches!(contract, ScopeContract::Empty)),
        );
    }
    let report = Arc::new(report);
    snapshot.query_cache().insert(
        revision,
        pdx_engine::CacheDomain::Documents,
        CONTRACT_CACHE_KEY.to_owned(),
        report.clone(),
    );
    Ok(report)
}

/// The entry constraint contributed by one body statement.
enum Statement<'rule> {
    /// Valid wherever any matching rule row accepts the scope.
    Rows(Vec<&'rule pdx_rules::SemanticRule>),
    /// Valid for exactly the callee contract's scopes.
    Scopes(Vec<String>),
    /// A callee with an empty contract: nothing can enter this call chain.
    Impossible,
    /// A key that is both a builtin rule and a dynamic definition: either accepts.
    Any(Vec<Statement<'rule>>),
}

impl Statement<'_> {
    fn accepts(&self, profile: &GameProfile, scope: &str) -> bool {
        match self {
            Self::Rows(rows) => rows.iter().any(|rule| {
                rule.allowed_scopes.is_empty()
                    || rule
                        .allowed_scopes
                        .iter()
                        .any(|expected| profile.scopes_compatible(scope, expected))
            }),
            Self::Scopes(scopes) => scopes
                .iter()
                .any(|expected| profile.scopes_compatible(scope, expected)),
            Self::Impossible => false,
            Self::Any(alternatives) => alternatives
                .iter()
                .any(|statement| statement.accepts(profile, scope)),
        }
    }
}

struct ContractInference<'a> {
    snapshot: &'a AnalysisSnapshot,
    profile: &'a GameProfile,
    /// Memoized contracts keyed by `(kind lower, name lower)`.
    memo: BTreeMap<(String, String), ScopeContract>,
    /// Definitions currently being inferred (cycle guard).
    visiting: BTreeSet<(String, String)>,
    dynamic: BTreeSet<(String, String)>,
}

impl<'a> ContractInference<'a> {
    fn contract_of(&mut self, resolved: &ResolvedDynamicDefinition) -> ScopeContract {
        let key = (
            resolved.summary.kind.to_ascii_lowercase(),
            resolved.summary.name.to_ascii_lowercase(),
        );
        if let Some(cached) = self.memo.get(&key) {
            return cached.clone();
        }
        if !self.visiting.insert(key.clone()) {
            // Recursive participation is already reported as a definition
            // cycle; contracts must not stack-overflow on top of it.
            return ScopeContract::Unknown;
        }
        let contract = match resolved.summary.template.as_ref() {
            Some(template) => {
                let body_context = resolved.body_context.clone();
                let mut inference = StatementInference::new(self.snapshot, self.profile);
                inference.walk_items(&template.items, &body_context, self);
                if inference.dynamic {
                    self.dynamic.insert(key.clone());
                }
                inference.finish()
            }
            None => ScopeContract::Unknown,
        };
        self.visiting.remove(&key);
        self.memo.insert(key, contract.clone());
        contract
    }
}

/// Accumulates the constraint of every body statement in one definition.
struct StatementInference<'a> {
    snapshot: &'a AnalysisSnapshot,
    profile: &'a GameProfile,
    /// Candidate scopes gathered from every constraining statement.
    candidates: BTreeSet<String>,
    /// Entry constraint of each body statement, in source order.
    statements: Vec<Statement<'a>>,
    /// Set when a `$param$` was used in key position.
    dynamic: bool,
}

impl<'a> StatementInference<'a> {
    fn new(snapshot: &'a AnalysisSnapshot, profile: &'a GameProfile) -> Self {
        Self {
            snapshot,
            profile,
            candidates: BTreeSet::new(),
            statements: Vec::new(),
            dynamic: false,
        }
    }

    fn finish(self) -> ScopeContract {
        if self.statements.is_empty() {
            return ScopeContract::Unconstrained;
        }
        let scopes = self
            .candidates
            .iter()
            .filter(|scope| {
                self.statements
                    .iter()
                    .all(|statement| statement.accepts(self.profile, scope))
            })
            .cloned()
            .collect::<Vec<_>>();
        if scopes.is_empty() {
            ScopeContract::Empty
        } else {
            ScopeContract::Scopes(scopes)
        }
    }

    fn walk_items(
        &mut self,
        items: &[TemplateItem],
        context: &str,
        contracts: &mut ContractInference<'_>,
    ) {
        for item in items {
            match item {
                TemplateItem::Property(property) => {
                    self.walk_property(property, context, contracts);
                }
                TemplateItem::Conditional(conditional) => {
                    // A conditional branch is active whenever its parameter is
                    // supplied, so its statements constrain the contract too.
                    self.walk_items(&conditional.items, context, contracts);
                }
                TemplateItem::BareValue(_) => {}
            }
        }
    }

    fn walk_property(
        &mut self,
        property: &TemplateProperty,
        context: &str,
        contracts: &mut ContractInference<'_>,
    ) {
        if token_has_parameter(&property.key) {
            self.dynamic = true;
            return;
        }
        let Some(key) = single_literal(&property.key).map(str::trim) else {
            return;
        };
        if key.is_empty() {
            return;
        }
        let lowered = key.to_ascii_lowercase();
        if is_dynamic_scope_link(&lowered, context) {
            // Dynamic scope links re-target a scope the caller decides at
            // runtime (the event root, the previous scope, the sender, a
            // saved event target); their bodies constrain that target, not
            // the definition entry, so they neither constrain nor descend.
            return;
        }
        if lowered == "or" {
            if let TemplateValue::Block { items, .. } = &property.value
                && let Some(statement) = self.or_statement(items, context, contracts)
            {
                self.statements.push(statement);
            }
            return;
        }
        // Rows matching this statement in the body context; keep only rows of
        // the statement's own shape so a scalar-only row cannot narrow a block
        // call and vice versa.
        let wants_block = matches!(property.value, TemplateValue::Block { .. });
        let matching: Vec<&'a pdx_rules::SemanticRule> =
            crate::semantic::semantic_rules_for_container_key(self.snapshot, context, &[], key)
                .into_iter()
                .filter(|rule| {
                    crate::semantic::semantic_rule_key_matches(self.snapshot, rule, &[], key)
                        && rule_has_shape(rule, wants_block)
                })
                .collect();
        // A same-kind dynamic definition call contributes its own contract; a key
        // that is both builtin and dynamic accepts through either path.
        let callee = dynamic_kind_for_context(self.snapshot, context)
            .and_then(|kind| resolve_dynamic_definition(self.snapshot, &kind, key));
        let callee_contract = callee.as_ref().map(|callee| contracts.contract_of(callee));
        let mut statement = Vec::with_capacity(2);
        if matching.iter().all(|rule| !rule.allowed_scopes.is_empty()) && !matching.is_empty() {
            for rule in &matching {
                for scope in &rule.allowed_scopes {
                    self.candidates.insert(scope.to_ascii_lowercase());
                }
            }
            statement.push(Statement::Rows(matching.clone()));
        }
        match callee_contract {
            Some(ScopeContract::Scopes(scopes)) => {
                for scope in &scopes {
                    self.candidates.insert(scope.clone());
                }
                statement.push(Statement::Scopes(scopes));
            }
            Some(ScopeContract::Empty) => statement.push(Statement::Impossible),
            Some(ScopeContract::Unconstrained | ScopeContract::Unknown) | None => {}
        }
        match statement.len() {
            0 => {}
            1 => self
                .statements
                .push(statement.pop().expect("one statement")),
            _ => self.statements.push(Statement::Any(statement)),
        }

        // Descend into same-scope containers so nested statements also
        // constrain the entry scope. A rule row that pushes or replaces the
        // scope evaluates its children elsewhere, so it must not contribute.
        if let TemplateValue::Block { items, .. } = &property.value {
            let containers: Vec<&pdx_rules::SemanticRule> = matching
                .iter()
                .copied()
                .filter(|rule| matches!(rule.shape, RuleShape::Node | RuleShape::QuotedScript))
                .collect();
            let descend = !containers.is_empty()
                && containers.iter().all(|rule| {
                    rule.push_scope.is_none()
                        && rule.replace_scope.is_empty()
                        && rule.child_context.as_deref().is_none_or(|child| {
                            child.eq_ignore_ascii_case(context)
                                || child.eq_ignore_ascii_case("trigger")
                                || child.eq_ignore_ascii_case("effect")
                        })
                });
            if descend {
                let child_context = containers
                    .iter()
                    .find_map(|rule| rule.child_context.as_deref())
                    .unwrap_or(context)
                    .to_owned();
                self.walk_items(items, &child_context, contracts);
            }
        }
    }

    /// Infers every `OR` branch in isolation and unions the results: the
    /// entry scope only needs one satisfiable branch, so branch contracts
    /// combine with any-of semantics instead of intersecting.
    fn or_statement(
        &mut self,
        items: &[TemplateItem],
        context: &str,
        contracts: &mut ContractInference<'_>,
    ) -> Option<Statement<'a>> {
        let mut branches = Vec::new();
        for item in items {
            let mut branch = StatementInference::new(self.snapshot, self.profile);
            branch.walk_items(std::slice::from_ref(item), context, contracts);
            if branch.dynamic {
                self.dynamic = true;
            }
            match branch.finish() {
                ScopeContract::Scopes(scopes) => {
                    for scope in &scopes {
                        self.candidates.insert(scope.clone());
                    }
                    branches.push(Statement::Scopes(scopes));
                }
                ScopeContract::Empty => branches.push(Statement::Impossible),
                // A branch open to any scope — or one inference could not
                // resolve — keeps the whole `OR` unconstrained.
                ScopeContract::Unconstrained | ScopeContract::Unknown => return None,
            }
        }
        if branches.is_empty() {
            return None;
        }
        Some(Statement::Any(branches))
    }
}

/// True for scope-link keys whose target the caller decides at runtime. Their
/// bodies constrain that unknown target, never the definition entry. `THIS` is the
/// exception in trigger context, where it denotes the entry scope itself;
/// everywhere else a `THIS` block does not run in the entry scope.
fn is_dynamic_scope_link(lowered: &str, context: &str) -> bool {
    match lowered {
        "this" => !context.eq_ignore_ascii_case("trigger"),
        "root" | "prev" | "from" | "fromfrom" | "fromfromfrom" => true,
        _ => lowered.starts_with("event_target:"),
    }
}

/// Resolves the dynamic-definition kind whose descriptor declares `context` as its
/// body context (e.g. `effect` -> `scripted_effect`), from rule data only.
fn dynamic_kind_for_context(snapshot: &AnalysisSnapshot, context: &str) -> Option<String> {
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

fn rule_has_shape(rule: &pdx_rules::SemanticRule, wants_block: bool) -> bool {
    match rule.shape {
        RuleShape::Node | RuleShape::QuotedScript => wants_block,
        RuleShape::Leaf | RuleShape::LeafValue => !wants_block,
        RuleShape::ValueClause => false,
    }
}

fn token_has_parameter(token: &TemplateToken) -> bool {
    token
        .fragments
        .iter()
        .any(|fragment| matches!(fragment, TemplateFragment::Parameter { .. }))
}

fn single_literal(token: &TemplateToken) -> Option<&str> {
    match token.fragments.as_slice() {
        [TemplateFragment::Literal(text)] => Some(text),
        _ => None,
    }
}

fn build_contract_report(
    snapshot: &AnalysisSnapshot,
    cancellation: &CancellationToken,
) -> Result<DynamicContractReport, Cancelled> {
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
    let mut inference = ContractInference {
        snapshot,
        profile,
        memo: BTreeMap::new(),
        visiting: BTreeSet::new(),
        dynamic: BTreeSet::new(),
    };
    let mut contracts = BTreeMap::new();
    for (kind, name) in &candidates {
        cancellation.checkpoint()?;
        let Some(resolved) = resolve_dynamic_definition(snapshot, kind, name) else {
            continue;
        };
        let key = (
            resolved.summary.kind.to_ascii_lowercase(),
            resolved.summary.name.to_ascii_lowercase(),
        );
        let contract = inference.contract_of(&resolved);
        contracts.insert(key, contract);
    }
    Ok(DynamicContractReport {
        contracts,
        dynamic: inference.dynamic,
    })
}
