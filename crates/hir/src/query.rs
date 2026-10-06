//! Source-container context for rule field queries.
//!
//! Selectors inspect the physical sibling property before projecting the matched
//! rule field. Repeated containers with the same path must never share selectors.
use crate::HirProperty;
use rules::query::{QueryContext, SiblingValue};
use text::TextRange;

/// Direct properties in one physical script container.
pub struct PropertyQueryContext<'a> {
    properties: Vec<&'a HirProperty>,
    unknown_ranges: &'a [TextRange],
    defer_templates: bool,
}

impl<'a> PropertyQueryContext<'a> {
    /// Creates the context containing the supplied property, excluding descendants.
    pub fn for_property(properties: &'a [HirProperty], property: &'a HirProperty) -> Self {
        let parent = properties
            .iter()
            .filter(|parent| {
                parent.path.len() + 1 == property.path.len()
                    && property.path.starts_with(&parent.path)
                    && parent.range.start() <= property.range.start()
                    && property.range.end() <= parent.range.end()
            })
            .min_by_key(|parent| parent.range.len());
        match parent {
            Some(parent) => {
                Self::in_container(properties, parent.value_range.unwrap_or(parent.range))
            }
            None => Self::new(properties.iter().filter(|item| item.top_level).collect()),
        }
    }

    /// Creates a context for a container selected by an editor schema fact.
    pub fn for_container(properties: &'a [HirProperty], container: TextRange) -> Self {
        Self::in_container(properties, container)
    }

    /// Selects direct properties in a physical container, excluding nested bodies.
    pub fn in_container(properties: &'a [HirProperty], container: TextRange) -> Self {
        let contained = properties
            .iter()
            .filter(|property| {
                container.start() <= property.range.start()
                    && property.range.end() <= container.end()
            })
            .collect::<Vec<_>>();
        let depth = contained.iter().map(|property| property.path.len()).min();
        Self::new(
            contained
                .into_iter()
                .filter(|property| Some(property.path.len()) == depth)
                .collect(),
        )
    }

    pub(crate) fn from_indices(properties: &'a [HirProperty], indices: &[usize]) -> Self {
        Self::new(indices.iter().map(|index| &properties[*index]).collect())
    }

    fn new(properties: Vec<&'a HirProperty>) -> Self {
        Self {
            properties,
            unknown_ranges: &[],
            defer_templates: false,
        }
    }

    /// Editing holes do not prove a missing or invalid selector.
    pub fn with_unknown_ranges(mut self, ranges: &'a [TextRange]) -> Self {
        self.unknown_ranges = ranges;
        self
    }

    /// Unbound definition-side Template tokens defer selection until instantiation.
    pub fn defer_templates(mut self, defer: bool) -> Self {
        self.defer_templates = defer;
        self
    }

    /// Source token supplying a sibling selector, when the property is present.
    pub fn source_range(&self, name: &str) -> Option<TextRange> {
        self.properties
            .iter()
            .find(|property| property.key.eq_ignore_ascii_case(name))
            .map(|property| {
                property
                    .scalar
                    .as_ref()
                    .map_or(property.key_range, |scalar| scalar.range)
            })
    }

    fn unknown(&self, range: TextRange) -> bool {
        self.unknown_ranges
            .iter()
            .any(|unknown| unknown.start() < range.end() && range.start() < unknown.end())
    }
}

impl QueryContext for PropertyQueryContext<'_> {
    fn sibling(&self, name: &str) -> SiblingValue<'_> {
        let mut found = None;
        let mut unknown_key = false;
        for property in &self.properties {
            if self.unknown(property.key_range)
                || self.defer_templates && property.key.contains('$')
            {
                unknown_key = true;
                continue;
            }
            if property.key.eq_ignore_ascii_case(name) {
                if found.is_some() {
                    return SiblingValue::Duplicate;
                }
                found = Some(*property);
            }
        }
        if unknown_key {
            return SiblingValue::Deferred;
        }
        let Some(property) = found else {
            return SiblingValue::Missing;
        };
        let Some(scalar) = &property.scalar else {
            return if property.value_range.is_none() || property.operator.is_none() {
                SiblingValue::Deferred
            } else {
                SiblingValue::Invalid
            };
        };
        if self.unknown(scalar.range) || self.defer_templates && scalar.value.contains('$') {
            SiblingValue::Deferred
        } else {
            SiblingValue::Scalar(&scalar.value)
        }
    }
}

/// Immutable sibling evidence retained by Template parameter sites.
#[derive(Clone, Debug, Default)]
pub struct OwnedQueryContext {
    pub(crate) values: std::sync::Arc<std::collections::BTreeMap<String, OwnedSibling>>,
    pub(crate) unknown_key: bool,
    pub(crate) retained_bytes: usize,
}

