//! Workspace facts for schema lowering, independent of completion preferences.
use engine::{AnalysisSnapshot, DocumentSource};
use rules::ir::{Symbol, SymbolFacts, TraitId, TypeId};

pub(crate) struct SnapshotSymbolFacts<'a> {
    pub(crate) snapshot: &'a AnalysisSnapshot,
}
impl SymbolFacts for SnapshotSymbolFacts<'_> {
    fn type_member(&self, type_id: TypeId, name: &str) -> bool {
        let snapshot = self.snapshot;
        let ir = snapshot.ir();
        let info = ir.type_info(type_id);
        if info.open
            || info
                .builtin
                .iter()
                .any(|member| ir.strings.resolve(*member).eq_ignore_ascii_case(name))
        {
            return true;
        }
        let kind = ir.strings.resolve(info.name);
        let overlays = snapshot
            .documents()
            .values()
            .filter(|document| document.source() == DocumentSource::Overlay)
            .collect::<Vec<_>>();
        if overlays
            .iter()
            .filter_map(|document| document.hir())
            .any(|hir| {
                hir.definitions().iter().any(|definition| {
                    definition.kind.eq_ignore_ascii_case(kind)
                        && definition.name.eq_ignore_ascii_case(name)
                })
            })
        {
            return true;
        }
        snapshot
            .index()
            .definitions_with_state(kind, name)
            .into_iter()
            .filter(|(_, active)| *active)
            .any(|(definition, _)| {
                !overlays.iter().any(|document| {
                    document
                        .path()
                        .and_then(|path| snapshot.source_file_id_for_path(path))
                        == Some(definition.file_id)
                })
            })
    }
    fn type_subtype_member(&self, type_id: TypeId, subtype: Symbol, name: &str) -> bool {
        let snapshot = self.snapshot;
        let ir = snapshot.ir();
        let kind = ir.strings.resolve(ir.type_info(type_id).name);
        let subtype = ir.strings.resolve(subtype);
        let overlays = snapshot
            .documents()
            .values()
            .filter(|document| document.source() == DocumentSource::Overlay)
            .collect::<Vec<_>>();
        if overlays
            .iter()
            .filter_map(|document| document.hir())
            .any(|hir| {
                hir.definition_attributes().iter().any(|attributes| {
                    attributes.kind.eq_ignore_ascii_case(kind)
                        && attributes.name.eq_ignore_ascii_case(name)
                        && attributes
                            .subtypes
                            .iter()
                            .any(|known| known.eq_ignore_ascii_case(subtype))
                })
            })
        {
            return true;
        }
        snapshot
            .index()
            .definitions_with_state(kind, name)
            .into_iter()
            .filter(|(_, active)| *active)
            .any(|(definition, _)| {
                if overlays.iter().any(|document| {
                    document
                        .path()
                        .and_then(|path| snapshot.source_file_id_for_path(path))
                        == Some(definition.file_id)
                }) {
                    return false;
                }
                snapshot
                    .index()
                    .shard(definition.file_id)
                    .is_some_and(|shard| {
                        shard.definition_attributes.iter().any(|attributes| {
                            attributes.kind.eq_ignore_ascii_case(kind)
                                && attributes.name.eq_ignore_ascii_case(name)
                                && attributes.definition_range == definition.range
                                && attributes
                                    .subtypes
                                    .iter()
                                    .any(|known| known.eq_ignore_ascii_case(subtype))
                        })
                    })
            })
    }
    fn trait_impl_member(&self, trait_id: TraitId, name: &str) -> bool {
        let ir = self.snapshot.ir();
        ir.types.iter().any(|info| {
            let Some(id) = ir.type_by_name(ir.strings.resolve(info.name)) else {
                return false;
            };
            (info
                .trait_impls
                .iter()
                .any(|implementation| implementation.trait_id == trait_id)
                && self.type_member(id, name))
                || info.subtypes.iter().any(|subtype| {
                    subtype
                        .trait_impls
                        .iter()
                        .any(|implementation| implementation.trait_id == trait_id)
                        && self.type_subtype_member(id, subtype.name, name)
                })
        })
    }
}

