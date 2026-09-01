//! Editor-neutral inlay-hint queries.
//!
//! Inlay hints are derived from the same lowered scope facts used by diagnostics and completion.
//! Keeping this traversal here prevents the LSP adapter from interpreting EU4 rule transitions
//! and makes the result reusable by other editor frontends.

use pdx_engine::hir::ScopeValue;
use pdx_engine::{AnalysisSnapshot, DocumentId};
use pdx_text::TextRange;

use crate::support::input_for_document;
use crate::types::{CancellationToken, Cancelled, ScopeInlayHint};

/// Upper bound for one visible inlay-hint response. A malformed/generated file can contain a
/// very large number of nested transitions; truncating in analysis keeps protocol payloads and
/// editor work bounded before conversion to LSP values.
pub const MAX_SCOPE_INLAY_HINTS: usize = 200;

/// Computes rule-proven scope transitions for one open document.
///
/// The HIR stores both the ambient state before a property and the state after a statically
/// selected transition. Ambiguous, unknown, placeholder, and same-scope transitions are omitted
/// rather than guessing. `range` is the requested visible byte range; `None` means the whole
/// document.
pub fn scope_inlay_hints_with_cancellation(
    snapshot: &AnalysisSnapshot,
    id: &DocumentId,
    range: Option<TextRange>,
    cancellation: &CancellationToken,
) -> Result<Vec<ScopeInlayHint>, Cancelled> {
    let Some(input) = input_for_document(snapshot, id) else {
        return Ok(Vec::new());
    };
    let Some(hir) = input.hir.as_deref() else {
        return Ok(Vec::new());
    };
    let mut hints = Vec::new();
    for property in hir.properties() {
        cancellation.checkpoint()?;
        let Some(value_range) = property.value_range else {
            continue;
        };
        // A scope transition describes a keyed block. Direct scalar properties can carry a
        // value range too, but they have no nested scope to annotate.
        if property.scalar.is_some() {
            continue;
        }
        let position = value_range.start();
        if range
            .is_some_and(|requested| position < requested.start() || position >= requested.end())
        {
            continue;
        }
        let Some(fact) = hir.scope_fact_at(property.key_range) else {
            continue;
        };
        let Some(transition) = fact.transition.as_ref() else {
            continue;
        };
        let Some(ambient) = concrete_scope(&fact.state.current) else {
            continue;
        };
        let Some(resolved) = concrete_scope(&transition.current) else {
            continue;
        };
        if ambient.eq_ignore_ascii_case(&resolved) || !snapshot.game_profile().is_scope(&resolved) {
            continue;
        }
        hints.push(ScopeInlayHint {
            position,
            scope: resolved,
        });
        if hints.len() >= MAX_SCOPE_INLAY_HINTS {
            break;
        }
    }
    Ok(hints)
}

fn concrete_scope(values: &[ScopeValue]) -> Option<String> {
    let ScopeValue::Known(scopes) = values.first()? else {
        return None;
    };
    (scopes.len() == 1).then(|| scopes[0].to_string())
}