#[derive(Clone, Debug)]
pub(crate) enum OwnedSibling {
    Scalar(String),
    Duplicate,
    Invalid,
    Deferred,
}

impl OwnedQueryContext {
    pub(crate) fn estimated_bytes(&self) -> usize {
        self.retained_bytes
    }

    pub(crate) fn insert(&mut self, name: String, value: OwnedSibling) {
        use std::collections::btree_map::Entry;
        match std::sync::Arc::make_mut(&mut self.values).entry(name.to_ascii_lowercase()) {
            Entry::Vacant(entry) => {
                self.retained_bytes += entry.key().len()
                    + 64
                    + match &value {
                        OwnedSibling::Scalar(value) => value.len(),
                        _ => 0,
                    };
                entry.insert(value);
            }
            Entry::Occupied(mut entry) => {
                if let OwnedSibling::Scalar(value) = entry.get() {
                    self.retained_bytes -= value.len();
                }
                entry.insert(OwnedSibling::Duplicate);
            }
        }
    }
}

impl PropertyQueryContext<'_> {
    pub(crate) fn to_owned(&self) -> OwnedQueryContext {
        let mut result = OwnedQueryContext::default();
        for property in &self.properties {
            if self.unknown(property.key_range)
                || self.defer_templates && property.key.contains('$')
            {
                result.unknown_key = true;
                continue;
            }
            let value = match &property.scalar {
                Some(scalar)
                    if self.unknown(scalar.range)
                        || self.defer_templates && scalar.value.contains('$') =>
                {
                    OwnedSibling::Deferred
                }
                Some(scalar) => OwnedSibling::Scalar(scalar.value.clone()),
                None if property.value_range.is_none() || property.operator.is_none() => {
                    OwnedSibling::Deferred
                }
                None => OwnedSibling::Invalid,
            };
            result.insert(property.key.clone(), value);
        }
        result
    }
}

