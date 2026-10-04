//! IDE projections of entry-scope queries over the shared Template program.
//! Guard-dependent requirements stay conditional; actual calls use supplied presence and text.
#[cfg(test)]
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::sync::Arc;

use engine::AnalysisSnapshot;
#[cfg(test)]
use engine::DocumentSource;
use hir::ScopeValue;

use crate::semantic::dynamic_definition_type;
#[cfg(test)]
use crate::semantic::probe_query_cache;
use crate::support::ParsedInput;
use crate::types::{CancellationToken, Cancelled, Diagnostic, DiagnosticCode, uncancelled};

/// Cache key for the workspace-wide contract report inside the query cache.
#[cfg(test)]
const CONTRACT_CACHE_KEY: &str = "template:scope-contracts";

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
#[cfg(test)]
#[derive(Clone, Debug, Default)]
pub(crate) struct TemplateContractReport {
    contracts: BTreeMap<(String, String), ScopeContract>,
    /// Definitions whose template dispatches through a `$param$` key.
    dynamic: BTreeSet<(String, String)>,
    conditions: BTreeMap<(String, String), BTreeSet<String>>,
}

#[cfg(test)]
impl TemplateContractReport {
    pub(crate) fn contract(&self, kind: &str, name: &str) -> Option<&ScopeContract> {
        self.contracts
            .get(&(kind.to_ascii_lowercase(), name.to_ascii_lowercase()))
    }
}

