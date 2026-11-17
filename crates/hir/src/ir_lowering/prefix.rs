//! Query-local reuse of unchanged, closed properties in direct Template trials.
use super::*;
use std::sync::Arc;

#[derive(Clone)]
pub(crate) struct PrefixMemo {
    base: Arc<str>,
    unchanged_end: usize,
    entries: Vec<Arc<Entry>>,
    bytes: usize,
    pub(crate) hits: usize,
}
impl PrefixMemo {
    pub(crate) fn new(base: Arc<str>) -> Self {
        Self {
            base,
            unchanged_end: 0,
            entries: Vec::new(),
            bytes: 0,
            hits: 0,
        }
    }
    pub(crate) fn begin(&mut self, text: &str) {
        self.unchanged_end = self
            .base
            .bytes()
            .zip(text.bytes())
            .take_while(|(a, b)| a == b)
            .count();
    }
    #[allow(clippy::too_many_arguments)]
    pub(super) fn lookup(
        &self,
        props: &[HirProperty],
        bare: &[HirScalar],
        property: &HirProperty,
        schema: SchemaId,
        subtypes: &SubtypeSet,
        state: &ScopeState,
        branch_self: Option<SchemaId>,
        out: &IrFacts,
        seen: &std::collections::BTreeSet<(u32, u32)>,
    ) -> Option<Arc<Entry>> {
        if property.range.end() as usize > self.unchanged_end
            || out
                .unknown_ranges
                .iter()
                .any(|r| r.start() < property.range.end())
        {
            return None;
        }
        self.entries
            .iter()
            .find(|entry| {
                let key = &entry.key;
                key.range == property.range
                    && key.schema == schema
                    && key.state == *state
                    && key.subtypes == *subtypes
                    && key.branch_self == branch_self
                    && key.definitions == out.definitions
                    && key.attributes == out.definition_attributes
                    && key.seen == *seen
                    && key.query_depth == out.query_depth
                    && key.coverage == out.analysis_coverage
                    && key.flags
                        == (
                            out.pattern_ambiguous.get(),
                            out.pattern_search_incomplete.get(),
                            out.symbol_facts_dependency.get(),
                        )
                    && key
                        .properties
                        .iter()
                        .eq(props.iter().filter(|p| within(p.range, property.range)))
                    && key
                        .bare
                        .iter()
                        .eq(bare.iter().filter(|p| within(p.range, property.range)))
            })
            .cloned()
    }
    #[allow(clippy::too_many_arguments)]
    pub(super) fn capture(
        &self,
        ir: &RulesIr,
        props: &[HirProperty],
        bare: &[HirScalar],
        property: &HirProperty,
        schema: SchemaId,
        subtypes: &SubtypeSet,
        state: &ScopeState,
        branch_self: Option<SchemaId>,
        out: &IrFacts,
        seen: &std::collections::BTreeSet<(u32, u32)>,
    ) -> Option<Capture> {
        if self.entries.len() >= 64
            || self.bytes >= 4 * 1024 * 1024
            || property.range.end() as usize > self.unchanged_end
            || self.base.len() > 64 * 1024
            || out
                .unknown_ranges
                .iter()
                .any(|r| r.start() < property.range.end())
        {
            return None;
        }
        let shape = if property.scalar.is_some() {
            Shape::Scalar
        } else {
            Shape::Block
        };
        let mut exact = ir.lookup_exact(schema, &property.key, shape).peekable();
        if exact.peek().is_none() || exact.any(|id| {
            let field = ir.field(id);
            matcher_contains_query(ir, field.key)
                || matches!(field.value, rules::ir::FieldValue::Scalar(m) if matcher_contains_query(ir,m))
        }) { return None; }
        Some(Capture {
            key: Key {
                range: property.range,
                schema,
                subtypes: subtypes.clone(),
                state: state.clone(),
                branch_self,
                definitions: out.definitions.clone(),
                attributes: out.definition_attributes.clone(),
                seen: seen.clone(),
                query_depth: out.query_depth,
                coverage: out.analysis_coverage.clone(),
                flags: (
                    out.pattern_ambiguous.get(),
                    out.pattern_search_incomplete.get(),
                    out.symbol_facts_dependency.get(),
                ),
                properties: props
                    .iter()
                    .filter(|p| within(p.range, property.range))
                    .cloned()
                    .collect(),
                bare: bare
                    .iter()
                    .filter(|p| within(p.range, property.range))
                    .cloned()
                    .collect(),
            },
            starts: Starts::new(out),
            forks: out.fork_generation,
            parent_query: out.query_context.values.clone(),
        })
    }
    pub(super) fn remember(
        &mut self,
        capture: Capture,
        out: &IrFacts,
        seen: &std::collections::BTreeSet<(u32, u32)>,
    ) {
        // Fork normalization can reorder earlier schema facts. Such a property
        // uses the ordinary traversal; append-only deltas are cached otherwise.
        if out.fork_generation != capture.forks {
            return;
        }
        let starts = capture.starts;
        let bytes = capture
            .key
            .properties
            .len()
            .saturating_mul(4096)
            .saturating_add(capture.key.definitions.len().saturating_mul(512))
            .saturating_add(4096);
        if self.entries.len() >= 64 || bytes.saturating_add(self.bytes) > 4 * 1024 * 1024 {
            return;
        }
        self.bytes += bytes;
        self.entries.push(Arc::new(Entry {
            key: capture.key,
            fields: out.field_facts[starts.fields..].to_vec(),
            scopes: out.scope_facts[starts.scopes..].to_vec(),
            schemas: out.schema_facts[starts.schemas..].to_vec(),
            definitions: out.definitions[starts.definitions..].to_vec(),
            attributes: out.definition_attributes[starts.attributes..].to_vec(),
            references: out.references[starts.references..].to_vec(),
            bindings: out.binding_references[starts.bindings..].to_vec(),
            overloads: out.overload_facts[starts.overloads..].to_vec(),
            guards: out.runtime_parameter_guards[starts.guards..].to_vec(),
            coverage: out.analysis_coverage.clone(),
            ambiguous: out.pattern_ambiguous.get(),
            incomplete: out.pattern_search_incomplete.get(),
            symbols: out.symbol_facts_dependency.get(),
            seen: seen.clone(),
            final_query: (!Arc::ptr_eq(&capture.parent_query, &out.query_context.values))
                .then(|| out.query_context.clone()),
        }));
    }
}
fn within(inner: TextRange, outer: TextRange) -> bool {
    outer.start() <= inner.start() && inner.end() <= outer.end()
}

