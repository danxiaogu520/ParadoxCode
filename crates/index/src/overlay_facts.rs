//! Overlay fact transactions. Rounds share immutable facts; a failed discovery
//! publishes base syntax with an explicit frontier, never speculative declarations.
use crate::fact_stabilization::TrackingFacts;
use crate::{DocumentSnapshot, IndexSymbolFacts, SymbolDependency, WorkspaceIndex};
use hir::{
    HirFile,
    analysis::{AnalysisCoverage, AnalysisLimit},
};
use rules::{GameProfile, RuleSet, ir::RulesIr};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use vfs::{DocumentId, DocumentSource, SourceFileId, SourceRoot, WorkspaceScanToken};

#[derive(Clone, Copy, Debug)]
pub struct OverlayFactBudget {
    pub rounds: usize,
    pub lowers: usize,
    pub history_bytes: usize,
}
impl Default for OverlayFactBudget {
    fn default() -> Self {
        Self {
            rounds: 4096,
            lowers: 100_000,
            history_bytes: 128 * 1024 * 1024,
        }
    }
}
#[derive(Clone, Debug, Default)]
pub struct OverlayFactReport {
    pub rounds: usize,
    pub lowers: usize,
    pub coverage: AnalysisCoverage,
}
#[derive(Clone, Debug)]
pub struct OverlayFactTransaction {
    pub documents: BTreeMap<DocumentId, DocumentSnapshot>,
    pub report: OverlayFactReport,
}

fn same_facts(a: &HirFile, b: &HirFile) -> bool {
    a.definitions() == b.definitions()
        && a.dynamic_templates() == b.dynamic_templates()
        && a.definition_attributes() == b.definition_attributes()
}
fn repeated(
    a: &BTreeMap<DocumentId, Arc<HirFile>>,
    b: &BTreeMap<DocumentId, Arc<HirFile>>,
) -> bool {
    a.len() == b.len()
        && a.iter().all(|(id, hir)| {
            b.get(id)
                .is_some_and(|other| Arc::ptr_eq(hir, other) || same_facts(hir, other))
        })
}
fn state(documents: &BTreeMap<DocumentId, DocumentSnapshot>) -> BTreeMap<DocumentId, Arc<HirFile>> {
    documents
        .iter()
        .filter(|(_, doc)| doc.source == DocumentSource::Overlay)
        .filter_map(|(id, doc)| doc.hir.as_ref().map(|hir| (id.clone(), hir.clone())))
        .collect()
}
fn changes(a: &HirFile, b: &HirFile) -> BTreeSet<SymbolDependency> {
    let mut result = BTreeSet::new();
    for hir in [a, b] {
        for def in hir.definitions() {
            result.insert(SymbolDependency {
                kind: def.kind.to_ascii_lowercase(),
                name: (!def.name.contains('$')).then(|| def.name.to_ascii_lowercase()),
            });
        }
        for attrs in hir.definition_attributes() {
            result.insert(SymbolDependency {
                kind: attrs.kind.to_ascii_lowercase(),
                name: (!attrs.name.contains('$')).then(|| attrs.name.to_ascii_lowercase()),
            });
        }
    }
    result
}
fn readers(
    documents: &BTreeMap<DocumentId, DocumentSnapshot>,
    changed: &BTreeSet<SymbolDependency>,
) -> BTreeSet<DocumentId> {
    let kinds = changed
        .iter()
        .map(|read| read.kind.as_str())
        .collect::<BTreeSet<_>>();
    let wildcard = changed
        .iter()
        .filter(|read| read.name.is_none())
        .map(|read| read.kind.as_str())
        .collect::<BTreeSet<_>>();
    documents
        .iter()
        .filter(|(_, doc)| {
            doc.fact_dependencies.iter().any(|read| {
                if read.name.is_none() {
                    kinds.contains(read.kind.as_str())
                } else {
                    wildcard.contains(read.kind.as_str()) || changed.contains(read)
                }
            })
        })
        .map(|(id, _)| id.clone())
        .collect()
}

fn reads_symbols(ir: &RulesIr, matcher: rules::ir::MatcherId) -> bool {
    reads_symbols_inner(ir, matcher, &mut BTreeSet::new())
}

