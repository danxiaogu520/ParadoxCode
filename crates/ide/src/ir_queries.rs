//! Workspace facts for schema lowering, independent of completion preferences.
use engine::{AnalysisSnapshot, DocumentSource};
use rules::ir::{Symbol, SymbolFacts, TypeId};
use std::fmt::Write;
use std::sync::{Arc, Mutex};

const ENVIRONMENT_BYTES: usize = 8 * 1024 * 1024;
#[derive(Default)]
struct MemoEnvironments(Mutex<Vec<(String, Arc<rules::template::TemplateMemo>)>>);
struct EnvironmentIdentity(String);
impl Write for EnvironmentIdentity {
    fn write_str(&mut self, text: &str) -> std::fmt::Result {
        if self.0.len().saturating_add(text.len()) > ENVIRONMENT_BYTES {
            return Err(std::fmt::Error);
        }
        self.0.push_str(text);
        Ok(())
    }
}
fn environment_identity(snapshot: &AnalysisSnapshot) -> Option<String> {
    let mut identity = EnvironmentIdentity(String::new());
    write!(
        identity,
        "textures:{};",
        snapshot.texture_catalog_generation()
    )
    .ok()?;
    for doc in snapshot
        .documents()
        .values()
        .filter(|doc| doc.source() == DocumentSource::Overlay)
    {
        write!(
            identity,
            "overlay:{:?}:{:?}:{:?};",
            doc.id(),
            doc.path(),
            doc.fact_coverage
        )
        .ok()?;
        let hir = doc.hir()?;
        for definition in hir.definitions() {
            // Membership reads have no dependence on caller source positions.
            write!(
                identity,
                "member:{:?}:{:?};",
                definition.kind, definition.name
            )
            .ok()?;
        }
        for attributes in hir.definition_attributes() {
            write!(
                identity,
                "attributes:{:?}:{:?}:{:?}:{:?};",
                attributes.kind, attributes.name, attributes.attribute_keys, attributes.subtypes
            )
            .ok()?;
        }
        for template in hir.dynamic_templates() {
            // Source and ranges determine both program semantics and definition-side maps.
            write!(
                identity,
                "body:{:?}:{:?}:{:?}:{:?}:{:?};",
                template.kind,
                template.name,
                template.source,
                template.definition_range,
                template.body_range
            )
            .ok()?;
        }
    }
    Some(identity.0)
}

pub(crate) struct SnapshotSymbolFacts<'a> {
    pub(crate) snapshot: &'a AnalysisSnapshot,
}
impl SymbolFacts for SnapshotSymbolFacts<'_> {
    fn facts_complete(&self) -> bool {
        self.snapshot
            .documents()
            .values()
            .all(|doc| doc.fact_coverage.is_known())
    }
    fn asset_member(&self, category: &str, name: &str) -> Option<bool> {
        category
            .eq_ignore_ascii_case("gfx")
            .then(|| self.snapshot.resolve_texture_path(name).is_some())
    }
    fn template_memo(&self) -> Option<std::sync::Arc<rules::template::TemplateMemo>> {
        const KEY: &str = "template:semantic-memo";
        let cache = self.snapshot.query_cache();
        let revision = self.snapshot.revision();
        if let Some(value) = cache.get::<rules::template::TemplateMemo>(revision, KEY) {
            return Some(value);
        }
        const POOL: &str = "template:environment-memos";
        let pool = cache
            .get::<MemoEnvironments>(revision, POOL)
            .unwrap_or_else(|| {
                let pool = Arc::new(MemoEnvironments::default());
                cache.insert(
                    revision,
                    engine::CacheDomain::Index,
                    POOL.to_owned(),
                    pool.clone(),
                );
                // Concurrent misses use the one published pool; old snapshots can use a private one.
                cache.get(revision, POOL).unwrap_or(pool)
            });
        let value = if let Some(identity) = environment_identity(self.snapshot) {
            let mut entries = pool.0.lock().ok()?;
            if let Some((_, memo)) = entries.iter().find(|(key, _)| key == &identity) {
                memo.clone()
            } else {
                let bytes = entries.iter().map(|(key, _)| key.len()).sum::<usize>();
                if entries.len() >= 4 || bytes.saturating_add(identity.len()) > ENVIRONMENT_BYTES {
                    entries.clear();
                }
                let memo = Arc::new(rules::template::TemplateMemo::default());
                entries.push((identity, memo.clone()));
                memo
            }
        } else {
            Arc::new(rules::template::TemplateMemo::default())
        };
        cache.insert(
            revision,
            engine::CacheDomain::Documents,
            KEY.to_owned(),
            value.clone(),
        );
        Some(value)
    }

    fn template(
        &self,
        type_id: TypeId,
        name: &str,
    ) -> Option<std::sync::Arc<rules::template::Template>> {
        let kind = self
            .snapshot
            .ir()
            .strings
            .resolve(self.snapshot.ir().type_info(type_id).name);
        crate::semantic::resolve_dynamic_definition(self.snapshot, kind, name)?
            .summary
            .template
            .map(std::sync::Arc::new)
    }

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
            || template_member(snapshot, kind, name)
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
}

/// Scalar `def<T>` sites inside parameterised scripts can name runtime
/// expansions. Retain their reachable name patterns without inventing literal
/// definitions or falling back to a game-specific write-command table.
fn template_member(snapshot: &AnalysisSnapshot, kind: &str, name: &str) -> bool {
    type Templates = std::collections::BTreeMap<
        String,
        std::collections::BTreeMap<engine::SourceFileId, engine::FlagWriteIndex>,
    >;
    let revision = snapshot.revision();
    const KEY: &str = "ir-symbol-templates";
    let templates = snapshot
        .query_cache()
        .get::<Templates>(revision, KEY)
        .unwrap_or_else(|| {
            let mut templates = Templates::new();
            for (definition, active) in snapshot.index().definition_identities() {
                if active && definition.name.contains('$') {
                    templates
                        .entry(definition.kind.to_ascii_lowercase())
                        .or_default()
                        .entry(definition.file_id)
                        .or_default()
                        .record(&definition.name);
                }
            }
            let templates = std::sync::Arc::new(templates);
            snapshot.query_cache().insert(
                revision,
                // Only immutable index patterns are cached. Overlay patterns
                // and hidden files are evaluated against the current snapshot.
                engine::CacheDomain::Index,
                KEY.to_owned(),
                templates.clone(),
            );
            templates
        });
    let overlays = snapshot
        .documents()
        .values()
        .filter(|document| document.source() == DocumentSource::Overlay)
        .collect::<Vec<_>>();
    let hidden = overlays
        .iter()
        .filter_map(|document| {
            document
                .path()
                .and_then(|path| snapshot.source_file_id_for_path(path))
        })
        .collect::<std::collections::BTreeSet<_>>();
    templates
        .get(&kind.to_ascii_lowercase())
        .is_some_and(|files| {
            files.iter().any(|(file, patterns)| {
                !hidden.contains(file)
                    && patterns.membership(name) != engine::FlagWriteMembership::Unknown
            })
        })
        || overlays
            .iter()
            .filter_map(|document| document.hir())
            .any(|hir| {
                hir.definitions()
                    .iter()
                    .filter(|definition| {
                        definition.kind.eq_ignore_ascii_case(kind) && definition.name.contains('$')
                    })
                    .any(|definition| {
                        let mut pattern = engine::FlagWriteIndex::default();
                        pattern.record(&definition.name);
                        pattern.membership(name) != engine::FlagWriteMembership::Unknown
                    })
            })
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
