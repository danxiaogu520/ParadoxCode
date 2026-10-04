//! Transaction-local fact discovery. Every round reads one immutable index;
//! only readers of changed positive or negative lookups enter the next round.
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use rules::ir::{RulesIr, Symbol, SymbolFacts, TypeId};
use vfs::{SourceFile, SourceFileId, WorkspaceError};

use crate::{FileIndexShard, FileState, IndexSymbolFacts, SourceLoadContext, WorkspaceIndex};

/// A lookup read by a file, including absent and ambiguous members. A missing
/// name denotes a namespace-wide dependency (for generated name patterns).
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct SymbolDependency {
    pub kind: String,
    pub name: Option<String>,
}
impl SymbolDependency {
    fn named(ir: &RulesIr, ty: TypeId, name: &str) -> Self {
        Self {
            kind: ir
                .strings
                .resolve(ir.type_info(ty).name)
                .to_ascii_lowercase(),
            name: Some(name.to_ascii_lowercase()),
        }
    }
}

pub(crate) struct TrackingFacts<'a> {
    pub(crate) ir: &'a RulesIr,
    pub(crate) facts: &'a (dyn SymbolFacts + Sync),
    pub(crate) reads: Mutex<BTreeSet<SymbolDependency>>,
}
impl TrackingFacts<'_> {
    fn read(&self, ty: TypeId, name: &str) {
        self.reads
            .lock()
            .expect("file-local dependency lock poisoned")
            .insert(SymbolDependency::named(self.ir, ty, name));
    }
}
impl SymbolFacts for TrackingFacts<'_> {
    fn facts_complete(&self) -> bool {
        self.facts.facts_complete()
    }
    // Reusing a memo without replaying its read set would lose dependencies.
    fn replacement_template(
        &self,
        ty: TypeId,
        name: &str,
    ) -> Option<Arc<rules::replacement::Template>> {
        self.read(ty, name);
        self.facts.replacement_template(ty, name)
    }
    fn type_member(&self, ty: TypeId, name: &str) -> bool {
        self.read(ty, name);
        self.facts.type_member(ty, name)
    }
    fn type_subtype_member(&self, ty: TypeId, subtype: Symbol, name: &str) -> bool {
        self.read(ty, name);
        self.facts.type_subtype_member(ty, subtype, name)
    }
    fn asset_member(&self, category: &str, name: &str) -> Option<bool> {
        self.facts.asset_member(category, name)
    }
}

fn changed_dependencies(old: &FileIndexShard, new: &FileIndexShard) -> BTreeSet<SymbolDependency> {
    let mut changed = BTreeSet::new();
    let mut add = |kind: &str, name: &str| {
        changed.insert(SymbolDependency {
            kind: kind.to_ascii_lowercase(),
            name: (!name.contains('$')).then(|| name.to_ascii_lowercase()),
        });
    };
    for shard in [old, new] {
        for def in &shard.definitions {
            add(&def.kind, &def.name);
        }
        for def in &shard.dynamic_definitions {
            add(&def.kind, &def.name);
        }
        for attrs in &shard.definition_attributes {
            add(&attrs.kind, &attrs.name);
        }
        for write in &shard.flag_writes {
            add(&write.kind, &write.name);
        }
    }
    changed
}

/// Conservative transitive reader component of a disk edit. Include outgoing
/// generated facts before rebuilding, so cycles cannot keep a deleted seed alive.
pub fn affected_fact_readers(
    states: &BTreeMap<SourceFileId, Arc<FileState>>,
    before: &WorkspaceIndex,
    after: &WorkspaceIndex,
    edited: &BTreeSet<SourceFileId>,
) -> BTreeSet<SourceFileId> {
    let mut affected = edited.clone();
    let mut changed = BTreeSet::new();
    let empty = |id| FileIndexShard {
        file_id: id,
        definitions: Vec::new(),
        references: Vec::new(),
        dynamic_definitions: Vec::new(),
        definition_attributes: Vec::new(),
        flag_writes: Vec::new(),
        reference_coverage_known: true,
        syntax_error_count: 0,
    };
    loop {
        for id in &affected {
            let vacant = empty(*id);
            changed.extend(changed_dependencies(
                before.shards.get(id).map_or(&vacant, |s| s.as_ref()),
                after.shards.get(id).map_or(&vacant, |s| s.as_ref()),
            ));
        }
        let next = readers_of(states, &changed)
            .difference(&affected)
            .copied()
            .collect::<Vec<_>>();
        if next.is_empty() {
            return affected;
        }
        affected.extend(next);
    }
}

fn readers_of(
    states: &BTreeMap<SourceFileId, Arc<FileState>>,
    changed: &BTreeSet<SymbolDependency>,
) -> BTreeSet<SourceFileId> {
    let kinds = changed
        .iter()
        .map(|dependency| dependency.kind.as_str())
        .collect::<BTreeSet<_>>();
    let wildcard = changed
        .iter()
        .filter(|dependency| dependency.name.is_none())
        .map(|dependency| dependency.kind.as_str())
        .collect::<BTreeSet<_>>();
    states
        .iter()
        .filter(|(_, state)| {
            state.fact_dependencies.iter().any(|read| {
                if read.name.is_none() {
                    kinds.contains(read.kind.as_str())
                } else {
                    wildcard.contains(read.kind.as_str()) || changed.contains(read)
                }
            })
        })
        .map(|(id, _)| *id)
        .collect()
}

