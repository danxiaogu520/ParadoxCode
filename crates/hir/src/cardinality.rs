//! Field families that can impose a nontrivial occurrence constraint.
use rules::ir::{FieldId, RulesIr, SchemaId};
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};

pub(crate) fn families(ir: &RulesIr, schema: SchemaId) -> Cow<'_, [Vec<FieldId>]> {
    if let Some(families) = ir.constrained_field_families(schema) {
        return Cow::Borrowed(families);
    }
    let schema = ir.schema(schema);
    let fields = || {
        schema
            .exact
            .values()
            .flat_map(|ids| ids.iter())
            .chain(schema.patterns.iter())
            .copied()
    };
    let constrained = fields()
        .filter_map(|id| {
            let field = ir.field(id);
            (field.card.min > 0 || field.card.max.is_some()).then_some(field.key.index())
        })
        .collect::<BTreeSet<_>>();
    if constrained.is_empty() {
        return Cow::Borrowed(&[]);
    }
    let mut families = BTreeMap::<usize, Vec<FieldId>>::new();
    for id in fields() {
        let key = ir.field(id).key.index();
        if constrained.contains(&key) {
            families.entry(key).or_default().push(id);
        }
    }
    Cow::Owned(
        families
            .into_values()
            .map(|mut family| {
                // Scope filtering may leave only the bounded overloads, so retain the
                // unbounded alternatives of a constrained family too.
                family.sort_unstable();
                family.dedup();
                family
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constrained_families_preserve_every_overload_in_the_production_ir() {
        let ir = game::eu4::first_party_ir().unwrap();
        let _ = rules::ir::RulesIr::shared_rule_hash(&ir);
        let schemas = ir
            .schemas
            .iter()
            .flat_map(|schema| ir.schema_instances(ir.strings().resolve(schema.name)))
            .collect::<BTreeSet<_>>();
        assert_eq!(schemas.len(), ir.schemas.len());
        let mut skipped = 0;
        for schema in schemas {
            let mut reference = BTreeMap::<usize, Vec<FieldId>>::new();
            for id in ir.fields(schema) {
                reference
                    .entry(ir.field(id).key.index())
                    .or_default()
                    .push(id);
            }
            let all = reference.len();
            let reference = reference
                .into_values()
                .filter(|family| {
                    family.iter().any(|id| {
                        let card = ir.field(*id).card;
                        card.min > 0 || card.max.is_some()
                    })
                })
                .collect::<Vec<_>>();
            skipped += all - reference.len();
            assert_eq!(
                families(&ir, schema).as_ref(),
                reference.as_slice(),
                "{}",
                ir.strings().resolve(ir.schema(schema).name)
            );
        }
        assert!(
            skipped > 512,
            "the fixture must exercise unconstrained field families"
        );
    }
}
