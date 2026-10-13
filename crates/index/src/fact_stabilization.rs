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
    #[cfg(test)]
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
    pub(crate) reads:
        Mutex<rustc_hash::FxHashMap<TypeId, rustc_hash::FxHashMap<String, Option<bool>>>>,
}
impl TrackingFacts<'_> {
    fn read(&self, ty: TypeId, name: &str) {
        let folded = if name.bytes().any(|byte| byte.is_ascii_uppercase()) {
            std::borrow::Cow::Owned(name.to_ascii_lowercase())
        } else {
            std::borrow::Cow::Borrowed(name)
        };
        let mut reads = self
            .reads
            .lock()
            .expect("file-local dependency lock poisoned");
        let names = reads.entry(ty).or_default();
        if !names.contains_key(folded.as_ref()) {
            names.insert(folded.into_owned(), None);
        }
    }

    pub(crate) fn into_reads(self) -> BTreeSet<SymbolDependency> {
        self.reads
            .into_inner()
            .expect("file-local dependency lock poisoned")
            .into_iter()
            .flat_map(|(ty, names)| {
                let kind = self
                    .ir
                    .strings
                    .resolve(self.ir.type_info(ty).name)
                    .to_ascii_lowercase();
                names.into_keys().map(move |name| SymbolDependency {
                    kind: kind.clone(),
                    name: Some(name),
                })
            })
            .collect()
    }
}
impl SymbolFacts for TrackingFacts<'_> {
    fn template_accepts_no_arguments(
        &self,
        type_id: TypeId,
        name: &str,
        checkpoint: &mut dyn FnMut() -> bool,
    ) -> Option<bool> {
        // Do not forward the underlying cached answer: every nested Template
        // read must pass through this transaction's dependency recorder.
        crate::pipeline::template_accepts_no_arguments(self.ir, self, type_id, name, checkpoint)
    }

    fn facts_complete(&self) -> bool {
        self.facts.facts_complete()
    }
    // Reusing a memo without replaying its read set would lose dependencies.
    fn template(&self, ty: TypeId, name: &str) -> Option<Arc<rules::template::Template>> {
        self.read(ty, name);
        self.facts.template(ty, name)
    }
    fn type_member(&self, ty: TypeId, name: &str) -> bool {
        let folded = if name.bytes().any(|byte| byte.is_ascii_uppercase()) {
            std::borrow::Cow::Owned(name.to_ascii_lowercase())
        } else {
            std::borrow::Cow::Borrowed(name)
        };
        {
            let reads = self
                .reads
                .lock()
                .expect("file-local dependency lock poisoned");
            if let Some(Some(answer)) = reads.get(&ty).and_then(|names| names.get(folded.as_ref()))
            {
                return *answer;
            }
        }
        // This adapter belongs to one file and one immutable discovery round.
        // Retain negative answers too: a repeated miss still records a dependency,
        // but need not repeat the workspace's namespace/pattern search.
        let answer = self.facts.type_member(ty, name);
        self.reads
            .lock()
            .expect("file-local dependency lock poisoned")
            .entry(ty)
            .or_default()
            .insert(folded.into_owned(), Some(answer));
        answer
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

#[cfg(test)]
mod callable_dependency_tests {
    use super::*;
    use rules::template::{
        Template, TemplateFragment, TemplateItem, TemplateProgram, TemplateProperty, TemplateToken,
        TemplateValue,
    };
    use text::TextRange;

    #[test]
    fn member_reuse_keeps_negative_reads_and_expires_with_the_fact_round() {
        struct Members {
            present: TypeId,
            calls: std::sync::atomic::AtomicUsize,
            visible: bool,
        }
        impl SymbolFacts for Members {
            fn type_member(&self, ty: TypeId, name: &str) -> bool {
                self.calls
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                self.visible && ty == self.present && name.eq_ignore_ascii_case("known")
            }
        }
        let ir = rules::lower::lower(
            &[(
                "members.json".to_owned(),
                serde_json::from_value(serde_json::json!({"types":{"one":{},"two":{}}})).unwrap(),
            )],
            Default::default(),
        )
        .unwrap();
        let one = ir.type_by_name("one").unwrap();
        let two = ir.type_by_name("two").unwrap();
        let members = Members {
            present: one,
            calls: Default::default(),
            visible: true,
        };
        let tracked = TrackingFacts {
            ir: &ir,
            facts: &members,
            reads: Default::default(),
        };
        for _ in 0..4 {
            assert!(tracked.type_member(one, "KNOWN"));
            assert!(tracked.type_member(one, "known"));
            assert!(!tracked.type_member(one, "missing"));
            assert!(!tracked.type_member(two, "known"));
            // A different fact capability must retain, rather than erase, the
            // cached member answer and its dependency.
            tracked.template(one, "known");
        }
        assert_eq!(members.calls.load(std::sync::atomic::Ordering::Relaxed), 3);
        assert_eq!(
            tracked.into_reads(),
            BTreeSet::from([
                SymbolDependency::named(&ir, one, "known"),
                SymbolDependency::named(&ir, one, "missing"),
                SymbolDependency::named(&ir, two, "known"),
            ])
        );
        let next = Members {
            visible: false,
            ..members
        };
        let tracked = TrackingFacts {
            ir: &ir,
            facts: &next,
            reads: Default::default(),
        };
        assert!(!tracked.type_member(one, "known"));
        assert_eq!(next.calls.load(std::sync::atomic::Ordering::Relaxed), 4);
        assert!(
            tracked
                .into_reads()
                .contains(&SymbolDependency::named(&ir, one, "known"))
        );
    }

    struct Definitions(BTreeMap<String, Arc<Template>>);
    impl SymbolFacts for Definitions {
        fn type_member(&self, _: TypeId, name: &str) -> bool {
            self.0.contains_key(name)
        }
        fn template(&self, _: TypeId, name: &str) -> Option<Arc<Template>> {
            self.0.get(name).cloned()
        }
        fn template_accepts_no_arguments(
            &self,
            _: TypeId,
            _: &str,
            _: &mut dyn FnMut() -> bool,
        ) -> Option<bool> {
            panic!("tracking must analyze with its own read recorder, not forward a cached answer")
        }
    }

    fn definition(name: &str, key: &str) -> Arc<Template> {
        let token = |text: &str| TemplateToken {
            range: TextRange::empty(0),
            quoted: false,
            fragments: vec![TemplateFragment::Literal(text.to_owned())],
        };
        let items = vec![TemplateItem::Property(TemplateProperty {
            range: TextRange::empty(0),
            key: token(key),
            operator: Some(Arc::from("=")),
            value: TemplateValue::Scalar(token("yes")),
        })];
        Arc::new(Template {
            source: Arc::from(""),
            kind: Arc::from("helper"),
            name: name.to_owned(),
            definition_range: TextRange::empty(0),
            body_range: TextRange::empty(0),
            program: Arc::new(TemplateProgram::compile(&items)),
            items: Arc::from(items),
        })
    }

    #[test]
    fn callable_eligibility_records_transitive_template_dependencies() {
        let source = serde_json::from_str(
            r#"{
          "files":{"test":{"path":"test","root":"body"}},
          "traits":{"Template":{}},
          "types":{"helper":{"impl":{"Template":{"body":"body"}}}},
          "schemas":{"body":{
            "fields":{"always":{"value":"bool","card":"0..*"}},
            "patterns":[{"key":"ref<helper>","value":"bool","card":"0..*"}]
          }}
        }"#,
        )
        .unwrap();
        let ir =
            rules::lower::lower(&[("query.json".to_owned(), source)], Default::default()).unwrap();
        let definitions = Definitions(BTreeMap::from([
            ("outer".to_owned(), definition("outer", "inner")),
            ("inner".to_owned(), definition("inner", "always")),
        ]));
        let facts = TrackingFacts {
            ir: &ir,
            facts: &definitions,
            reads: Default::default(),
        };
        let kind = ir.type_by_name("helper").unwrap();
        assert_eq!(
            facts.template_accepts_no_arguments(kind, "outer", &mut || false),
            Some(true)
        );
        let reads = facts.into_reads();
        assert!(reads.contains(&SymbolDependency::named(&ir, kind, "outer")));
        assert!(reads.contains(&SymbolDependency::named(&ir, kind, "inner")));
    }
}