/// Modifier applications use the referenced type's schema and retained attributes.
pub(crate) fn modifier_scope_diagnostics(
    snapshot: &AnalysisSnapshot,
    input: &crate::support::ParsedInput,
    cancellation: &crate::CancellationToken,
) -> Result<Vec<crate::Diagnostic>, crate::Cancelled> {
    use hir::ScopeValue;
    use rules::ir::{FieldValue, Matcher, MatcherId, RefTarget, Shape, TypeId};
    fn targets(ir: &rules::ir::RulesIr, matcher: MatcherId, out: &mut Vec<TypeId>) {
        match ir.matcher(matcher) {
            Matcher::Ref(RefTarget::Type { type_id, .. }) => out.push(*type_id),
            Matcher::Union(items) => {
                for item in items {
                    targets(ir, *item, out);
                }
            }
            _ => {}
        }
    }
    let Some(hir) = input.hir.as_deref() else {
        return Ok(Vec::new());
    };
    let ir = snapshot.ir();
    let Some(modifier_trait) = ir.trait_by_name("ModifierSource") else {
        return Ok(Vec::new());
    };
    let mut diagnostics = Vec::new();
    for property in hir.properties() {
        cancellation.checkpoint()?;
        let (Some(scalar), Some(field_fact), Some(scope_fact)) = (
            &property.scalar,
            hir.field_fact_at(property.key_range),
            hir.scope_fact_at(property.key_range),
        ) else {
            continue;
        };
        if scalar.value.contains('$') {
            continue;
        }
        let Some(ScopeValue::Known(current)) = scope_fact.state.current.first() else {
            continue;
        };
        if current.len() != 1 {
            continue;
        }
        let application = current[0].as_ref();
        for field in &field_fact.fields {
            let FieldValue::Scalar(matcher) = ir.field(*field).value else {
                continue;
            };
            let mut types = Vec::new();
            targets(ir, matcher, &mut types);
            for type_id in types {
                let info = ir.type_info(type_id);
                if !info
                    .trait_impls
                    .iter()
                    .any(|implementation| implementation.trait_id == modifier_trait)
                {
                    continue;
                }
                let kind = ir.strings.resolve(info.name);
                let attributes = snapshot
                    .documents()
                    .values()
                    .filter(|document| document.source() == DocumentSource::Overlay)
                    .filter_map(|document| document.hir())
                    .find_map(|hir| {
                        hir.definition_attributes().iter().find(|attributes| {
                            attributes.kind.eq_ignore_ascii_case(kind)
                                && attributes.name.eq_ignore_ascii_case(&scalar.value)
                        })
                    })
                    .or_else(|| {
                        snapshot
                            .index()
                            .active_definition_attributes(kind, &scalar.value)
                    });
                let Some(attributes) = attributes else {
                    continue;
                };
                let mut incompatible = std::collections::BTreeMap::<String, Vec<String>>::new();
                for key in &attributes.attribute_keys {
                    let mut allowed = Vec::new();
                    for schema in ir
                        .fields
                        .iter()
                        .filter(|field| {
                            field.def.as_ref().is_some_and(|def| def.type_id == type_id)
                        })
                        .filter_map(|field| match field.value {
                            FieldValue::Block(id) => Some(ir.schema(id)),
                            _ => None,
                        })
                    {
                        if let Some(fields) = ir
                            .strings
                            .lookup_folded(key)
                            .and_then(|symbol| schema.exact.get(&symbol))
                        {
                            for field_id in fields
                                .iter()
                                .filter(|id| ir.shape(**id) == Some(Shape::Scalar))
                            {
                                if let Some(scope) = &ir.field(*field_id).scope {
                                    allowed.extend(
                                        scope
                                            .scopes_in
                                            .iter()
                                            .map(|scope| ir.strings.resolve(*scope).to_owned()),
                                    );
                                }
                            }
                        }
                    }
                    allowed.sort();
                    allowed.dedup();
                    if !allowed.is_empty()
                        && !allowed.iter().any(|scope| {
                            scope == "any"
                                || scope.eq_ignore_ascii_case(application)
                                || ir
                                    .strings
                                    .lookup_folded(application)
                                    .zip(ir.strings.lookup_folded(scope))
                                    .is_some_and(|(actual, expected)| {
                                        ir.scopes_compatible(actual, expected)
                                    })
                        })
                    {
                        incompatible
                            .entry(allowed.join("/"))
                            .or_default()
                            .push(key.to_string());
                    }
                }
                for (class, keys) in incompatible {
                    diagnostics.push(crate::Diagnostic::new(crate::DiagnosticCode::ModifierScopeMismatch, crate::Severity::Information, scalar.range,
                        format!("modifier `{}` applies {class}-class attributes ({}) in {application} scope", scalar.value, keys.join(", "))));
                }
            }
        }
    }
    Ok(diagnostics)
}
