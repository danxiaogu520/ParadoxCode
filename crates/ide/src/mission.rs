//! EU4 mission-tree structural validation merged into the diagnostic pipeline.
//!
//! The mission editor's checks (duplicate ids, dangling or cyclic
//! `required_missions`, illegal prerequisite placement) used to reach users
//! only through the mission-preview webview. This module runs the same
//! validator for every analyzed `missions/` file and maps its findings onto
//! the shared diagnostic codes:
//!
//! - `dangling-required`, `dependency-cycle`, `illegal-edge-placement` →
//!   [`DiagnosticCode::InvalidDependency`];
//! - `zero-position` → [`DiagnosticCode::InvalidValue`].
//!
//! Duplicate id findings are left to the symbol layer: missions and mission
//! trees are indexed definitions (`mission`, `mission_series` kinds), so the
//! workspace-wide later-wins pass already warns at the shadower with full
//! cross-file awareness — re-reporting them here would double-diagnose the
//! same token.

use std::collections::HashSet;
use std::sync::Arc;

use engine::{AnalysisSnapshot, CacheDomain};
use text::LogicalPath;

use crate::support::ParsedInput;
use crate::types::*;

/// Mission-file diagnostics for `input`; empty unless `input` is an EU4
/// mission file.
pub(crate) fn mission_diagnostics(
    snapshot: &AnalysisSnapshot,
    input: &ParsedInput,
    cancellation: &CancellationToken,
) -> Result<Vec<Diagnostic>, Cancelled> {
    cancellation.checkpoint()?;
    let Some(view) = game::mission::view(&input.profile) else {
        return Ok(Vec::new());
    };
    if input.format != parser::FileFormat::Script
        || !input
            .path
            .as_ref()
            .is_some_and(|path| view.matches(path.as_str()))
    {
        return Ok(Vec::new());
    }
    let universe = mission_universe(snapshot, &view.spec.symbol_kind, cancellation)?;
    let loaded = view.parse(&input.source);
    // Syntax errors in the file are owned by the main syntax pass; the
    // mission validator only contributes structural findings.
    Ok(view
        .validate(&loaded.file, &universe.ids)
        .into_iter()
        .filter_map(mission_diagnostic)
        .collect())
}

/// Whether the active package exposes a mission view at this logical path.
pub(crate) fn is_mission_path(profile: &rules::GameProfile, path: Option<&LogicalPath>) -> bool {
    game::mission::view(profile)
        .is_some_and(|view| path.is_some_and(|path| view.matches(path.as_str())))
}

/// Translates one mission-validator finding into a pipeline diagnostic.
///
/// Returns `None` for findings the rest of the pipeline owns: duplicate ids
/// (the symbol layer's later-wins pass) and any code this mapping has not
/// caught up with — better silent than wrong.
fn mission_diagnostic(finding: game::mission::Diagnostic) -> Option<Diagnostic> {
    let (code, severity) = match finding.code {
        "dangling-required" | "dependency-cycle" => {
            (DiagnosticCode::InvalidDependency, Severity::Error)
        }
        // The game still loads and renders stacked or same-row prerequisite
        // layouts, just not cleanly; an illegal edge is a layout warning.
        "illegal-edge-placement" => (DiagnosticCode::InvalidDependency, Severity::Warning),
        "zero-position" => (DiagnosticCode::InvalidValue, Severity::Warning),
        // `duplicate-tree-id`/`duplicate-mission-id` are reported by the
        // symbol layer at the shadower; unknown codes must not be guessed.
        _ => return None,
    };
    Some(Diagnostic::new(
        code,
        severity,
        finding.range,
        finding.message,
    ))
}

/// Mission ids reachable across the workspace.
struct MissionUniverse {
    ids: HashSet<String>,
}

/// Collects the ids of every mission the workspace can see. EU4 1.35+ allows
/// `required_missions` to reference missions defined in other files, so
/// dangling-reference resolution needs this cross-file view. Missions are
/// indexed workspace symbols (kind `mission`), so the member-name view —
/// which already merges the disk index (including cache-backed first-party
/// roots) with open overlays — is exactly the reachable set; reparsing source
/// files here would both duplicate that work and miss files whose text is not
/// resident. The set is cached per revision because it is shared by every
/// mission file analyzed.
fn mission_universe(
    snapshot: &AnalysisSnapshot,
    kind: &str,
    cancellation: &CancellationToken,
) -> Result<Arc<MissionUniverse>, Cancelled> {
    cancellation.checkpoint()?;
    let revision = snapshot.revision();
    let key = format!("mission-universe-ids:{kind}");
    if let Some(cached) = snapshot
        .query_cache()
        .get::<MissionUniverse>(revision, &key)
    {
        return Ok(cached);
    }
    let ids = crate::semantic::effective_workspace_member_names(snapshot, kind)
        .into_iter()
        .collect();
    let universe = Arc::new(MissionUniverse { ids });
    snapshot
        .query_cache()
        .insert(revision, CacheDomain::Documents, key, Arc::clone(&universe));
    Ok(universe)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn declared_view_supports_another_identity_and_missing_capability_disables_it() {
        let mut profile = rules::GameProfile::empty("synthetic");
        let mut spec = game::first_party_ir()
            .unwrap()
            .game
            .profile
            .mission_view
            .clone()
            .unwrap();
        spec.path.pattern = "graphs/".to_owned();
        spec.symbol_kind = "node".to_owned();
        spec.node_fields.required_missions = "after".to_owned();
        profile.mission_view = Some(spec);
        let sources = vec![("graph.json".to_owned(), serde_json::from_str(r#"{"files":{"graph":{"path":"graphs","root":"body"}},"schemas":{"body":{"open":true}},"types":{"node":{}}}"#).unwrap())];
        let source = "tree = { a = { after = { b } } b = { after = { a } } }";
        for (enabled, expected) in [(true, 1), (false, 0)] {
            let mut selected = profile.clone();
            if !enabled {
                selected.mission_view = None;
            }
            let ir = Arc::new(
                rules::lower::lower(
                    &sources,
                    rules::ir::GameConfig {
                        profile: selected.clone(),
                    },
                )
                .unwrap(),
            );
            let host =
                engine::AnalysisHost::with_ir(rules::RuleSet::from_ir_catalog(&ir), selected, ir);
            let snapshot = host.snapshot();
            let path = LogicalPath::parse("graphs/example.txt").unwrap();
            let input = crate::support::input_for_text(&snapshot, &path, source).unwrap();
            let findings =
                mission_diagnostics(&snapshot, &input, &CancellationToken::new()).unwrap();
            assert_eq!(
                findings
                    .iter()
                    .filter(|finding| finding.message.contains("dependency cycle"))
                    .count(),
                expected
            );
        }
    }
}
