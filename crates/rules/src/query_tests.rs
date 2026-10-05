use crate::ir::{
    FieldValue, GameConfig, Matcher, MatcherId, NoSymbolFacts, RulesIr, Shape, SymbolFacts, TypeId,
};
use crate::query::{NoQueryContext, QueryContext, QueryProjection, QueryState, SiblingValue};

fn compile(source: &str) -> RulesIr {
    crate::lower::lower(
        &[("query.json".into(), serde_json::from_str(source).unwrap())],
        GameConfig::default(),
    )
    .unwrap_or_else(|error| panic!("{:?}", error.diagnostics()))
}

fn fixture() -> RulesIr {
    compile(
        r#"{
        "files":{"sample":{"path":"sample","root":"consumer"}},
        "types":{"member":{}},
        "enums":{"letters":["a","b"]},
        "scopes":{"types":["country","province","trade"],"compat":[{"actual":"trade","expected":"country"}]},
        "schemas":{
            "consumer":{"fields":{
                "selector":{"value":"keysof<domain,shape=scalar>","card":"0..1"},
                "case":{"value":"valuesof<domain,key=sibling<selector>,shape=scalar>","card":"0..*"},
                "numeric":{"value":"keysof<domain,shape=scalar,value_kind_any=(int|float|bool)>","card":"0..1"},
                "export":{"value":"keysof<domain,capability=exportable,value_kind_any=(int|float|bool)>","card":"0..1"},
                "country":{"value":"keysof<domain,scope_accepts=country>","card":"0..1"},
                "trade":{"value":"keysof<domain,scope_accepts=trade>","card":"0..1"},
                "template":{"value":"'pre_{keysof<domain,shape=scalar>}_post'","card":"0..1"},
                "fixed":{"value":"valuesof<domain,key=bounded,shape=scalar>","card":"0..1"}
            }},
            "domain":{"fields":{
                "bounded":{"value":"int[1..3]","card":"0..*","capabilities":["exportable"]},
                "mixed":{"value":"ref<member>|float[0..]","card":"0..*"},
                "boolean":{"value":"bool","card":"0..*"},
                "arbitrary":{"value":"scalar","card":"0..*"},
                "reference":{"value":"ref<member>","card":"0..*"},
                "enumerated":{"value":"enum<letters>","card":"0..*"},
                "country_only":{"value":"float","card":"0..*","scope":{"in":["country"]}},
                "province_only":{"value":"float","card":"0..*","scope":{"in":["province"]}},
                "unrestricted":{"value":"float","card":"0..*","scope":{"in":[]}},
                "effects_ignored":{"value":"bool","card":"0..*","scope":{"push":"province"}},
                "container":{"body":"leaf","card":"0..*"}
            },"patterns":[{"key":"'dynamic_{ref<member>}'","value":"bool","card":"0..*"}]},
            "leaf":{"open":true}
        }
    }"#,
    )
}

fn matcher(ir: &RulesIr, name: &str) -> MatcherId {
    let schema = ir.schema_by_name("consumer").unwrap();
    let field = ir.lookup(schema, name, Shape::Scalar).next().unwrap();
    let FieldValue::Scalar(matcher) = ir.field(field).value else {
        panic!("scalar")
    };
    matcher
}

struct Facts {
    complete: bool,
}
impl SymbolFacts for Facts {
    fn facts_complete(&self) -> bool {
        self.complete
    }
    fn type_member(&self, _: TypeId, name: &str) -> bool {
        name.eq_ignore_ascii_case("known")
    }
}
struct Context<'a>(SiblingValue<'a>);
impl QueryContext for Context<'_> {
    fn sibling(&self, name: &str) -> SiblingValue<'_> {
        assert_eq!(name, "selector");
        self.0
    }
}

