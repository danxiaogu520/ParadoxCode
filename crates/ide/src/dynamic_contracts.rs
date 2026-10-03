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

use engine::{AnalysisSnapshot, DocumentSource};
use hir::{ScopeValue, TemplateFragment, TemplateItem, TemplateToken, TemplateValue};
use rules::GameProfile;

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
    if input.format != parser::FileFormat::Script {
        return Ok(Vec::new());
    }
    let Some(hir) = input.hir.as_deref() else {
        return Ok(Vec::new());
    };
    if !hir
        .definitions()
        .iter()
        .any(|definition| dynamic_definition_type(snapshot, &definition.kind))
    {
        return Ok(Vec::new());
    }
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
    if input.format != parser::FileFormat::Script {
        return Ok(Vec::new());
    }
    let Some(hir) = input.hir.as_deref() else {
        return Ok(Vec::new());
    };
    let profile = snapshot.game_profile();
    let mut report = None;
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
        let mut dynamic_kind: Option<String> = None;
        let mut builtin = false;
        if hir.uses_ir()
            && let Some(field_fact) = hir.field_fact_at(property.key_range)
        {
            for id in &field_fact.fields {
                if let Some(kind) =
                    crate::ir_template::template_kind(snapshot.ir(), snapshot.ir().field(*id).key)
                {
                    dynamic_kind = Some(kind);
                } else {
                    builtin = true;
                }
            }
        }
        if builtin {
            continue;
        }
        let Some(dynamic_kind) = dynamic_kind else {
            continue;
        };
        let report = match report.as_ref() {
            Some(report) => report,
            None => {
                let computed = dynamic_contract_report(snapshot, cancellation)?;
                if computed.contracts.is_empty() {
                    return Ok(Vec::new());
                }
                report.insert(computed)
            }
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
            DiagnosticCode::WrongScope,
            DiagnosticCode::WrongScope.severity(),
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

/// Loads the workspace-wide contract report for cancellable consumers such as
/// completion filtering. The report is cached per revision, and the caller's
/// token must reach the workspace traversal so obsolete editor requests stop
/// before consuming a full core.
pub(crate) fn dynamic_contract_report_view(
    snapshot: &AnalysisSnapshot,
    cancellation: &CancellationToken,
) -> Result<std::sync::Arc<DynamicContractReport>, Cancelled> {
    dynamic_contract_report(snapshot, cancellation)
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
    // The report derives from the dynamic-definition set, so it lives in the
    // definitions domain: editing a document that only calls scripted
    // definitions keeps it valid. A worker whose revision the domain has
    // advanced past must not rebuild — the insert would be dropped and every
    // following probe would rerun the workspace-wide inference.
    if snapshot
        .query_cache()
        .is_superseded(engine::CacheDomain::Definitions, revision)
    {
        return Ok(Arc::new(DynamicContractReport::default()));
    }
    cancellation.checkpoint()?;
    let report = build_contract_report(snapshot, cancellation)?;
    if std::env::var("PDC_DEBUG_DYNAMIC_CONTRACTS").is_ok_and(|value| !value.is_empty()) {
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
        engine::CacheDomain::Definitions,
        CONTRACT_CACHE_KEY.to_owned(),
        report.clone(),
    );
    Ok(report)
}

struct ContractInference<'a> {
    snapshot: &'a AnalysisSnapshot,
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
            return ScopeContract::Unknown;
        }
        let contract = match resolved.summary.template.as_ref() {
            Some(template) => {
                let contract = self
                    .snapshot
                    .ir()
                    .schema_by_name(&resolved.body_context)
                    .map_or(ScopeContract::Unknown, |schema| {
                        self.ir_items(schema, &template.items)
                    });
                if template_dispatches(&template.items) {
                    self.dynamic.insert(key.clone());
                }
                contract
            }
            None => ScopeContract::Unknown,
        };
        self.visiting.remove(&key);
        self.memo.insert(key, contract.clone());
        contract
    }

    fn ir_items(&mut self, schema: rules::ir::SchemaId, items: &[TemplateItem]) -> ScopeContract {
        let contracts = items
            .iter()
            .map(|item| self.ir_item(schema, item))
            .collect::<Vec<_>>();
        self.combine_ir_contracts(&contracts, false)
    }

    fn combine_ir_contracts(&self, contracts: &[ScopeContract], union: bool) -> ScopeContract {
        if contracts.is_empty() {
            return ScopeContract::Unconstrained;
        }
        if union
            && contracts.iter().any(|contract| {
                matches!(
                    contract,
                    ScopeContract::Unconstrained | ScopeContract::Unknown
                )
            })
        {
            return ScopeContract::Unconstrained;
        }
        if !union
            && contracts.iter().all(|contract| {
                matches!(
                    contract,
                    ScopeContract::Unconstrained | ScopeContract::Unknown
                )
            })
        {
            return ScopeContract::Unconstrained;
        }
        let ir = self.snapshot.ir();
        let candidates = contracts
            .iter()
            .filter_map(|contract| match contract {
                ScopeContract::Scopes(scopes) => Some(scopes),
                _ => None,
            })
            .flatten()
            .cloned()
            .collect::<BTreeSet<_>>();
        let scopes = candidates
            .iter()
            .filter(|candidate| {
                let accepts = |contract: &ScopeContract| match contract {
                    ScopeContract::Scopes(expected) => expected.iter().any(|expected| {
                        ir.strings()
                            .lookup_folded(expected)
                            .is_some_and(|expected| {
                                ir.strings()
                                    .lookup_folded(candidate)
                                    .is_some_and(|actual| ir.scopes_compatible(actual, expected))
                            })
                    }),
                    ScopeContract::Empty => false,
                    _ => true,
                };
                if union {
                    contracts.iter().any(accepts)
                } else {
                    contracts.iter().all(accepts)
                }
            })
            .cloned()
            .collect::<Vec<_>>();
        if scopes.is_empty() {
            ScopeContract::Empty
        } else {
            ScopeContract::Scopes(scopes)
        }
    }

    fn ir_item(&mut self, schema: rules::ir::SchemaId, item: &TemplateItem) -> ScopeContract {
        use rules::ir::{FieldValue, Matcher, Shape};
        let ir = self.snapshot.ir();
        let property = match item {
            TemplateItem::Conditional(conditional) => {
                return self.ir_items(schema, &conditional.items);
            }
            TemplateItem::BareValue(_) | TemplateItem::Recover(_) => {
                return ScopeContract::Unconstrained;
            }
            TemplateItem::Property(property) => property,
        };
        let Some(key) = single_literal(&property.key) else {
            return ScopeContract::Unconstrained;
        };
        let shape = match &property.value {
            TemplateValue::Block { .. } => Shape::Block,
            _ => Shape::Scalar,
        };
        let mut fields = ir.lookup(schema, key, shape).collect::<Vec<_>>();
        let exact = fields.iter().copied().filter(|id| matches!(ir.matcher(ir.field(*id).key), Matcher::Literal(symbol) if ir.strings().resolve(*symbol).eq_ignore_ascii_case(key))).collect::<Vec<_>>();
        if !exact.is_empty() {
            fields = exact;
        }
        fields.retain(|id| {
            crate::ir_semantic::matcher_matches(
                ir,
                ir.field(*id).key,
                key,
                &crate::ir_semantic::WorkspaceFacts {
                    snapshot: self.snapshot,
                },
            )
        });
        let mut alternatives = Vec::new();
        for id in fields {
            let field = ir.field(id);
            if let Some(kind) = crate::ir_template::template_kind(ir, field.key) {
                if let Some(resolved) = resolve_dynamic_definition(self.snapshot, &kind, key) {
                    alternatives.push(self.contract_of(&resolved));
                }
                continue;
            }
            // Registers and links retarget the body. They constrain their own origin only;
            // statements inside them do not constrain the definition's entry scope.
            if matches!(ir.matcher(field.key), Matcher::Link) {
                if let Some(link) = ir.scopes.links.iter().find(|link| {
                    crate::ir_semantic::matcher_pattern_matches(
                        ir,
                        &link.pattern,
                        key.split('.').next().unwrap_or(key),
                        &crate::ir_semantic::WorkspaceFacts {
                            snapshot: self.snapshot,
                        },
                    )
                }) && !link
                    .from
                    .iter()
                    .any(|scope| matches!(scope, rules::ir::ScopeRef::Any))
                {
                    alternatives.push(ScopeContract::Scopes(
                        link.from
                            .iter()
                            .filter_map(|scope| scope.type_name())
                            .map(|scope| ir.strings().resolve(scope).to_owned())
                            .collect(),
                    ));
                } else {
                    alternatives.push(ScopeContract::Unconstrained);
                }
                continue;
            }
            let mut constraints = Vec::new();
            if let Some(effect) = &field.scope
                && !effect.scopes_in.is_empty()
            {
                constraints.push(ScopeContract::Scopes(
                    effect
                        .scopes_in
                        .iter()
                        .map(|scope| ir.strings().resolve(*scope).to_owned())
                        .collect(),
                ));
            }
            if field
                .scope
                .as_ref()
                .is_none_or(|effect| effect.push.is_none() && effect.set.is_empty())
            {
                let child = match field.value {
                    FieldValue::Block(child) => Some(child),
                    FieldValue::SelfBlock => Some(schema),
                    _ => None,
                };
                if let Some(child) = child
                    && let TemplateValue::Block { items, .. } = &property.value
                {
                    let union = field
                        .control
                        .as_ref()
                        .and_then(|control| control.op)
                        .is_some_and(|op| ir.strings().resolve(op).eq_ignore_ascii_case("or"));
                    let children = items
                        .iter()
                        .map(|item| self.ir_item(child, item))
                        .collect::<Vec<_>>();
                    constraints.push(self.combine_ir_contracts(&children, union));
                }
            }
            alternatives.push(self.combine_ir_contracts(&constraints, false));
        }
        self.combine_ir_contracts(&alternatives, true)
    }
}

fn template_dispatches(items: &[TemplateItem]) -> bool {
    items.iter().any(|item| match item {
        TemplateItem::Recover(_) => true,
        TemplateItem::Conditional(conditional) => template_dispatches(&conditional.items),
        TemplateItem::Property(property) => {
            token_has_parameter(&property.key)
                || match &property.value {
                    TemplateValue::Block { items, .. } => template_dispatches(items),
                    _ => false,
                }
        }
        _ => false,
    })
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
    let mut candidates: Vec<(Arc<str>, String)> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for definition in snapshot.index().definitions_iter() {
        if !dynamic_definition_type(snapshot, &definition.kind) {
            continue;
        }
        if seen.insert((
            definition.kind.to_ascii_lowercase(),
            definition.name.to_ascii_lowercase(),
        )) {
            candidates.push((definition.kind.clone(), definition.name.to_string()));
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
                candidates.push((definition.kind.clone(), definition.name.to_string()));
            }
        }
    }
    let mut inference = ContractInference {
        snapshot,
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
