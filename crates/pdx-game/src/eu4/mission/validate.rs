//! Structural validation of a mission file.
//!
//! Checks are file-local: duplicate ids, dangling `required_missions` references,
//! dependency cycles, and suspicious layout values. [`validate_in`] additionally
//! resolves dangling references against a universe of other files (the mod's
//! mission files), because cross-file references are legitimate EU4 1.35+;
//! [`validate_with_universe_ids`] accepts the universe as a precomputed id set,
//! which is how the language-server pipeline shares one parse across files.
//! The editor renders these as in-canvas markers and a panel list; it never
//! fixes them silently.
//!
//! Every diagnostic carries the byte range of the offending token, so callers
//! can underline exactly the id, prerequisite, or value at fault.
//!
//! Cross-tree `required_missions` are a legitimate EU4 1.35+ feature (branching
//! missions) and are not diagnosed — but edges whose prerequisite is not on the
//! row directly above or directly above in the same column are flagged as
//! warnings (`illegal-edge-placement`): the game cannot render them cleanly.

use std::collections::{HashMap, HashSet};

use pdx_text::TextRange;

use super::graph;
use super::model::{MissionFile, MissionTree};

/// Severity of a validation diagnostic.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Severity {
    Error,
    Warning,
}

/// One structural problem found in a mission file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Diagnostic {
    pub severity: Severity,
    pub code: &'static str,
    pub message: String,
    /// Byte range of the offending token in the source file.
    pub range: TextRange,
    /// Tree the problem belongs to.
    pub tree: String,
    /// Mission the problem belongs to, when mission-scoped.
    pub mission: Option<String>,
}

/// Validates the whole file and returns diagnostics in a stable order.
#[must_use]
pub fn validate(file: &MissionFile) -> Vec<Diagnostic> {
    validate_in(file, &[])
}

/// Validates `focus` with dangling references resolved against `universe` (the
/// other mission files of the mod). Diagnostics are only reported for `focus`;
/// spatial edge checks are evaluated on `focus` alone because effective
/// positions are file-local.
#[must_use]
pub fn validate_in(focus: &MissionFile, universe: &[&MissionFile]) -> Vec<Diagnostic> {
    let mut universe_ids: HashSet<String> = HashSet::new();
    for file in universe {
        for (id, _) in file.mission_ids() {
            universe_ids.insert((*id).to_owned());
        }
    }
    validate_with_universe_ids(focus, &universe_ids)
}

/// Validates `focus` against a precomputed universe of mission ids (typically
/// every mission id reachable in the workspace, including other files).
#[must_use]
pub fn validate_with_universe_ids(
    focus: &MissionFile,
    universe_ids: &HashSet<String>,
) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    let ids = focus.mission_ids();

    // Duplicate tree ids. The editor cannot keep two same-named trees apart, so
    // the shadower is called out; the game keeps loading the file.
    let mut seen_trees = HashSet::new();
    for tree in &focus.trees {
        if !seen_trees.insert(&tree.id) {
            diagnostics.push(tree_scoped(
                Severity::Warning,
                "duplicate-tree-id",
                tree,
                format!("duplicate mission tree `{}`", tree.id),
            ));
        }
    }

    // Per-tree checks.
    for tree in &focus.trees {
        let mut seen = HashSet::new();
        for mission in &tree.missions {
            if !seen.insert(&mission.id) {
                diagnostics.push(mission_scoped(
                    Severity::Warning,
                    "duplicate-mission-id",
                    tree,
                    mission,
                    mission.id_range,
                    format!("duplicate mission id `{}`", mission.id),
                ));
            }
            if mission.position == Some(0) {
                diagnostics.push(mission_scoped(
                    Severity::Warning,
                    "zero-position",
                    tree,
                    mission,
                    mission.position_range.unwrap_or(mission.id_range),
                    format!(
                        "mission `{}` has position = 0; game columns start at 1",
                        mission.id
                    ),
                ));
            }
            for (index, required) in mission.required.iter().enumerate() {
                if !ids.contains_key(required.as_str()) && !universe_ids.contains(required) {
                    diagnostics.push(mission_scoped(
                        Severity::Error,
                        "dangling-required",
                        tree,
                        mission,
                        mission
                            .required_ranges
                            .get(index)
                            .copied()
                            .unwrap_or(mission.id_range),
                        format!(
                            "mission `{}` requires unknown mission `{}`",
                            mission.id, required
                        ),
                    ));
                }
            }
        }
    }

    find_cycles(focus, &mut diagnostics);

    // Spatial edge legality: a prerequisite must sit on the row directly above
    // its dependent or directly above it in the same column. Vanilla files use
    // stacked/same-row layouts here and there and the game renders them, so
    // this is a warning, never an error.
    for violation in graph::spatial_violations(focus) {
        let tree = &focus.trees[violation.tree];
        let mission = &tree.missions[violation.mission];
        diagnostics.push(mission_scoped(
            Severity::Warning,
            "illegal-edge-placement",
            tree,
            mission,
            mission
                .required_ranges
                .get(violation.required_index)
                .copied()
                .unwrap_or(mission.id_range),
            format!(
                "mission `{}` requires `{}`, but the prerequisite is not on the row \
                 directly above or directly above in the same column",
                mission.id, violation.required
            ),
        ));
    }

    diagnostics
}