#[test]
fn kinds_use_any_known_primitive_branch_and_preserve_ranges() {
    let ir = fixture();
    let numeric = matcher(&ir, "numeric");
    for accepted in [
        "bounded",
        "mixed",
        "boolean",
        "country_only",
        "dynamic_known",
    ] {
        assert!(
            ir.scalar_matches(numeric, accepted, &Facts { complete: true }),
            "{accepted}"
        );
    }
    for excluded in ["arbitrary", "reference", "enumerated", "container"] {
        assert!(
            !ir.scalar_matches(numeric, excluded, &Facts { complete: true }),
            "{excluded}"
        );
    }
    let fixed = matcher(&ir, "fixed");
    assert!(ir.scalar_matches(fixed, "2", &NoSymbolFacts));
    for invalid in ["0", "4", "yes", "1.5"] {
        assert!(!ir.scalar_matches(fixed, invalid, &NoSymbolFacts));
    }
}

#[test]
fn scope_filter_uses_actual_compatibility_and_includes_unrestricted() {
    let ir = fixture();
    for name in ["country", "trade"] {
        let query = matcher(&ir, name);
        for accepted in ["country_only", "unrestricted", "effects_ignored", "bounded"] {
            assert!(
                ir.scalar_matches(query, accepted, &NoSymbolFacts),
                "{name}:{accepted}"
            );
        }
        assert!(!ir.scalar_matches(query, "province_only", &NoSymbolFacts));
    }
}

#[test]
fn capabilities_are_explicit_and_case_insensitive() {
    let ir = fixture();
    let export = matcher(&ir, "export");
    assert!(ir.scalar_matches(export, "BoUnDeD", &NoSymbolFacts));
    assert!(!ir.scalar_matches(export, "mixed", &NoSymbolFacts));
    assert!(!ir.scalar_matches(export, "dynamic_known", &Facts { complete: true }));
}

#[test]
fn dependent_values_preserve_each_context_state() {
    let ir = fixture();
    let id = matcher(&ir, "case");
    let Matcher::Query(query) = ir.matcher(id) else {
        panic!("query")
    };
    for (context, expected) in [
        (SiblingValue::Missing, QueryState::Missing),
        (SiblingValue::Duplicate, QueryState::Duplicate),
        (SiblingValue::Invalid, QueryState::Invalid),
        (SiblingValue::Deferred, QueryState::Deferred),
        (SiblingValue::Scalar("absent"), QueryState::Invalid),
        (SiblingValue::Scalar("container"), QueryState::Invalid),
    ] {
        assert_eq!(
            ir.query_fields(query, &NoSymbolFacts, &Context(context))
                .state,
            expected
        );
        assert_eq!(
            ir.scalar_outcome_with_context(id, "yes", &NoSymbolFacts, &Context(context)),
            if expected == QueryState::Deferred {
                None
            } else {
                Some(false)
            }
        );
    }
    assert_eq!(ir.scalar_outcome(id, "yes", &NoSymbolFacts), None);
    let context = Context(SiblingValue::Scalar("bounded"));
    assert_eq!(
        ir.scalar_outcome_with_context(id, "2", &NoSymbolFacts, &context),
        Some(true)
    );
    assert_eq!(
        ir.scalar_outcome_with_context(id, "9", &NoSymbolFacts, &context),
        Some(false)
    );
}

#[test]
fn dynamic_patterns_and_template_holes_keep_workspace_facts() {
    let ir = fixture();
    let selector = matcher(&ir, "selector");
    assert_eq!(
        ir.scalar_outcome(selector, "dynamic_missing", &Facts { complete: false }),
        None
    );
    assert_eq!(
        ir.scalar_outcome(selector, "dynamic_missing", &Facts { complete: true }),
        Some(false)
    );
    assert_eq!(
        ir.scalar_outcome(selector, "dynamic_known", &Facts { complete: true }),
        Some(true)
    );
    let template = matcher(&ir, "template");
    assert!(ir.scalar_matches(
        template,
        "pre_dynamic_known_post",
        &Facts { complete: true }
    ));
    assert!(!ir.scalar_matches(template, "pre_container_post", &Facts { complete: true }));
    let context = Context(SiblingValue::Scalar("dynamic_known"));
    assert_eq!(
        ir.scalar_outcome_with_context(
            matcher(&ir, "case"),
            "yes",
            &Facts { complete: true },
            &context
        ),
        Some(true)
    );
}