impl QueryContext for OwnedQueryContext {
    fn sibling(&self, name: &str) -> SiblingValue<'_> {
        if matches!(
            self.values.get(&name.to_ascii_lowercase()),
            Some(OwnedSibling::Duplicate)
        ) {
            return SiblingValue::Duplicate;
        }
        if self.unknown_key {
            return SiblingValue::Deferred;
        }
        match self.values.get(&name.to_ascii_lowercase()) {
            Some(OwnedSibling::Scalar(value)) => SiblingValue::Scalar(value),
            Some(OwnedSibling::Duplicate) => SiblingValue::Duplicate,
            Some(OwnedSibling::Invalid) => SiblingValue::Invalid,
            Some(OwnedSibling::Deferred) => SiblingValue::Deferred,
            None => SiblingValue::Missing,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::Validation;
    use crate::{ScopeState, ScopeValue};
    use rules::ir::{FieldValue, NoSymbolFacts, Shape};
    use std::sync::Arc;

    fn rules() -> rules::ir::RulesIr {
        rules::lower::lower(&[("query.json".into(), serde_json::from_str(r#"{
          "files":{"test":{"path":"test","ext":"txt","root":"root"}},
          "types":{"country":{},"religion":{}},
          "schemas":{
            "root":{"fields":{"group":{"body":"select","card":"0..*"}}},
            "select":{"fields":{
              "on_trigger":{"value":"keysof<triggers,shape=scalar>","card":"0..*"},
              "value":{"value":"valuesof<triggers,key=sibling<on_trigger>,shape=scalar>","card":"0..*"}
            }},
            "triggers":{"fields":{
              "tag":{"value":"ref<country>","card":"0..*"},
              "faith":{"value":"ref<religion>","card":"0..*"},
              "count":{"value":"int","card":"0..*"},
              "flag":{"value":"bool","card":"0..*"},
              "nested":{"body":"empty","card":"0..*"}
            }},
            "empty":{}
          }
        }"#).unwrap())], Default::default()).unwrap()
    }

    struct Facts;
    impl rules::ir::SymbolFacts for Facts {
        fn type_member(&self, _: rules::ir::TypeId, name: &str) -> bool {
            matches!(name, "AAA" | "catholic")
        }
    }

    fn lower(ir: &rules::ir::RulesIr, source: &str, schema: &str) -> crate::HirFile {
        crate::lower_ir_schema(
            Arc::new(parser::parse(parser::FileFormat::Script, source)),
            ir,
            ir.schema_by_name(schema).unwrap(),
            Default::default(),
            ScopeState::initial(ScopeValue::Unknown),
            &Facts,
        )
    }

    #[test]
    fn query_siblings_stay_in_their_physical_container_and_project_references() {
        let ir = rules();
        let hir = lower(
            &ir,
            "group = { on_trigger = tag value = AAA } group = { on_trigger = faith value = catholic }",
            "root",
        );
        let references = hir
            .references()
            .iter()
            .map(|reference| (reference.kind.as_ref(), reference.name.as_str()))
            .collect::<Vec<_>>();
        assert_eq!(
            references,
            vec![("country", "AAA"), ("religion", "catholic")]
        );
        assert!(hir.depends_on_symbol_facts());
        for property in hir
            .properties()
            .iter()
            .filter(|property| property.key == "value")
        {
            let context = PropertyQueryContext::for_property(hir.properties(), property);
            assert_eq!(
                context.sibling("on_trigger"),
                SiblingValue::Scalar(if property.scalar.as_ref().unwrap().value == "AAA" {
                    "tag"
                } else {
                    "faith"
                })
            );
        }
    }

    #[test]
    fn concrete_bad_selectors_reject_while_unbound_templates_defer() {
        let ir = rules();
        let schema = ir.schema_by_name("select").unwrap();
        let FieldValue::Scalar(matcher) = ir
            .field(ir.lookup(schema, "value", Shape::Scalar).next().unwrap())
            .value
        else {
            panic!("scalar");
        };
        for (source, expected) in [
            ("value = yes", SiblingValue::Missing),
            (
                "on_trigger = flag on_trigger = count value = yes",
                SiblingValue::Duplicate,
            ),
            ("on_trigger = {} value = yes", SiblingValue::Invalid),
        ] {
            let hir = lower(&ir, source, "select");
            let property = hir
                .properties()
                .iter()
                .find(|property| property.key == "value")
                .unwrap();
            let context = PropertyQueryContext::for_property(hir.properties(), property);
            assert_eq!(context.sibling("on_trigger"), expected);
            assert_eq!(
                crate::checking::scalar_validation_with_context(
                    &ir,
                    matcher,
                    "yes",
                    &ScopeState::initial(ScopeValue::Unknown),
                    &NoSymbolFacts,
                    &context
                ),
                Validation::Invalid
            );
        }
        let hir = lower(&ir, "on_trigger = $SELECT$ value = yes", "select");
        let property = hir
            .properties()
            .iter()
            .find(|property| property.key == "value")
            .unwrap();
        let context =
            PropertyQueryContext::for_property(hir.properties(), property).defer_templates(true);
        assert_eq!(context.sibling("on_trigger"), SiblingValue::Deferred);
        assert_eq!(
            crate::checking::scalar_validation_with_context(
                &ir,
                matcher,
                "yes",
                &ScopeState::initial(ScopeValue::Unknown),
                &NoSymbolFacts,
                &context
            ),
            Validation::Unknown
        );
    }

    #[test]
    fn a_concrete_instance_rechecks_the_selected_value_domain() {
        let ir = rules();
        for (source, invalid) in [
            ("on_trigger = flag value = yes", false),
            ("on_trigger = count value = yes", true),
            ("value = yes", true),
        ] {
            let hir = lower(&ir, source, "select");
            let rendered = crate::template_text::RenderedTemplate {
                text: source.into(),
                ..Default::default()
            };
            let checked = crate::checking::check_fragment::<std::convert::Infallible>(
                &ir,
                &hir,
                &Facts,
                &rendered,
                &mut || Ok(()),
            )
            .unwrap();
            assert_eq!(
                checked
                    .value
                    .iter()
                    .any(|issue| issue.kind == crate::checking::IssueKind::Value),
                invalid,
                "{source}: {:?}",
                checked.value
            );
        }
    }
    #[test]
    fn selector_failures_are_reported_once_per_container() {
        let ir = rules();
        for (source, wording) in [
            ("value = yes value = no", "missing sibling selector"),
            (
                "on_trigger = flag on_trigger = count value = yes value = no",
                "duplicate sibling selector",
            ),
            ("on_trigger = {} value = yes value = no", "must be a scalar"),
        ] {
            let hir = lower(&ir, source, "select");
            let rendered = crate::template_text::RenderedTemplate {
                text: source.into(),
                ..Default::default()
            };
            let checked = crate::checking::check_fragment::<std::convert::Infallible>(
                &ir,
                &hir,
                &Facts,
                &rendered,
                &mut || Ok(()),
            )
            .unwrap();
            let issues = checked
                .value
                .iter()
                .filter(|issue| issue.explanation.contains(wording))
                .collect::<Vec<_>>();
            assert_eq!(issues.len(), 1, "{source}: {:?}", checked.value);
            assert!(
                !checked
                    .value
                    .iter()
                    .any(|issue| issue.explanation.contains("value `yes`")
                        || issue.explanation.contains("value `no`")),
                "{:?}",
                checked.value
            );
        }
    }
    #[test]
    fn unrelated_sibling_queries_do_not_disable_legacy_switch_validation() {
        let ir = rules::lower::lower(&[("legacy.json".into(),serde_json::from_str(r#"{
          "files":{"test":{"path":"test","ext":"txt","root":"root"}},
          "schemas":{
            "root":{"fields":{"switch":{"body":"legacy","card":"0..*","control":{"kind":"switch","on":"on","selector_schema":"trigger"}}}},
            "legacy":{"fields":{"on":{"value":"scalar","card":"0..*"},"audit":{"value":"valuesof<other,key=sibling<on>>","card":"0..1"}},"patterns":[{"key":"scalar","body":"empty","card":"0..*"}]},
            "trigger":{"fields":{"flag":{"value":"bool","card":"0..*"}}},
            "other":{"fields":{"flag":{"value":"bool","card":"0..*"}}},
            "empty":{}
          }
        }"#).unwrap())],Default::default()).unwrap();
        for (source, wording) in [
            ("switch = { on = flag maybe = {} }", "expected yes or no"),
            (
                "switch = { on = flag on = flag yes = {} }",
                "duplicate sibling selector",
            ),
            ("switch = { yes = {} }", "missing sibling selector"),
        ] {
            let hir = lower(&ir, source, "root");
            let issues = crate::checking::control_lints::<std::convert::Infallible>(
                &ir,
                &hir,
                &Facts,
                None,
                &mut || Ok(()),
            )
            .unwrap();
            assert_eq!(issues.len(), 1, "{source}: {issues:?}");
            assert!(
                issues[0].explanation.contains(wording),
                "{source}: {issues:?}"
            );
        }
    }
    #[test]
    fn query_identity_projection_never_inherits_source_definition_effects() {
        let ir = rules::lower::lower(&[("identity.json".into(), serde_json::from_str(r#"{
          "files":{"test":{"path":"test","root":"root"}},
          "traits":{"Template":{}},
          "types":{"helper":{"impl":{"Template":{"body":"empty"}}},"created":{}},
          "schemas":{
            "root":{"fields":{
              "direct":{"value":"def<created>","card":"0..*"},
              "eligible":{"value":"valuesof<domain,key=optional,shape=scalar,call_args=none>","card":"0..*"},
              "ineligible":{"value":"valuesof<domain,key=required,shape=scalar,call_args=none>","card":"0..*"},
              "nested":{"value":"'prefix:{valuesof<domain,key=optional,shape=scalar,call_args=none>}'","card":"0..*"},
              "outer":{"value":"'{def<created>}:{keysof<domain,shape=scalar,call_args=none>}'","card":"0..*"}
            }},
            "domain":{"patterns":[{"key":"ref<helper>","value":"def<created>","card":"0..*"}]},
            "empty":{}
          }
        }"#).unwrap())],Default::default()).unwrap();
        struct Callable;
        impl rules::ir::SymbolFacts for Callable {
            fn type_member(&self, _: rules::ir::TypeId, name: &str) -> bool {
                matches!(name, "optional" | "required")
            }
            fn template_accepts_no_arguments(
                &self,
                _: rules::ir::TypeId,
                name: &str,
                _: &mut dyn FnMut() -> bool,
            ) -> Option<bool> {
                Some(name == "optional")
            }
        }
        let source = "direct = authored eligible = projected ineligible = rejected nested = prefix:nested outer = invalid:required outer = valid:optional";
        let hir = crate::lower_ir_schema(
            Arc::new(parser::parse(parser::FileFormat::Script, source)),
            &ir,
            ir.schema_by_name("root").unwrap(),
            Default::default(),
            ScopeState::initial(ScopeValue::Unknown),
            &Callable,
        );
        let names = hir
            .definitions()
            .iter()
            .map(|definition| definition.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["authored", "valid"]);
        let references = hir
            .references()
            .iter()
            .map(|reference| reference.name.as_str())
            .collect::<Vec<_>>();
        assert!(references.contains(&"required"), "{references:?}");
        assert!(references.contains(&"optional"), "{references:?}");
    }

    #[test]
    fn large_flat_containers_keep_only_direct_siblings() {
        let ir = rules();
        let source = format!(
            "group = {{ on_trigger = flag {} nested = {{ on_trigger = count value = 0 }} }}",
            "value = yes ".repeat(2048)
        );
        let hir = lower(&ir, &source, "root");
        let group = hir
            .properties()
            .iter()
            .find(|property| property.key == "group")
            .unwrap();
        let context =
            PropertyQueryContext::in_container(hir.properties(), group.value_range.unwrap());
        assert_eq!(context.properties.len(), 2050);
        assert_eq!(context.sibling("on_trigger"), SiblingValue::Scalar("flag"));
        let owned = context.to_owned();
        let cloned = owned.clone();
        assert!(std::sync::Arc::ptr_eq(&owned.values, &cloned.values));
    }
}
