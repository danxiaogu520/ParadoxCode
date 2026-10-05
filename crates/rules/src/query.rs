//! Schema field queries. Projections retain source field identities, matchers,
//! and provenance; they never turn a schema into an independent member table.
use serde::{Deserialize, Serialize};

use crate::ir::{
    FieldId, FieldValue, Matcher, MatcherId, RulesIr, SchemaId, Shape, Symbol, SymbolFacts,
};

/// The part of a selected field exposed as the scalar domain.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub enum QueryProjection {
    Keys,
    Values,
}

/// A concrete selector or a value supplied by the containing script block.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub enum QuerySelector {
    Literal(Symbol),
    Sibling(Symbol),
}

/// Known primitive branches accepted by `value_kind_any`.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub enum QueryValueKind {
    Int,
    Float,
    Bool,
}

/// Compiled, reusable schema query. Filters are conjunctive; kind members are ANY.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct FieldQuery {
    pub schema: SchemaId,
    pub projection: QueryProjection,
    pub selector: Option<QuerySelector>,
    pub scope_accepts: Option<Symbol>,
    pub shape: Option<Shape>,
    pub value_kind_any: Box<[QueryValueKind]>,
    pub capability: Option<Symbol>,
}

impl FieldQuery {
    /// An unfiltered projection, useful for consumers of existing control metadata.
    #[must_use]
    pub fn new(schema: SchemaId, projection: QueryProjection) -> Self {
        Self {
            schema,
            projection,
            selector: None,
            scope_accepts: None,
            shape: None,
            value_kind_any: Box::new([]),
            capability: None,
        }
    }
}

/// A sibling selector's evidence, before attempting schema lookup.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SiblingValue<'a> {
    Scalar(&'a str),
    Missing,
    Duplicate,
    Invalid,
    Deferred,
}

/// Same-container values supplied by the script consumer. Implementations must
/// distinguish duplicates and non-scalars rather than selecting an arbitrary one.
pub trait QueryContext {
    fn sibling(&self, name: &str) -> SiblingValue<'_>;
}

/// A context-free request has no evidence about physical sibling values.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoQueryContext;

impl QueryContext for NoQueryContext {
    fn sibling(&self, _name: &str) -> SiblingValue<'_> {
        SiblingValue::Deferred
    }
}

/// Context-free calls cannot prove a sibling value is missing in the document.
impl QueryContext for () {
    fn sibling(&self, _name: &str) -> SiblingValue<'_> {
        SiblingValue::Deferred
    }
}

/// Why a dependent domain could (or could not) be selected.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QueryState {
    Resolved,
    Missing,
    Duplicate,
    Invalid,
    Deferred,
}

/// Selected source fields, preserving their key/value matchers and provenance.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryResolution {
    pub state: QueryState,
    pub fields: Vec<FieldId>,
}

impl QueryResolution {
    fn state(state: QueryState) -> Self {
        Self {
            state,
            fields: Vec::new(),
        }
    }
}

impl RulesIr {
    /// Projects a field without copying its matcher or provenance.
    #[must_use]
    pub fn query_projected_matcher(&self, query: &FieldQuery, field: FieldId) -> Option<MatcherId> {
        match query.projection {
            QueryProjection::Keys => Some(self.field(field).key),
            QueryProjection::Values => match self.field(field).value {
                FieldValue::Scalar(matcher) => Some(matcher),
                FieldValue::Block(_) | FieldValue::SelfBlock => None,
            },
        }
    }

    /// Source membership without document-dependent selector resolution, for
    /// completion and conservative dependency tracing.
    #[must_use]
    pub fn query_source_fields(&self, query: &FieldQuery) -> Vec<FieldId> {
        self.fields(query.schema)
            .into_iter()
            .filter(|field| self.query_field_passes(query, *field))
            .collect()
    }

    /// Enumerates a query or resolves its selector using normal exact/pattern
    /// dispatch BEFORE applying query filters. Scope effects are never applied.
    #[must_use]
    pub fn query_fields(
        &self,
        query: &FieldQuery,
        facts: &impl SymbolFacts,
        context: &impl QueryContext,
    ) -> QueryResolution {
        self.query_fields_with(query, context, &mut |id, text| {
            self.scalar_outcome_with_context(id, text, facts, context)
        })
    }

    pub(crate) fn query_fields_with(
        &self,
        query: &FieldQuery,
        context: &impl QueryContext,
        matches: &mut impl FnMut(MatcherId, &str) -> Option<bool>,
    ) -> QueryResolution {
        let selected = match &query.selector {
            None => None,
            Some(QuerySelector::Literal(key)) => Some(self.strings.resolve(*key)),
            Some(QuerySelector::Sibling(name)) => {
                match context.sibling(self.strings.resolve(*name)) {
                    SiblingValue::Scalar(value) => Some(value),
                    SiblingValue::Missing => return QueryResolution::state(QueryState::Missing),
                    SiblingValue::Duplicate => {
                        return QueryResolution::state(QueryState::Duplicate);
                    }
                    SiblingValue::Invalid => return QueryResolution::state(QueryState::Invalid),
                    SiblingValue::Deferred => return QueryResolution::state(QueryState::Deferred),
                }
            }
        };
        if let Some(key) = selected {
            self.query_selected_fields_with(query, key, matches)
        } else {
            QueryResolution {
                state: QueryState::Resolved,
                fields: self.query_source_fields(query),
            }
        }
    }