#[test]
fn source_identity_and_provenance_survive_projection_and_bake() {
    let ir = fixture();
    let Matcher::Query(query) = ir.matcher(matcher(&ir, "fixed")) else {
        panic!("query")
    };
    let resolution = ir.query_fields(query, &NoSymbolFacts, &NoQueryContext);
    assert_eq!(resolution.state, QueryState::Resolved);
    let source = resolution.fields[0];
    assert_eq!(
        ir.lookup(query.schema, "bounded", Shape::Scalar).next(),
        Some(source)
    );
    assert_eq!(
        ir.strings
            .resolve(ir.provenance_of(source).unwrap().pointer),
        "/schemas/domain/fields/bounded"
    );
    assert_eq!(
        Some(ir.field(source).value),
        ir.query_projected_matcher(query, source)
            .map(FieldValue::Scalar)
    );
    let decoded = RulesIr::from_baked(&serde_json::to_vec(&ir).unwrap()).unwrap();
    assert_eq!(decoded.rule_hash(), ir.rule_hash());
    assert!(decoded.scalar_matches(matcher(&decoded, "fixed"), "3", &NoSymbolFacts));
}

#[test]
fn excluded_exact_field_cannot_reappear_through_broad_pattern() {
    let ir = compile(
        r#"{
        "files":{"sample":{"path":"sample","root":"consumer"}},
        "schemas":{
            "consumer":{"fields":{"numeric":{"value":"keysof<domain,value_kind_any=(int|float)>","card":"0..1"},"fixed":{"value":"valuesof<domain,key=excluded,value_kind_any=(int|float)>","card":"0..1"}}},
            "domain":{"fields":{"excluded":{"value":"scalar","card":"0..*"}},"patterns":[{"key":"scalar","value":"int","card":"0..*"}]}
        }
    }"#,
    );
    assert!(!ir.scalar_matches(matcher(&ir, "numeric"), "excluded", &NoSymbolFacts));
    assert!(ir.scalar_matches(matcher(&ir, "numeric"), "other", &NoSymbolFacts));
    assert!(!ir.scalar_matches(matcher(&ir, "fixed"), "2", &NoSymbolFacts));
}

#[test]
fn schema_changes_immediately_change_projected_domains_and_hashes() {
    let source = r#"{"files":{"sample":{"path":"sample","root":"consumer"}},"schemas":{"consumer":{"fields":{"numeric":{"value":"keysof<domain,value_kind_any=(int)>","card":"0..1"}}},"domain":{"fields":{"item":{"value":"int","card":"0..*"}}}}}"#;
    let before = compile(source);
    let after = compile(&source.replace("\"value\":\"int\"", "\"value\":\"scalar\""));
    assert!(before.scalar_matches(matcher(&before, "numeric"), "item", &NoSymbolFacts));
    assert!(!after.scalar_matches(matcher(&after, "numeric"), "item", &NoSymbolFacts));
    assert_ne!(before.rule_hash(), after.rule_hash());
}

#[test]
fn selector_convenience_projects_original_fields() {
    let ir = fixture();
    let mut query = crate::query::FieldQuery::new(
        ir.schema_by_name("domain").unwrap(),
        QueryProjection::Values,
    );
    query.shape = Some(Shape::Scalar);
    let resolution = ir.query_selected_fields(&query, "boolean", &NoSymbolFacts, &NoQueryContext);
    assert_eq!(resolution.state, QueryState::Resolved);
    assert!(matches!(
        ir.matcher(
            ir.query_projected_matcher(&query, resolution.fields[0])
                .unwrap()
        ),
        Matcher::Bool
    ));
}

