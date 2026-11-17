//! Reuses closed top-level bodies after validating caller context and all reads.
//! Source-relative deltas never carry the containing document's root range.
use super::*;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

pub(super) struct RootMemo {
    memo: rules::template::TemplateMemo,
    #[cfg(test)]
    hits: std::sync::atomic::AtomicUsize,
}
pub(super) fn for_facts(facts: &dyn SymbolFacts) -> Option<Arc<RootMemo>> {
    const KEY: &str = "hir:root-bodies";
    const BYTES: usize = 64 * 1024 * 1024;
    let shared = facts.shared_template_memo()?;
    if let Some(memo) = shared.get(KEY) {
        return Some(memo);
    }
    let memo = Arc::new(RootMemo {
        memo: rules::template::TemplateMemo::with_max_bytes(BYTES),
        #[cfg(test)]
        hits: Default::default(),
    });
    shared.insert(KEY.into(), memo.clone(), BYTES);
    Some(shared.get(KEY).unwrap_or(memo))
}
fn closed_root(source: &ParsedFile, p: &HirProperty) -> bool {
    p.operator.is_some()
        && p.value_range.is_some()
        && !source.errors().iter().any(|error| {
            error.range.start() <= p.range.end() && p.range.start() <= error.range.end()
        })
        && (p.scalar.is_some()
            || p.value_range
                .and_then(|r| source.text(r))
                .is_some_and(|text| {
                    let text = text.trim();
                    text.starts_with('{') && text.ends_with('}')
                }))
}
fn bucket(path: &LogicalPath, schema: SchemaId, text: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    path.as_str().hash(&mut hash);
    schema.hash(&mut hash);
    text.hash(&mut hash);
    format!("{:x}", hash.finish())
}
struct Key {
    path: LogicalPath,
    text: Arc<str>,
    range: TextRange,
    schema: SchemaId,
    subtypes: SubtypeSet,
    state: ScopeState,
    names: BTreeMap<String, BTreeSet<String>>,
    local_subtypes: BTreeSet<(String, String)>,
    coverage: crate::analysis::AnalysisCoverage,
    flags: (bool, bool, bool),
    retain: bool,
    depth: usize,
}
fn flags(out: &IrFacts) -> (bool, bool, bool) {
    (
        out.pattern_ambiguous.get(),
        out.pattern_search_incomplete.get(),
        out.symbol_facts_dependency.get(),
    )
}
fn inside(outer: TextRange, inner: TextRange) -> bool {
    outer.start() <= inner.start() && inner.end() <= outer.end()
}
struct Starts {
    fields: usize,
    scopes: usize,
    schemas: usize,
    definitions: usize,
    attributes: usize,
    references: usize,
    bindings: usize,
    overloads: usize,
    guards: usize,
}
pub(super) struct Capture {
    key: Key,
    bucket: String,
    starts: Starts,
    forks: usize,
}
struct Entry {
    key: Key,
    delta: Delta,
    reads: crate::memo_facts::Receipts,
    bytes: usize,
}
struct Entries(Vec<Arc<Entry>>);
#[derive(Clone)]
pub(super) struct Delta {
    fields: Vec<FieldFact>,
    scopes: Vec<ScopeFact>,
    schemas: Vec<SchemaFact>,
    definitions: Vec<HirDefinition>,
    attributes: Vec<crate::DefinitionAttributes>,
    references: Vec<HirReference>,
    bindings: Vec<HirReference>,
    overloads: Vec<crate::block_checking::OverloadFact>,
    guards: Vec<(TextRange, Option<TextRange>)>,
    coverage: crate::analysis::AnalysisCoverage,
    flags: (bool, bool, bool),
    seen: Vec<(u32, u32)>,
}
// These are append-only summaries, never semantic dispatch inputs. Analyze
// each root with a fresh summary and merge its prefix back after caching. An
// earlier incomplete root must neither poison nor be erased by a later hit.
pub(super) struct Metadata {
    coverage: crate::analysis::AnalysisCoverage,
    flags: (bool, bool, bool),
}
impl Metadata {
    pub(super) fn take(out: &mut IrFacts) -> Self {
        Self {
            coverage: std::mem::take(&mut out.analysis_coverage),
            flags: (
                out.pattern_ambiguous.replace(false),
                out.pattern_search_incomplete.replace(false),
                out.symbol_facts_dependency.replace(false),
            ),
        }
    }
    pub(super) fn restore(self, out: &mut IrFacts) {
        out.analysis_coverage.merge(&self.coverage);
        out.pattern_ambiguous
            .set(out.pattern_ambiguous.get() || self.flags.0);
        out.pattern_search_incomplete
            .set(out.pattern_search_incomplete.get() || self.flags.1);
        out.symbol_facts_dependency
            .set(out.symbol_facts_dependency.get() || self.flags.2);
    }
}
impl RootMemo {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn lookup(
        &self,
        path: &LogicalPath,
        source: &ParsedFile,
        p: &HirProperty,
        schema: SchemaId,
        subtypes: &SubtypeSet,
        state: &ScopeState,
        out: &IrFacts,
        seen: &BTreeSet<(u32, u32)>,
        facts: &dyn SymbolFacts,
    ) -> Option<Box<Delta>> {
        if !closed_root(source, p) {
            return None;
        }
        let text = source.text(p.range)?;
        if text.len() > 128 * 1024
            || seen
                .iter()
                .any(|(position, _)| p.range.start() <= *position && *position < p.range.end())
        {
            return None;
        }
        let entries = self.memo.get::<Entries>(&bucket(path, schema, text))?;
        for entry in &entries.0 {
            let key = &entry.key;
            if key.path != *path
                || key.text.as_ref() != text
                || key.schema != schema
                || key.subtypes != *subtypes
                || key.state != *state
                || key.names != out.local_definitions.names
                || key.local_subtypes != out.local_definitions.subtypes
                || key.coverage != out.analysis_coverage
                || key.flags != flags(out)
                || key.retain != out.retain_validation_facts
                || key.depth != out.query_depth
            {
                continue;
            }
            if !entry
                .reads
                .validate_and_replay(facts, &mut || Ok::<(), std::convert::Infallible>(()))
                .expect("infallible checkpoint")
            {
                continue;
            }
            let mut delta = entry.delta.clone();
            delta.shift(i64::from(p.range.start()) - i64::from(key.range.start()))?;
            #[cfg(test)]
            self.hits.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return Some(Box::new(delta));
        }
        None
    }
    #[allow(clippy::too_many_arguments)]
    pub(super) fn capture(
        &self,
        path: &LogicalPath,
        source: &ParsedFile,
        p: &HirProperty,
        schema: SchemaId,
        subtypes: &SubtypeSet,
        state: &ScopeState,
        out: &IrFacts,
        seen: &BTreeSet<(u32, u32)>,
    ) -> Option<Box<Capture>> {
        if !closed_root(source, p) {
            return None;
        }
        let text = source.text(p.range)?;
        if text.len() > 128 * 1024
            || seen
                .iter()
                .any(|(position, _)| p.range.start() <= *position && *position < p.range.end())
        {
            return None;
        }
        Some(Box::new(Capture {
            bucket: bucket(path, schema, text),
            key: Key {
                path: path.clone(),
                text: Arc::from(text),
                range: p.range,
                schema,
                subtypes: subtypes.clone(),
                state: state.clone(),
                names: out.local_definitions.names.clone(),
                local_subtypes: out.local_definitions.subtypes.clone(),
                coverage: out.analysis_coverage.clone(),
                flags: flags(out),
                retain: out.retain_validation_facts,
                depth: out.query_depth,
            },
            starts: Starts {
                fields: out.field_facts.len(),
                scopes: out.scope_facts.len(),
                schemas: out.schema_facts.len(),
                definitions: out.definitions.len(),
                attributes: out.definition_attributes.len(),
                references: out.references.len(),
                bindings: out.binding_references.len(),
                overloads: out.overload_facts.len(),
                guards: out.runtime_parameter_guards.len(),
            },
            forks: out.fork_generation,
        }))
    }
    pub(super) fn remember(
        &self,
        capture: Box<Capture>,
        out: &IrFacts,
        seen: &BTreeSet<(u32, u32)>,
        reads: crate::memo_facts::Receipts,
    ) {
        // Forks sort cumulative schema tables. Cache only append-only bodies.
        if capture.forks != out.fork_generation
            || capture.key.depth != out.query_depth
            || !out.analysis_coverage.is_complete()
        {
            return;
        }
        let st = capture.starts;
        let delta = Delta {
            fields: out.field_facts[st.fields..].to_vec(),
            scopes: out.scope_facts[st.scopes..].to_vec(),
            schemas: out.schema_facts[st.schemas..].to_vec(),
            definitions: out.definitions[st.definitions..].to_vec(),
            attributes: out.definition_attributes[st.attributes..].to_vec(),
            references: out.references[st.references..].to_vec(),
            bindings: out.binding_references[st.bindings..].to_vec(),
            overloads: out.overload_facts[st.overloads..].to_vec(),
            guards: out.runtime_parameter_guards[st.guards..].to_vec(),
            coverage: out.analysis_coverage.clone(),
            flags: flags(out),
            seen: seen
                .iter()
                .copied()
                .filter(|(position, _)| {
                    capture.key.range.start() <= *position && *position < capture.key.range.end()
                })
                .collect(),
        };
        if !delta.within(capture.key.range) {
            return;
        }
        let inputs = capture
            .key
            .names
            .iter()
            .map(|(kind, names)| {
                128 + kind.len() + names.iter().map(|name| 96 + name.len()).sum::<usize>()
            })
            .sum::<usize>()
            + capture
                .key
                .local_subtypes
                .iter()
                .map(|(name, subtype)| 128 + name.len() + subtype.len())
                .sum::<usize>();
        let count = delta.fields.len()
            + delta.scopes.len()
            + delta.schemas.len()
            + delta.definitions.len()
            + delta.attributes.len()
            + delta.references.len()
            + delta.bindings.len()
            + delta.overloads.len()
            + delta.guards.len()
            + delta.seen.len();
        let bytes = capture
            .key
            .text
            .len()
            .saturating_mul(8)
            .saturating_add(inputs)
            .saturating_add(count.saturating_mul(2048))
            .saturating_add(reads.bytes());
        if bytes > 16 * 1024 * 1024 {
            return;
        }
        let mut entries = self
            .memo
            .get::<Entries>(&capture.bucket)
            .map_or_else(Vec::new, |entries| entries.0.clone());
        if entries.len() >= 2 {
            entries.remove(0);
        }
        entries.push(Arc::new(Entry {
            key: capture.key,
            delta,
            reads,
            bytes,
        }));
        let bytes = entries
            .iter()
            .fold(0usize, |sum, entry| sum.saturating_add(entry.bytes));
        self.memo
            .replace(capture.bucket, Arc::new(Entries(entries)), bytes);
    }
}
impl Delta {
    fn within(&self, range: TextRange) -> bool {
        self.fields.iter().all(|f| inside(range, f.range))
            && self.scopes.iter().all(|f| inside(range, f.range))
            && self.schemas.iter().all(|f| inside(range, f.range))
            && self
                .definitions
                .iter()
                .all(|d| inside(range, d.range) && inside(range, d.selection_range))
            && self
                .attributes
                .iter()
                .all(|a| inside(range, a.definition_range))
            && self
                .references
                .iter()
                .chain(&self.bindings)
                .all(|r| inside(range, r.range))
            && self
                .overloads
                .iter()
                .all(|f| inside(range, f.range) && inside(range, f.container))
            && self
                .guards
                .iter()
                .all(|(r, g)| inside(range, *r) && g.is_none_or(|g| inside(range, g)))
    }
    fn shift(&mut self, delta: i64) -> Option<()> {
        let point = |p: u32| u32::try_from(i64::from(p) + delta).ok();
        let range = |r: TextRange| TextRange::new(point(r.start())?, point(r.end())?);
        for f in &mut self.fields {
            f.range = range(f.range)?;
        }
        for f in &mut self.scopes {
            f.range = range(f.range)?;
        }
        for f in &mut self.schemas {
            f.range = range(f.range)?;
        }
        for f in &mut self.definitions {
            f.range = range(f.range)?;
            f.selection_range = range(f.selection_range)?;
        }
        for f in &mut self.attributes {
            f.definition_range = range(f.definition_range)?;
        }
        for f in self.references.iter_mut().chain(&mut self.bindings) {
            f.range = range(f.range)?;
        }
        for f in &mut self.overloads {
            f.range = range(f.range)?;
            f.container = range(f.container)?;
        }
        for (r, g) in &mut self.guards {
            *r = range(*r)?;
            if let Some(g) = g {
                *g = range(*g)?;
            }
        }
        for (position, _) in &mut self.seen {
            *position = point(*position)?;
        }
        Some(())
    }
    pub(super) fn apply(self: Box<Self>, out: &mut IrFacts, seen: &mut BTreeSet<(u32, u32)>) {
        for d in &self.definitions {
            out.local_definitions.insert(d);
        }
        for a in &self.attributes {
            out.local_definitions.insert_attributes(a);
        }
        out.field_facts.extend(self.fields);
        out.scope_facts.extend(self.scopes);
        out.schema_facts.extend(self.schemas);
        out.definitions.extend(self.definitions);
        out.definition_attributes.extend(self.attributes);
        out.references.extend(self.references);
        out.binding_references.extend(self.bindings);
        out.overload_facts.extend(self.overloads);
        out.runtime_parameter_guards.extend(self.guards);
        out.analysis_coverage.merge(&self.coverage);
        out.pattern_ambiguous
            .set(out.pattern_ambiguous.get() || self.flags.0);
        out.pattern_search_incomplete
            .set(out.pattern_search_incomplete.get() || self.flags.1);
        out.symbol_facts_dependency
            .set(out.symbol_facts_dependency.get() || self.flags.2);
        seen.extend(self.seen);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    struct Facts {
        shared: bool,
        member: Cell<bool>,
        template: Option<(TypeId, Arc<crate::Template>)>,
        memo: Arc<rules::template::TemplateMemo>,
    }
    impl SymbolFacts for Facts {
        fn shared_template_memo(&self) -> Option<Arc<rules::template::TemplateMemo>> {
            self.shared.then(|| self.memo.clone())
        }
        fn type_member(&self, kind: TypeId, name: &str) -> bool {
            if name.eq_ignore_ascii_case("cycle") {
                return self.template.as_ref().is_some_and(|(ty, _)| *ty == kind);
            }
            self.member.get()
        }
        fn template(&self, kind: TypeId, name: &str) -> Option<Arc<crate::Template>> {
            self.template
                .as_ref()
                .filter(|(ty, _)| *ty == kind && name.eq_ignore_ascii_case("cycle"))
                .map(|(_, template)| template.clone())
        }
    }
    #[test]
    fn unchanged_roots_rebase_exactly_and_revalidate_local_and_workspace_inputs() {
        let ir = game::eu4::first_party_ir().unwrap();
        let rules = game::eu4::runtime_rules().unwrap();
        let profile = game::eu4::profile();
        let path = LogicalPath::parse("events/roots.txt").unwrap();
        let mut facts = Facts {
            shared: true,
            member: Cell::new(false),
            template: None,
            memo: Arc::new(rules::template::TemplateMemo::with_max_bytes(
                128 * 1024 * 1024,
            )),
        };
        let source = "country_event = { id = first immediate = { add_stability = VALUE } } country_event = { id = second trigger = { religion = faith } immediate = { add_prestige = 1 } }";
        let run = |facts: &Facts, source: &str| {
            crate::lower_shared_with_ir_and_facts(
                Arc::new(parser::parse(parser::FileFormat::Script, source)),
                &path,
                &rules,
                &profile,
                &ir,
                facts,
            )
        };
        run(&facts, &source.replace("VALUE", "1"));
        let memo = for_facts(&facts).unwrap();
        for value in ["1000", "-2", "3"] {
            let source = source.replace("VALUE", value);
            let reused = run(&facts, &source);
            facts.shared = false;
            let fresh = run(&facts, &source);
            assert_eq!(reused, fresh, "{value}");
            facts.shared = true;
        }
        assert!(
            memo.hits.load(std::sync::atomic::Ordering::Relaxed) >= 3,
            "unchanged second roots must use the cache"
        );
        facts.member.set(true);
        let definition = crate::lower_shared_with_ir(
            Arc::new(parser::parse(
                parser::FileFormat::Script,
                "cycle={cycle={}}",
            )),
            &LogicalPath::parse("common/scripted_effects/cycle.txt").unwrap(),
            &rules,
            &profile,
            &ir,
        );
        facts.template = Some((
            ir.type_by_name("scripted_effect").unwrap(),
            Arc::new(definition.dynamic_templates()[0].clone()),
        ));
        let incomplete = source.replace("add_stability = VALUE", "cycle = { ARG=one }");
        let original = run(&facts, &incomplete);
        assert!(!original.analysis_coverage().is_complete());
        let hits = memo.hits.load(std::sync::atomic::Ordering::Relaxed);
        for replacement in ["cycle = { ARG=two }", "cycle = { ARG=three }"] {
            let changed = incomplete.replace("cycle = { ARG=one }", replacement);
            let reused = run(&facts, &changed);
            facts.shared = false;
            let fresh = run(&facts, &changed);
            assert_eq!(reused, fresh, "earlier incomplete root: {replacement}");
            facts.shared = true;
        }
        assert!(memo.hits.load(std::sync::atomic::Ordering::Relaxed) >= hits + 2);
        for (member, value) in [
            (true, "1000"),
            (false, "set_country_flag = local"),
            (true, "300"),
        ] {
            facts.member.set(member);
            let source = if value.contains('=') {
                source.replace("add_stability = VALUE", value)
            } else {
                source.replace("VALUE", value)
            };
            let reused = run(&facts, &source);
            facts.shared = false;
            let fresh = run(&facts, &source);
            assert_eq!(reused, fresh, "workspace/local membership changed: {value}");
            facts.shared = true;
        }
    }
}