/// Returns definition-site diagnostics for dynamic definitions in `input` whose
/// inferred entry contract is empty.
pub(crate) fn template_contract_diagnostics(
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
    let mut diagnostics = Vec::new();
    for definition in hir.definitions() {
        if !dynamic_definition_type(snapshot, &definition.kind) {
            continue;
        }
        let entry = contract_entry(snapshot, &definition.kind, &definition.name, cancellation)?;
        if !matches!(entry.contract, ScopeContract::Empty) {
            continue;
        }
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
pub(crate) fn template_call_site_diagnostics(
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
        if crate::semantic::dynamic_definition_summary(snapshot, &dynamic_kind, &property.key)
            .is_some_and(|summary| crate::ir_template::needs_body_analysis(snapshot, &summary))
        {
            continue;
        }
        let entry = contract_entry(snapshot, &dynamic_kind, &property.key, cancellation)?;
        // A definition-side guaranteed contradiction already has its located report.
        if matches!(entry.contract, ScopeContract::Empty) {
            continue;
        }
        let scoped = hir::template_scope::check_scope(
            snapshot.ir(),
            &crate::ir_semantic::WorkspaceFacts { snapshot },
            &dynamic_kind,
            &property.key,
            &crate::ir_template::invocation_inputs(hir, property),
            fact.state.clone(),
            &mut || cancellation.checkpoint(),
        )?;
        if scoped.value != hir::analysis::Validation::Invalid {
            continue;
        }
        if !scoped.coverage.is_complete() {
            diagnostics.push(Diagnostic::new(
                DiagnosticCode::AnalysisIncomplete,
                crate::Severity::Information,
                property.key_range,
                scoped.coverage.limit_description(),
            ));
        }
        let inputs = crate::ir_template::invocation_inputs(hir, property);
        let mut expected = Vec::new();
        for scope in &snapshot.ir().scopes.types {
            let scope = snapshot.ir().strings().resolve(*scope);
            let mut state = fact.state.clone();
            state.current = vec![ScopeValue::known_single(scope)];
            let check = hir::template_scope::check_scope(
                snapshot.ir(),
                &crate::ir_semantic::WorkspaceFacts { snapshot },
                &dynamic_kind,
                &property.key,
                &inputs,
                state,
                &mut || cancellation.checkpoint(),
            )?;
            if check.value != hir::analysis::Validation::Invalid {
                expected.push(scope.to_owned());
            }
        }
        let message = if expected.is_empty() {
            format!(
                "dynamic definition `{}` has no valid entry scope for these parameter bindings",
                property.key
            )
        } else {
            format!(
                "dynamic definition `{}` requires entry scope {} but is called in `{}` scope",
                property.key,
                expected.join(", "),
                ambient
            )
        };
        diagnostics.push(Diagnostic::new(
            DiagnosticCode::WrongScope,
            DiagnosticCode::WrongScope.severity(),
            property.key_range,
            message,
        ));
    }
    Ok(diagnostics)
}

/// One definition's shared scope projection, with the caller's cancellation token.
pub(crate) fn template_contract_with_cancellation(
    snapshot: &AnalysisSnapshot,
    kind: &str,
    name: &str,
    cancellation: &CancellationToken,
) -> Result<ScopeContract, Cancelled> {
    Ok(contract_entry(snapshot, kind, name, cancellation)?
        .contract
        .clone())
}

/// Owned audit convenience for one definition's scope projection.
#[cfg(test)]
pub(crate) fn template_contract(
    snapshot: &AnalysisSnapshot,
    kind: &str,
    name: &str,
) -> Option<ScopeContract> {
    let cancellation = CancellationToken::new();
    Some(
        uncancelled(contract_entry(snapshot, kind, name, &cancellation))
            .contract
            .clone(),
    )
}

/// Complete owned audit view; interactive consumers use targeted projections.
#[cfg(test)]
pub(crate) fn template_contract_report_view(
    snapshot: &AnalysisSnapshot,
    cancellation: &CancellationToken,
) -> Result<std::sync::Arc<TemplateContractReport>, Cancelled> {
    template_contract_report(snapshot, cancellation)
}

/// One-line hover summary of a definition's inferred contract.
pub(crate) fn contract_hover_line(snapshot: &AnalysisSnapshot, kind: &str, name: &str) -> String {
    let cancellation = CancellationToken::new();
    let entry = uncancelled(contract_entry(snapshot, kind, name, &cancellation));
    let scope = entry.contract.display();
    let dispatch = if entry.dynamic {
        " (dynamic `$param$` dispatch: not narrowed)"
    } else {
        ""
    };
    let guards = if entry.conditions.is_empty() {
        String::new()
    } else {
        format!(
            "; conditional requirements depend on {}",
            entry
                .conditions
                .iter()
                .map(|name| format!("`{name}`"))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    format!("- Inferred entry scope: {scope}{dispatch}{guards}")
}

#[derive(Clone, Debug)]
struct ContractEntry {
    contract: ScopeContract,
    dynamic: bool,
    conditions: BTreeSet<String>,
}

/// Interactive consumers infer only the definition they consume. The exact
/// snapshot cache and shared environment memo invalidate generated-fact reads.
fn contract_entry(
    snapshot: &AnalysisSnapshot,
    kind: &str,
    name: &str,
    cancellation: &CancellationToken,
) -> Result<Arc<ContractEntry>, Cancelled> {
    cancellation.checkpoint()?;
    let key = format!(
        "template:scope-contract:{}:{}",
        kind.to_ascii_lowercase(),
        name.to_ascii_lowercase()
    );
    if let Some(entry) = snapshot.query_cache().get(snapshot.revision(), &key) {
        return Ok(entry);
    }
    let facts = crate::ir_semantic::WorkspaceFacts { snapshot };
    let checked =
        hir::template_scope::entry_scopes(snapshot.ir(), &facts, kind, name, &mut || {
            cancellation.checkpoint()
        })?;
    let contract = if snapshot.ir().scopes.types.is_empty() {
        ScopeContract::Unknown
    } else if checked.value.possible.is_empty() {
        ScopeContract::Empty
    } else if checked.value.possible.len() == snapshot.ir().scopes.types.len() {
        if checked.coverage.is_complete() {
            ScopeContract::Unconstrained
        } else {
            ScopeContract::Unknown
        }
    } else {
        ScopeContract::Scopes(checked.value.possible.clone())
    };
    let entry = Arc::new(ContractEntry {
        contract,
        dynamic: checked.value.dynamic,
        conditions: checked.value.conditions,
    });
    snapshot.query_cache().insert(
        snapshot.revision(),
        engine::CacheDomain::Documents,
        key,
        entry.clone(),
    );
    Ok(entry)
}

#[cfg(test)]
fn template_contract_report(
    snapshot: &AnalysisSnapshot,
    cancellation: &CancellationToken,
) -> Result<Arc<TemplateContractReport>, Cancelled> {
    let revision = snapshot.revision();
    if let Some(cached) =
        probe_query_cache::<TemplateContractReport>(snapshot, revision, &[CONTRACT_CACHE_KEY])
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
        .is_superseded(engine::CacheDomain::Documents, revision)
    {
        return Ok(Arc::new(TemplateContractReport::default()));
    }
    cancellation.checkpoint()?;
    let report = build_contract_report(snapshot, cancellation)?;
    if std::env::var("PDC_DEBUG_TEMPLATE_CONTRACTS").is_ok_and(|value| !value.is_empty()) {
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
        engine::CacheDomain::Documents,
        CONTRACT_CACHE_KEY.to_owned(),
        report.clone(),
    );
    Ok(report)
}

#[cfg(test)]
fn build_contract_report(
    snapshot: &AnalysisSnapshot,
    cancellation: &CancellationToken,
) -> Result<TemplateContractReport, Cancelled> {
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
    let mut report = TemplateContractReport::default();
    for (kind, name) in candidates {
        let entry = contract_entry(snapshot, &kind, &name, cancellation)?;
        let key = (kind.to_ascii_lowercase(), name.to_ascii_lowercase());
        if entry.dynamic {
            report.dynamic.insert(key.clone());
        }
        report
            .conditions
            .insert(key.clone(), entry.conditions.clone());
        report.contracts.insert(key, entry.contract.clone());
    }
    Ok(report)
}