#[test]
fn query_lookup_preserves_exact_and_dynamic_shape_dispatch() {
    let ir = compile(
        r#"{
        "files":{"sample":{"path":"sample","root":"consumer"}},
        "types":{"member":{}},
        "schemas":{
            "consumer":{"fields":{
                "scalar":{"value":"keysof<domain,shape=scalar>","card":"0..1"},
                "block":{"value":"keysof<domain,shape=block>","card":"0..1"}
            }},
            "domain":{"fields":{"known":{"body":"leaf","card":"0..*"}},"patterns":[
                {"key":"ref<member>","value":"bool","card":"0..*"},
                {"key":"ref<member>","body":"leaf","card":"0..*"}
            ]},
            "leaf":{"open":true}
        }
    }"#,
    );
    // No exact scalar shape exists, so normal lookup falls through to patterns.
    assert!(ir.scalar_matches(matcher(&ir, "scalar"), "known", &Facts { complete: true }));
    assert!(ir.scalar_matches(matcher(&ir, "block"), "known", &Facts { complete: true }));
    let schema = ir.schema_by_name("domain").unwrap();
    let mut query = crate::query::FieldQuery::new(schema, QueryProjection::Keys);
    query.shape = Some(Shape::Block);
    // The block overload of a dynamic key is not shadowed by its scalar peer.
    let mut dynamic = ir.clone();
    let exact = dynamic.strings.lookup_folded("known").unwrap();
    dynamic.schemas[schema.index()].exact.remove(&exact);
    let result =
        dynamic.query_selected_fields(&query, "known", &Facts { complete: true }, &NoQueryContext);
    assert_eq!(result.state, QueryState::Resolved);
    assert_eq!(dynamic.shape(result.fields[0]), Some(Shape::Block));
}

#[test]
fn acyclic_query_chains_share_depth_and_work_budgets() {
    use crate::pattern::{SearchBudget, SearchLimit, SearchLimits};
    let mut schemas = serde_json::Map::new();
    for index in 0..80 {
        let value = if index == 79 {
            "bool".to_owned()
        } else {
            format!("valuesof<schema_{},key=item>", index + 1)
        };
        schemas.insert(
            format!("schema_{index}"),
            serde_json::json!({"fields":{"item":{"value":value,"card":"0..1"}}}),
        );
    }
    schemas.insert("consumer".into(), serde_json::json!({"fields":{"fixed":{"value":"valuesof<schema_0,key=item>","card":"0..1"}}}));
    let source = serde_json::json!({"files":{"sample":{"path":"sample","root":"consumer"}},"schemas":schemas});
    let ir = compile(&source.to_string());
    let id = matcher(&ir, "fixed");
    let mut no_cancel = || false;
    let mut budget = SearchBudget::new(SearchLimits::default(), &mut no_cancel);
    assert_eq!(
        crate::pattern::evaluate(&ir, id, "yes", &mut budget, &mut |_, value| Some(
            value == "yes"
        )),
        None
    );
    assert_eq!(budget.limit, Some(SearchLimit::Depth));
    let mut budget = SearchBudget::new(
        SearchLimits {
            depth: 128,
            ..Default::default()
        },
        &mut no_cancel,
    );
    assert_eq!(
        crate::pattern::evaluate(&ir, id, "yes", &mut budget, &mut |_, value| Some(
            value == "yes"
        )),
        Some(true)
    );
    let mut budget = SearchBudget::new(
        SearchLimits {
            depth: 128,
            work: 20,
            ..Default::default()
        },
        &mut no_cancel,
    );
    assert_eq!(
        crate::pattern::evaluate(&ir, id, "yes", &mut budget, &mut |_, value| Some(
            value == "yes"
        )),
        None
    );
    assert_eq!(budget.limit, Some(SearchLimit::Work));
    assert_eq!(ir.scalar_outcome(id, "yes", &NoSymbolFacts), None);
}