fn reads_symbols_inner(
    ir: &RulesIr,
    matcher: rules::ir::MatcherId,
    visited: &mut BTreeSet<rules::ir::MatcherId>,
) -> bool {
    // Dependency enumeration is deliberately broader than a query's selected
    // runtime path. Legal fixed-selector self queries can revisit this matcher.
    if !visited.insert(matcher) {
        return false;
    }
    match ir.matcher(matcher) {
        rules::ir::Matcher::Ref(_) => true,
        rules::ir::Matcher::Query(query) => ir.fields(query.schema).into_iter()
            .filter(|field| query.shape.is_none_or(|shape| ir.shape(*field) == Some(shape)))
            .any(|field| {
                // Dispatch reads source keys before metadata filters: even an
                // excluded dynamic key can shadow a later accepted pattern.
                reads_symbols_inner(ir, ir.field(field).key, visited)
                    || ir.query_field_passes(query, field)
                        && ir.query_projected_matcher(query, field)
                            .is_some_and(|projected| reads_symbols_inner(ir, projected, visited))
            }),
        rules::ir::Matcher::Union(parts) => parts.iter().any(|part| reads_symbols_inner(ir, *part, visited)),
        rules::ir::Matcher::Pattern(parts) => parts.iter().any(
            |part| matches!(part,rules::ir::PatternPart::Hole(hole) if reads_symbols_inner(ir,*hole,visited)),
        ),
        _ => false,
    }
}
fn discard_conditional_base_facts(ir: &RulesIr, hir: &mut HirFile) {
    let ranges=hir.properties().iter().filter_map(|property| {
        let fact=hir.field_fact_at(property.key_range)?;
        let shape=if property.scalar.is_some(){rules::ir::Shape::Scalar}else{rules::ir::Shape::Block};
        let candidates=ir.lookup(fact.schema,&property.key,shape).collect::<Vec<_>>();
        let exact=candidates.iter().any(|id|matches!(ir.matcher(ir.field(*id).key),rules::ir::Matcher::Literal(key) if ir.strings.resolve(*key).eq_ignore_ascii_case(&property.key)));
        ((!exact && candidates.iter().any(|id|reads_symbols(ir,ir.field(*id).key)))
            || candidates.len()>1 && candidates.iter().any(|id|matches!(ir.field(*id).value,rules::ir::FieldValue::Scalar(matcher) if reads_symbols(ir,matcher))))
            .then_some(property.range)
    }).collect::<Vec<_>>();
    hir.discard_facts_in_ranges(&ranges);
}