    /// Resolves an external selector (for example a control attribute) without
    /// interning document text. Missing and malformed sibling evidence belongs
    /// to `query_fields`; this method accepts an already established scalar.
    #[must_use]
    pub fn query_selected_fields(
        &self,
        query: &FieldQuery,
        key: &str,
        facts: &impl SymbolFacts,
        context: &impl QueryContext,
    ) -> QueryResolution {
        self.query_selected_fields_with(query, key, &mut |id, text| {
            self.scalar_outcome_with_context(id, text, facts, context)
        })
    }

    pub(crate) fn query_selected_fields_with(
        &self,
        query: &FieldQuery,
        key: &str,
        matches: &mut impl FnMut(MatcherId, &str) -> Option<bool>,
    ) -> QueryResolution {
        let schema = self.schema(query.schema);
        // Shape dispatch precedes metadata filters. An exact key of the
        // selected shape cannot escape a filter through a broader pattern.
        let exact = self
            .strings
            .lookup_folded(key)
            .and_then(|symbol| schema.exact.get(&symbol));
        if let Some(fields) = exact
            && fields.iter().any(|id| {
                query
                    .shape
                    .is_none_or(|shape| self.shape(*id) == Some(shape))
            })
        {
            // Retain every exact overload of the selected shape. Filters may
            // narrow these actual candidates but cannot expose a fallback.
            let fields: Vec<_> = fields
                .iter()
                .copied()
                .filter(|id| self.query_field_passes(query, *id))
                .collect();
            return if fields.is_empty() {
                QueryResolution::state(QueryState::Invalid)
            } else {
                QueryResolution {
                    state: QueryState::Resolved,
                    fields,
                }
            };
        }
        let candidates = schema.patterns.iter().copied().filter(|id| {
            query
                .shape
                .is_none_or(|shape| self.shape(*id) == Some(shape))
        });
        for field in candidates {
            let matched = matches(self.field(field).key, key);
            match matched {
                None => return QueryResolution::state(QueryState::Deferred),
                Some(false) => continue,
                Some(true) => {
                    return if self.query_field_passes(query, field) {
                        QueryResolution {
                            state: QueryState::Resolved,
                            fields: vec![field],
                        }
                    } else {
                        QueryResolution::state(QueryState::Invalid)
                    };
                }
            }
        }
        QueryResolution::state(QueryState::Invalid)
    }

    /// Tests metadata on the original declaration. Numeric membership is known
    /// only from int/float/bool branches, never inferred from scalar/ref/enum.
    #[must_use]
    pub fn query_field_passes(&self, query: &FieldQuery, field: FieldId) -> bool {
        let source = self.field(field);
        if query
            .shape
            .is_some_and(|shape| self.shape(field) != Some(shape))
        {
            return false;
        }
        if query.projection == QueryProjection::Values
            && !matches!(source.value, FieldValue::Scalar(_))
        {
            return false;
        }
        if let Some(actual) = query.scope_accepts
            && let Some(scope) = &source.scope
            && !scope.scopes_in.is_empty()
            && !scope
                .scopes_in
                .iter()
                .any(|expected| self.scopes_compatible(actual, *expected))
        {
            return false;
        }
        if let Some(capability) = query.capability
            && !source.capabilities.contains(&capability)
        {
            return false;
        }
        if !query.value_kind_any.is_empty() {
            let FieldValue::Scalar(matcher) = source.value else {
                return false;
            };
            if !self.matcher_has_query_kind(matcher, &query.value_kind_any) {
                return false;
            }
        }
        true
    }

    fn matcher_has_query_kind(&self, matcher: MatcherId, kinds: &[QueryValueKind]) -> bool {
        match self.matcher(matcher) {
            Matcher::Int { .. } => kinds.contains(&QueryValueKind::Int),
            Matcher::Float { .. } => kinds.contains(&QueryValueKind::Float),
            Matcher::Bool => kinds.contains(&QueryValueKind::Bool),
            Matcher::Union(branches) => branches
                .iter()
                .any(|branch| self.matcher_has_query_kind(*branch, kinds)),
            _ => false,
        }
    }

    /// Evaluates a query domain with its source matchers and script context.
    #[must_use]
    pub fn query_outcome(
        &self,
        query: &FieldQuery,
        value: &str,
        facts: &impl SymbolFacts,
        context: &impl QueryContext,
    ) -> Option<bool> {
        let mut no_cancel = || false;
        let mut budget = crate::pattern::SearchBudget::new(Default::default(), &mut no_cancel);
        crate::pattern::evaluate_query(self, query, value, &mut budget, context, &mut |id, text| {
            self.scalar_primitive_outcome(id, text, facts)
        })
    }
}
