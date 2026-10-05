//! Definition-side projection of shared Template traversal coverage.
//! A repeated binding/scope state is unfinished analysis, not a game-runtime assertion.
use crate::semantic::dynamic_definition_type;
use crate::support::ParsedInput;
use crate::{CancellationToken, Cancelled, Diagnostic, DiagnosticCertainty, DiagnosticCode};
use engine::AnalysisSnapshot;
use hir::analysis::AnalysisLimit;
use std::{collections::BTreeMap, sync::Arc};

pub(crate) fn template_recursion_diagnostics(
    snapshot: &AnalysisSnapshot,
    input: &ParsedInput,
    cancellation: &CancellationToken,
) -> Result<Vec<Diagnostic>, Cancelled> {
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
    for definition in hir
        .definitions()
        .iter()
        .filter(|definition| dynamic_definition_type(snapshot, &definition.kind))
    {
        if let Some(message) =
            cycle_message(snapshot, &definition.kind, &definition.name, cancellation)?
        {
            diagnostics.push(
                Diagnostic::new(
                    DiagnosticCode::DynamicDefinitionCycle,
                    crate::Severity::Information,
                    definition.selection_range,
                    message,
                )
                .with_certainty(DiagnosticCertainty::Unresolved),
            );
        }
    }
    Ok(diagnostics)
}

fn cycle_message(
    snapshot: &AnalysisSnapshot,
    kind: &str,
    name: &str,
    cancellation: &CancellationToken,
) -> Result<Option<String>, Cancelled> {
    cancellation.checkpoint()?;
    let key = format!(
        "template:recursion-coverage:{}:{}",
        kind.to_ascii_lowercase(),
        name.to_ascii_lowercase()
    );
    if let Some(message) = snapshot
        .query_cache()
        .get::<Option<String>>(snapshot.revision(), &key)
    {
        return Ok((*message).clone());
    }
    let analysed = crate::ir_template::missing_parameters(
        snapshot,
        kind,
        name,
        &BTreeMap::new(),
        hir::ScopeState {
            root: hir::ScopeValue::Unknown,
            current: vec![hir::ScopeValue::Unknown],
            previous: Vec::new(),
            from: Vec::new(),
        },
        cancellation,
    )?;
    let message = analysed.coverage.limits.contains(&AnalysisLimit::RecursiveState).then(||
        format!("Template `{name}` analysis repeats a binding/scope state with no arguments; expansion remains unknown"));
    snapshot.query_cache().insert(
        snapshot.revision(),
        engine::CacheDomain::Documents,
        key,
        Arc::new(message.clone()),
    );
    Ok(message)
}