/// File-wide cycle detection over the full mission graph (cross-tree edges count).
fn find_cycles(file: &MissionFile, diagnostics: &mut Vec<Diagnostic>) {
    let ids = file.mission_ids();
    // mission id -> prerequisite ids that exist somewhere in the file.
    let mut index: HashMap<&str, Vec<&str>> = HashMap::new();
    for (mission_id, (_, mission)) in &ids {
        for required in &mission.required {
            if ids.contains_key(required.as_str()) {
                index.entry(mission_id).or_default().push(required);
            }
        }
    }

    #[derive(Clone, Copy, PartialEq)]
    enum Mark {
        Visiting,
        Done,
    }
    let mut marks: HashMap<&str, Mark> = HashMap::new();
    let mut stack: Vec<&str> = Vec::new();

    fn visit<'a>(
        node: &'a str,
        index: &HashMap<&'a str, Vec<&'a str>>,
        marks: &mut HashMap<&'a str, Mark>,
        stack: &mut Vec<&'a str>,
        diagnostics: &mut Vec<Diagnostic>,
        mission_of: &HashMap<&str, (&MissionTree, &super::model::Mission)>,
    ) {
        match marks.get(node) {
            Some(Mark::Done) => return,
            Some(Mark::Visiting) => {
                let start = stack.iter().position(|n| *n == node).unwrap_or(0);
                let cycle: Vec<&str> = stack[start..].to_vec();
                let mut message = format!("mission dependency cycle: {}", cycle.join(" -> "));
                // Close the loop on the mission the cycle re-enters.
                message.push_str(&format!(" -> {node}"));
                let (tree, mission) = mission_of.get(node).copied().expect("node is in the graph");
                diagnostics.push(Diagnostic {
                    severity: Severity::Error,
                    code: "dependency-cycle",
                    message,
                    range: mission.id_range,
                    tree: tree.id.clone(),
                    mission: Some(mission.id.clone()),
                });
                return;
            }
            None => {}
        }
        marks.insert(node, Mark::Visiting);
        stack.push(node);
        if let Some(nexts) = index.get(node) {
            for next in nexts {
                visit(next, index, marks, stack, diagnostics, mission_of);
            }
        }
        stack.pop();
        marks.insert(node, Mark::Done);
    }

    let mission_of: HashMap<&str, (&MissionTree, &super::model::Mission)> =
        file.mission_ids().into_iter().collect();
    // Visit in file order so which cycle member carries the diagnostic is
    // stable across runs (HashMap iteration is not).
    let all_ids: Vec<&str> = file
        .trees
        .iter()
        .flat_map(|tree| tree.missions.iter())
        .map(|mission| mission.id.as_str())
        .collect();
    for id in all_ids {
        visit(id, &index, &mut marks, &mut stack, diagnostics, &mission_of);
    }
}

fn tree_scoped(
    severity: Severity,
    code: &'static str,
    tree: &MissionTree,
    message: String,
) -> Diagnostic {
    Diagnostic {
        severity,
        code,
        message,
        range: tree.id_range,
        tree: tree.id.clone(),
        mission: None,
    }
}

fn mission_scoped(
    severity: Severity,
    code: &'static str,
    tree: &MissionTree,
    mission: &super::model::Mission,
    range: TextRange,
    message: String,
) -> Diagnostic {
    Diagnostic {
        severity,
        code,
        message,
        range,
        tree: tree.id.clone(),
        mission: Some(mission.id.clone()),
    }
}
