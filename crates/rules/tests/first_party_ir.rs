//! Behavior contracts for the maintained source bundle, independent of the legacy converter.
use std::path::Path;
use std::sync::OnceLock;

use rules::ir::{
    DefName, FieldId, NoSymbolFacts, RootRule, RulesIr, SchemaId, Shape, SymbolFacts, TypeId,
};
use rules::source::ControlKind;
use text::LogicalPath;

fn ir() -> &'static RulesIr {
    static IR: OnceLock<RulesIr> = OnceLock::new();
    IR.get_or_init(|| {
        let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../rules/eu4-v2");
        let sources = rules::bundle::load_directory(&directory).expect("first-party source bundle");
        rules::lower::lower(&sources.files, sources.game).expect("checked first-party IR")
    })
}

fn schema(ir: &RulesIr, name: &str) -> SchemaId {
    ir.schema_by_name(name)
        .unwrap_or_else(|| panic!("schema {name}"))
}

fn exact(ir: &RulesIr, schema: SchemaId, key: &str, shape: Shape) -> FieldId {
    let symbol = ir.strings().lookup_folded(key).expect("field spelling");
    let fields = ir.schema(schema).exact.get(&symbol).expect("exact field");
    let mut candidates = fields
        .iter()
        .copied()
        .filter(|field| ir.shape(*field) == Some(shape));
    let field = candidates
        .next()
        .unwrap_or_else(|| panic!("{key}: {shape:?}"));
    assert!(candidates.next().is_none(), "unexpected overload for {key}");
    field
}

#[test]
fn predicates_iterators_and_weighted_branches_keep_their_behavior() {
    let ir = ir();
    let trigger = schema(ir, "trigger");
    let effect = schema(ir, "effect");
    let always = exact(ir, trigger, "always", Shape::Scalar);
    assert_eq!(
        ir.field(always).control.as_ref().unwrap().kind,
        ControlKind::Constant
    );
    assert!(
        ir.field(exact(ir, trigger, "is_capital", Shape::Scalar))
            .control
            .is_none()
    );

    let iterator = exact(ir, trigger, "any_country", Shape::Block);
    assert_eq!(ir.child(iterator, trigger), Some(trigger));
    assert_eq!(
        ir.field(iterator).scope.as_ref().unwrap().push,
        ir.strings().lookup_folded("country")
    );
    for key in ["every_country", "random_country"] {
        let field = exact(ir, effect, key, Shape::Block);
        assert_eq!(ir.field(field).card.min, 0, "optional invocation {key}");
        assert_eq!(
            ir.field(field).scope.as_ref().unwrap().push,
            ir.strings().lookup_folded("country")
        );
    }
    let any_country = ir.strings().lookup_folded("any_country").unwrap();
    assert!(!ir.schema(effect).exact.contains_key(&any_country));
    assert!(ir.scopes.link(ir.strings(), any_country).is_none());
    let owner = ir.strings().lookup_folded("owner").unwrap();
    assert!(ir.scopes.link(ir.strings(), owner).is_some());
    assert_eq!(
        ir.field(exact(ir, effect, "add_prestige", Shape::Scalar))
            .card
            .min,
        0
    );

    let weighted = schema(ir, "effect__random_list");
    let branch = ir
        .schema(weighted)
        .patterns
        .iter()
        .copied()
        .find(|field| ir.scalar_matches(ir.field(*field).key, "10", &NoSymbolFacts))
        .expect("weighted branch");
    let body = ir.child(branch, weighted).expect("weighted branch body");
    exact(ir, body, "trigger", Shape::Block);
    exact(ir, body, "modifier", Shape::Block);
    exact(ir, body, "add_prestige", Shape::Scalar);
}

#[test]
fn nested_definitions_use_their_actual_body_and_name_source() {
    let ir = ir();
    let missions = schema(ir, "mission_series_body");
    let mission_type = ir.type_by_name("mission").unwrap();
    let mission = ir
        .schema(missions)
        .patterns
        .iter()
        .copied()
        .find(|field| {
            ir.field(*field)
                .def
                .as_ref()
                .is_some_and(|def| def.type_id == mission_type)
        })
        .expect("mission definition");
    assert_eq!(ir.field(mission).def.as_ref().unwrap().name, DefName::Key);
    let body = ir.child(mission, missions).expect("direct mission body");
    // These fields must be immediately inside a mission, without a synthetic map wrapper.
    exact(ir, body, "trigger", Shape::Block);
    exact(ir, body, "effect", Shape::Block);
    exact(ir, body, "icon", Shape::Scalar);

    let path = LogicalPath::parse("interface/test.gfx").unwrap();
    let RootRule::Schema(root) = ir.root_rule(&path).expect("gfx root") else {
        panic!("gfx schema root")
    };
    let container_field = exact(ir, *root, "spriteTypes", Shape::Block);
    let container = ir.child(container_field, *root).unwrap();
    let sprite = exact(ir, container, "spriteType", Shape::Block);
    let definition = ir.field(sprite).def.as_ref().expect("sprite definition");
    assert_eq!(definition.type_id, ir.type_by_name("sprite").unwrap());
    assert_eq!(
        definition.name,
        DefName::Field(ir.strings().lookup_folded("name").unwrap())
    );
    let body = ir.child(sprite, container).unwrap();
    exact(ir, body, "name", Shape::Scalar);
    exact(ir, body, "texturefile", Shape::Scalar);

    let path = LogicalPath::parse("history/wars/test.txt").unwrap();
    let RootRule::Instance {
        def: Some(def),
        body: Some(body),
        ..
    } = ir.root_rule(&path).unwrap()
    else {
        panic!("war instance root")
    };
    assert_eq!(def.type_id, ir.type_by_name("war_history").unwrap());
    assert_eq!(def.name, DefName::File);
    exact(ir, *body, "name", Shape::Scalar);
}

struct PowerFacts(TypeId);

impl SymbolFacts for PowerFacts {
    fn type_member(&self, type_id: TypeId, name: &str) -> bool {
        type_id == self.0 && name == "test_power"
    }
}

#[test]
fn inherited_modifiers_and_numeric_trigger_domains_validate_values() {
    let ir = ir();
    let facts = PowerFacts(ir.type_by_name("government_mechanic_power").unwrap());
    for name in ["policy_body", "fervor_body"] {
        let body = schema(ir, name);
        let field = exact(ir, body, "global_tax_modifier", Shape::Scalar);
        let rules::ir::FieldValue::Scalar(value) = ir.field(field).value else {
            unreachable!()
        };
        assert!(ir.scalar_matches(value, "0.1", &NoSymbolFacts));
        assert!(!ir.scalar_matches(value, "invalid", &NoSymbolFacts));

        let template = ir
            .schema(body)
            .patterns
            .iter()
            .copied()
            .find(|field| ir.scalar_matches(ir.field(*field).key, "monthly_test_power", &facts))
            .expect("inherited typed modifier template");
        assert!(!ir.scalar_matches(ir.field(template).key, "monthly_unknown_power", &facts));
        let rules::ir::FieldValue::Scalar(value) = ir.field(template).value else {
            panic!("scalar modifier")
        };
        assert!(ir.scalar_matches(value, "1.5", &facts));
        assert!(!ir.scalar_matches(value, "yes", &facts));
    }

    let numeric = ir.enum_by_name("numeric_or_bool_trigger").unwrap();
    assert!(ir.enum_contains(numeric, "num_of_revolutionary_guard"));
    assert!(ir.enum_contains(numeric, "always"));
    assert!(!ir.enum_contains(numeric, "primary_culture"));
}