/// Rebuilds symbol-dependent overlays from base declarations. Pure documents
/// reuse their worker-prepared frontend. No disk/index mutation occurs here.
#[allow(clippy::too_many_arguments)]
pub fn stabilize_overlay_facts(
    documents: &BTreeMap<DocumentId, DocumentSnapshot>,
    rules: &RuleSet,
    profile: &GameProfile,
    ir: &RulesIr,
    roots: &[SourceRoot],
    index: &WorkspaceIndex,
    excluded: &BTreeSet<SourceFileId>,
    budget: OverlayFactBudget,
    cancellation: &WorkspaceScanToken,
) -> OverlayFactTransaction {
    let mut base = documents.clone();
    let mut pending = BTreeSet::new();
    let mut report = OverlayFactReport::default();
    let mut failed = false;
    for (id, doc) in &mut base {
        if cancellation.checkpoint().is_err() {
            failed = true;
            break;
        }
        doc.fact_coverage = AnalysisCoverage::default();
        if doc
            .hir
            .as_ref()
            .is_some_and(|hir| hir.depends_on_symbol_facts())
        {
            if report.lowers >= budget.lowers {
                failed = true;
                break;
            }
            *doc = crate::pipeline::relower_document_snapshot(
                rules,
                profile,
                ir,
                roots,
                doc.clone(),
                None,
            );
            doc.fact_dependencies.clear();
            pending.insert(id.clone());
            report.lowers += 1;
        }
    }
    let mut candidate = base.clone();
    let mut history = vec![state(&candidate)];
    let mut history_bytes = history[0].len().saturating_mul(64);
    while !failed && !pending.is_empty() {
        if cancellation.checkpoint().is_err()
            || report.rounds >= budget.rounds
            || report.lowers.saturating_add(pending.len()) > budget.lowers
            || history_bytes > budget.history_bytes
        {
            failed = true;
            break;
        }
        let overlays = candidate
            .values()
            .filter(|doc| doc.source == DocumentSource::Overlay)
            .filter_map(|doc| doc.hir.clone())
            .collect::<Vec<_>>();
        let facts = IndexSymbolFacts::with_overlay_files(ir, index, &overlays, excluded);
        let mut batch = Vec::new();
        for id in &pending {
            if cancellation.checkpoint().is_err() {
                failed = true;
                break;
            }
            let Some(doc) = candidate.get(id) else {
                continue;
            };
            let tracked = TrackingFacts {
                ir,
                facts: &facts,
                reads: Mutex::new(BTreeSet::new()),
            };
            let mut next = crate::pipeline::relower_document_snapshot(
                rules,
                profile,
                ir,
                roots,
                doc.clone(),
                Some(&tracked),
            );
            next.fact_dependencies = tracked
                .reads
                .into_inner()
                .expect("overlay-local dependency lock poisoned");
            batch.push((id.clone(), next));
        }
        if failed {
            break;
        }
        report.rounds += 1;
        report.lowers += batch.len();
        let mut changed = BTreeSet::new();
        for (id, next) in batch {
            if let (Some(old), Some(new)) = (
                candidate.get(&id).and_then(|doc| doc.hir.as_deref()),
                next.hir.as_deref(),
            ) && !same_facts(old, new)
            {
                changed.extend(changes(old, new));
                history_bytes =
                    history_bytes.saturating_add(new.syntax().source().len().saturating_mul(16));
            }
            candidate.insert(id, next);
        }
        if changed.is_empty() {
            break;
        }
        let current = state(&candidate);
        if history.iter().any(|past| repeated(past, &current)) {
            failed = true;
            break;
        }
        pending = readers(&candidate, &changed);
        if !pending.is_empty() {
            history_bytes = history_bytes.saturating_add(current.len().saturating_mul(64));
            history.push(current);
        }
    }
    if failed {
        report.coverage.limits.insert(AnalysisLimit::FactStability);
        for doc in base.values_mut() {
            doc.fact_coverage.merge(&report.coverage);
            if let Some(hir) = &mut doc.hir {
                let hir = Arc::make_mut(hir);
                discard_conditional_base_facts(ir, hir);
                hir.merge_analysis_coverage(&report.coverage);
            }
        }
        candidate = base;
    }
    OverlayFactTransaction {
        documents: candidate,
        report,
    }
}

#[cfg(test)]
mod query_dependency_tests {
    use super::reads_symbols;
    use rules::ir::{FieldValue, Shape};

    #[test]
    fn query_dependencies_include_source_key_and_projected_value() {
        let source = serde_json::from_str(
            r#"{
            "types": { "dynamic_name": {} },
            "files": { "test": { "path": "test", "ext": "txt", "root": "consumer" } },
            "schemas": {
                "recursive": { "fields": {
                    "selected": { "card": "0..*", "value": "int" },
                    "dependent": { "card": "0..*", "value": "valuesof<recursive,key=selected>" }
                } },
                "source": { "patterns": [{ "key": "ref<dynamic_name>", "card": "0..*", "value": "bool" }] },
                "priority": { "patterns": [
                    { "key": "ref<dynamic_name>", "card": "0..*", "value": "scalar" },
                    { "key": "scalar", "card": "0..*", "value": "int" }
                ] },
                "consumer": { "fields": {
                    "keys": { "card": "0..*", "value": "keysof<source>" },
                    "values": { "card": "0..*", "value": "valuesof<source,key=chosen>" },
                    "plain": { "card": "0..*", "value": "bool" },
                    "self_query": { "card": "0..*", "value": "valuesof<recursive,key=dependent>" },
                    "filtered_key": { "card": "0..*", "value": "keysof<priority,value_kind_any=(int)>" }
                } }
            }
        }"#,
        )
        .unwrap();
        let ir =
            rules::lower::lower(&[("query.json".to_owned(), source)], Default::default()).unwrap();
        let schema = ir.schema_by_name("consumer").unwrap();
        for (name, expected) in [
            ("keys", true),
            ("values", true),
            ("plain", false),
            ("self_query", false),
            ("filtered_key", true),
        ] {
            let field = ir.lookup(schema, name, Shape::Scalar).next().unwrap();
            let FieldValue::Scalar(matcher) = ir.field(field).value else {
                unreachable!()
            };
            assert_eq!(reads_symbols(&ir, matcher), expected, "{name}");
        }
    }
}