fn same_facts(
    left: &BTreeMap<SourceFileId, Arc<FileIndexShard>>,
    right: &BTreeMap<SourceFileId, Arc<FileIndexShard>>,
) -> bool {
    left.len() == right.len()
        && left.iter().all(|(id, shard)| {
            right
                .get(id)
                .is_some_and(|other| Arc::ptr_eq(shard, other) || shard.same_symbol_facts(other))
        })
}
fn retained_bytes(shard: &FileIndexShard) -> usize {
    // Includes references because histories hold shared shards, even though
    // references do not participate in convergence.
    shard
        .definitions
        .len()
        .saturating_mul(std::mem::size_of::<crate::Definition>())
        .saturating_add(
            shard
                .references
                .len()
                .saturating_mul(std::mem::size_of::<crate::Reference>()),
        )
        .saturating_add(
            shard
                .definitions
                .iter()
                .map(|d| d.name.len() + d.kind.len())
                .sum::<usize>(),
        )
        .saturating_add(
            shard
                .references
                .iter()
                .map(|r| r.name.len() + r.kind.len())
                .sum::<usize>(),
        )
        .saturating_add(
            shard
                .dynamic_definitions
                .iter()
                .map(|d| {
                    d.template
                        .as_ref()
                        .map_or(0, |t| t.source.len().saturating_mul(16))
                })
                .sum::<usize>(),
        )
        .saturating_add(shard.definition_attributes.len().saturating_mul(256))
        .saturating_add(
            shard
                .flag_writes
                .iter()
                .map(|w| w.name.len() + w.kind.len() + std::mem::size_of::<crate::FlagWrite>())
                .sum::<usize>(),
        )
}

/// Measured discovery work; a successful return certifies stability. Callers
/// publish the candidate states/index together, never a partially solved round.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FactStabilizationReport {
    pub rounds: usize,
    pub lowered_files: usize,
}

/// Starts from base-lowered states, so obsolete generated facts cannot support
/// each other after removal of their original source. All writes are local to
/// the caller's transaction. Cancellation, oscillation and budgets return errors.
#[allow(clippy::too_many_arguments)]
pub fn stabilize_symbol_dependent_files(
    files: &BTreeMap<SourceFileId, SourceFile>,
    states: &mut BTreeMap<SourceFileId, Arc<FileState>>,
    index: &mut WorkspaceIndex,
    ir: &RulesIr,
    overlays: &[Arc<hir::HirFile>],
    excluded: &BTreeSet<SourceFileId>,
    priorities: &BTreeMap<SourceFileId, u64>,
    context: &SourceLoadContext<'_>,
    initial: Option<&BTreeSet<SourceFileId>>,
) -> Result<FactStabilizationReport, WorkspaceError> {
    let mut pending = states
        .iter()
        .filter(|(id, s)| s.symbol_facts_dependency && initial.is_none_or(|ids| ids.contains(id)))
        .map(|(id, _)| *id)
        .collect::<BTreeSet<_>>();
    let mut history = vec![index.shards.clone()];
    let mut history_bytes = index.shards.len().saturating_mul(64);
    let mut report = FactStabilizationReport::default();
    while !pending.is_empty() {
        context.cancellation.checkpoint()?;
        if report.rounds >= 4096
            || report.lowered_files.saturating_add(pending.len()) > 1_000_000
            || history_bytes > 256 * 1024 * 1024
        {
            return Err(unfinished(
                "generated fact discovery exhausted its work or history budget",
            ));
        }
        let facts = IndexSymbolFacts::with_overlay_files(ir, index, overlays, excluded);
        let rebuilt =
            crate::pipeline::relower_fact_readers(files, states, ir, &facts, &pending, context)?;
        drop(facts);
        report.rounds += 1;
        report.lowered_files += rebuilt.len();
        let mut changed = BTreeSet::new();
        let mut shards = Vec::new();
        for (id, state) in rebuilt {
            context.cancellation.checkpoint()?;
            if let Some(old) = states.get(&id)
                && !state.shard().same_symbol_facts(old.shard())
            {
                changed.extend(changed_dependencies(old.shard(), state.shard()));
                history_bytes = history_bytes.saturating_add(retained_bytes(state.shard()));
            }
            shards.push(state.shard_handle());
            states.insert(id, state);
        }
        index.replace_replayed_shards_cancellable(shards, priorities, context.cancellation)?;
        if changed.is_empty() {
            break;
        }
        if history
            .iter()
            .any(|previous| same_facts(previous, &index.shards))
        {
            return Err(unfinished(
                "generated fact discovery oscillated between previously observed states",
            ));
        }
        pending = readers_of(states, &changed);
        if !pending.is_empty() {
            history_bytes = history_bytes.saturating_add(index.shards.len().saturating_mul(64));
            history.push(index.shards.clone());
        }
    }
    Ok(report)
}
fn unfinished(message: &str) -> WorkspaceError {
    WorkspaceError::Io(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        message,
    ))
}