struct Key {
    range: TextRange,
    schema: SchemaId,
    subtypes: SubtypeSet,
    state: ScopeState,
    branch_self: Option<SchemaId>,
    definitions: Vec<HirDefinition>,
    attributes: Vec<crate::DefinitionAttributes>,
    seen: std::collections::BTreeSet<(u32, u32)>,
    query_depth: usize,
    properties: Vec<HirProperty>,
    bare: Vec<HirScalar>,
    coverage: crate::analysis::AnalysisCoverage,
    flags: (bool, bool, bool),
}
pub(super) struct Capture {
    key: Key,
    starts: Starts,
    forks: usize,
    parent_query: Arc<std::collections::BTreeMap<String, crate::query::OwnedSibling>>,
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
impl Starts {
    fn new(out: &IrFacts) -> Self {
        Self {
            fields: out.field_facts.len(),
            scopes: out.scope_facts.len(),
            schemas: out.schema_facts.len(),
            definitions: out.definitions.len(),
            attributes: out.definition_attributes.len(),
            references: out.references.len(),
            bindings: out.binding_references.len(),
            overloads: out.overload_facts.len(),
            guards: out.runtime_parameter_guards.len(),
        }
    }
}
pub(super) struct Entry {
    key: Key,
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
    ambiguous: bool,
    incomplete: bool,
    symbols: bool,
    seen: std::collections::BTreeSet<(u32, u32)>,
    final_query: Option<crate::query::OwnedQueryContext>,
}
impl Entry {
    pub(super) fn apply(
        &self,
        out: &mut IrFacts,
        seen: &mut std::collections::BTreeSet<(u32, u32)>,
    ) {
        out.field_facts.extend_from_slice(&self.fields);
        out.scope_facts.extend_from_slice(&self.scopes);
        out.schema_facts.extend_from_slice(&self.schemas);
        for definition in &self.definitions {
            out.local_definitions.insert(definition);
        }
        out.definitions.extend_from_slice(&self.definitions);
        for attributes in &self.attributes {
            out.local_definitions.insert_attributes(attributes);
        }
        out.definition_attributes
            .extend_from_slice(&self.attributes);
        out.references.extend_from_slice(&self.references);
        out.binding_references.extend_from_slice(&self.bindings);
        out.overload_facts.extend_from_slice(&self.overloads);
        out.runtime_parameter_guards.extend_from_slice(&self.guards);
        out.analysis_coverage.merge(&self.coverage);
        out.pattern_ambiguous
            .set(out.pattern_ambiguous.get() || self.ambiguous);
        out.pattern_search_incomplete
            .set(out.pattern_search_incomplete.get() || self.incomplete);
        out.symbol_facts_dependency
            .set(out.symbol_facts_dependency.get() || self.symbols);
        *seen = self.seen.clone();
        if let Some(query) = &self.final_query {
            out.query_context = query.clone();
        }
    }
}
