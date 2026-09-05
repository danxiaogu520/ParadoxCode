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

use pdx_engine::{AnalysisSnapshot, CacheDomain};
use pdx_text::LogicalPath;

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
    if input.format != pdx_parser::FileFormat::Script
        || !input
            .profile
            .game_id
            .eq_ignore_ascii_case(pdx_game::eu4::GAME_ID)
        || !is_mission_path(input.path.as_ref())
    {
        return Ok(Vec::new());
    }
    let universe = mission_universe(snapshot, cancellation)?;
    let loaded = pdx_game::eu4::mission::parse_file(&input.source);
    // Syntax errors in the file are owned by the main syntax pass; the
    // mission validator only contributes structural findings.
    Ok(
        pdx_game::eu4::mission::validate_with_universe_ids(&loaded.file, &universe.ids)
            .into_iter()
            .filter_map(mission_diagnostic)
            .collect(),
    )
}

/// True when `path` sits inside a `missions/` directory, the EU4 mission-file
/// root (EU4 1.35+ format).
fn is_mission_path(path: Option<&LogicalPath>) -> bool {
    path.and_then(|path| path.as_str().split('/').next())
        .is_some_and(|first| first.eq_ignore_ascii_case("missions"))
}

/// Translates one mission-validator finding into a pipeline diagnostic.
///
/// Returns `None` for findings the rest of the pipeline owns: duplicate ids
/// (the symbol layer's later-wins pass) and any code this mapping has not
/// caught up with — better silent than mis-coded.
fn mission_diagnostic(finding: pdx_game::eu4::mission::Diagnostic) -> Option<Diagnostic> {
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
    cancellation: &CancellationToken,
) -> Result<Arc<MissionUniverse>, Cancelled> {
    cancellation.checkpoint()?;
    let revision = snapshot.revision();
    if let Some(cached) = snapshot
        .query_cache()
        .get::<MissionUniverse>(revision, "mission-universe-ids")
    {
        return Ok(cached);
    }
    let ids = crate::semantic::effective_workspace_member_names(snapshot, "mission")
        .into_iter()
        .collect();
    let universe = Arc::new(MissionUniverse { ids });
    snapshot.query_cache().insert(
        revision,
        CacheDomain::Documents,
        "mission-universe-ids".to_owned(),
        Arc::clone(&universe),
    );
    Ok(universe)
}
