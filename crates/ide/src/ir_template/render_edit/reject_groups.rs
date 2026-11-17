//! Reuses a rejection only for a proven semantic rename of one bare block key.
//! The field, scope transition, sibling observations and all other text agree.
use super::*;

pub(super) struct Normalized {
    text: String,
    pub(super) offset: usize,
}
pub(super) fn normalize(value: &str, label: &str) -> Option<Normalized> {
    if label.is_empty()
        || label.len() > 256
        || value.len() > 64 * 1024
        || !label
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b':' | b'.' | b'-'))
    {
        return None;
    }
    let mut positions = value.match_indices(label).filter_map(|(start, _)| {
        let before = value[..start].chars().next_back();
        let after = &value[start + label.len()..];
        (before.is_none_or(|c| c.is_whitespace() || matches!(c, '{' | '}'))
            && after
                .chars()
                .next()
                .is_some_and(|c| c.is_whitespace() || c == '=')
            && after.trim_start().starts_with('='))
        .then_some(start)
    });
    let offset = positions.next()?;
    if positions.next().is_some() {
        return None;
    }
    let mut text = value.to_owned();
    text.replace_range(offset..offset + label.len(), "\0");
    Some(Normalized { text, offset })
}

// The encoded carrier keeps the same bytes except one safe bare-key rename.
pub(super) fn encoded_key(value: &str, label: &str) -> Option<(String, usize)> {
    normalize(value, label).map(|key| (key.text, key.offset))
}

fn observed_keys(snapshot: &AnalysisSnapshot) -> Arc<BTreeSet<String>> {
    const KEY: &str = "template:observed-sibling-keys";
    if let Some(keys) = snapshot.query_cache().get(snapshot.revision(), KEY) {
        return keys;
    }
    let ir = snapshot.ir();
    let mut keys = BTreeSet::new();
    for matcher in ir.matchers.iter() {
        if let Matcher::Query(query) = matcher
            && let Some(rules::query::QuerySelector::Sibling(name)) = query.selector
        {
            keys.insert(ir.strings().resolve(name).to_ascii_lowercase());
        }
    }
    for field in ir.fields.iter() {
        if let Some(def) = &field.def
            && let rules::ir::DefName::Field(name) = def.name
        {
            keys.insert(ir.strings().resolve(name).to_ascii_lowercase());
        }
        if let Some(control) = &field.control {
            for name in [control.guard, control.on].into_iter().flatten() {
                keys.insert(ir.strings().resolve(name).to_ascii_lowercase());
            }
        }
    }
    for file in &ir.files {
        if let rules::ir::RootRule::Instance { def: Some(def), .. } = &file.root
            && let rules::ir::DefName::Field(name) = def.name
        {
            keys.insert(ir.strings().resolve(name).to_ascii_lowercase());
        }
    }
    let keys = Arc::new(keys);
    snapshot.query_cache().insert(
        snapshot.revision(),
        engine::CacheDomain::Index,
        KEY.into(),
        keys.clone(),
    );
    keys
}

#[derive(Clone)]
pub(super) struct Groups {
    observed: Arc<BTreeSet<String>>,
    entries: Vec<Arc<Group>>,
    syntax: Vec<Arc<SyntaxGroup>>,
    bytes: usize,
}
struct SyntaxGroup {
    signature: Normalized,
    markers: Vec<String>,
}
struct Group {
    signature: Normalized,
    markers: Vec<String>,
    field: rules::ir::FieldId,
    shape: rules::ir::Shape,
    schema: rules::ir::SchemaId,
    state: ScopeState,
    transition: ScopeState,
    coverage: hir::analysis::AnalysisCoverage,
}
impl Groups {
    pub(super) fn new(snapshot: &AnalysisSnapshot) -> Self {
        Self {
            observed: observed_keys(snapshot),
            entries: Vec::new(),
            syntax: Vec::new(),
            bytes: 0,
        }
    }
    pub(super) fn syntax_reuse(&self, key: &Normalized, markers: &[String]) -> bool {
        self.syntax.iter().any(|entry| {
            entry.signature.text == key.text
                && entry.signature.offset == key.offset
                && entry.markers == markers
        })
    }
    fn rename_allowed(
        &self,
        snapshot: &AnalysisSnapshot,
        label: &str,
        schema: rules::ir::SchemaId,
        field: rules::ir::FieldId,
        shape: rules::ir::Shape,
    ) -> bool {
        let ir = snapshot.ir();
        if self.observed.contains(&label.to_ascii_lowercase())
            || ir.lookup_exact(schema, label, shape).next().is_some()
        {
            return false;
        }
        let mut fields =
            hir::checking::field_candidates(ir, schema, label, shape, &WorkspaceFacts { snapshot })
                .into_iter();
        fields.next() == Some(field) && fields.next().is_none()
    }
    pub(super) fn reuse(
        &self,
        snapshot: &AnalysisSnapshot,
        key: &Normalized,
        label: &str,
        markers: &[String],
    ) -> Option<hir::analysis::AnalysisCoverage> {
        for group in &self.entries {
            if group.signature.text != key.text
                || group.signature.offset != key.offset
                || group.markers != markers
            {
                continue;
            }
            if !self.rename_allowed(snapshot, label, group.schema, group.field, group.shape) {
                continue;
            }
            let transition = hir::transition_ir_scope(
                snapshot.ir(),
                group.state.clone(),
                snapshot.ir().field(group.field).scope.as_ref(),
                label,
            );
            if transition == group.transition {
                return Some(group.coverage.clone());
            }
        }
        None
    }
    #[allow(clippy::too_many_arguments)]
    pub(super) fn remember(
        &mut self,
        snapshot: &AnalysisSnapshot,
        key: Normalized,
        label: &str,
        markers: &[String],
        body: &BodyAnalysis,
        parameter: &str,
        piece: usize,
    ) {
        // In the protocol goal a known frontier already fixes isIncomplete.
        // A bare-key rename preserves every physical syntax witness, provided
        // neither spelling can invoke a Template (proven by RenderEdit).
        let dependent_syntax = body.evidence.iter().any(|e| {
            e.kind == hir::checking::IssueKind::Syntax
                && evidence_dependencies(body, e).contains(&parameter.to_ascii_lowercase())
        });
        let is_property = body
            .rendered
            .pieces
            .get(piece)
            .and_then(|piece| {
                text::TextRange::new(
                    piece.range.start() + key.offset as u32,
                    piece.range.start() + (key.offset + label.len()) as u32,
                )
            })
            .and_then(|range| body.hir.property_at_key_range(range))
            .is_some_and(|p| p.key == label);
        if dependent_syntax
            && is_property
            && self.syntax.len() < 32
            && self.bytes.saturating_add(key.text.len()) < 4 * 1024 * 1024
        {
            self.bytes += key.text.len();
            self.syntax.push(Arc::new(SyntaxGroup {
                signature: Normalized {
                    text: key.text.clone(),
                    offset: key.offset,
                },
                markers: markers.to_vec(),
            }));
        }
        if self.entries.len() >= 32
            || !body.coverage.is_complete()
            || body.rendered.text.len() > 64 * 1024
            || key.text.len().saturating_mul(2).saturating_add(self.bytes) > 4 * 1024 * 1024
        {
            return;
        }
        // Only a rejection supported by physical syntax supplied by this binding
        // is shared. Accepted/unknown items always retain their own witnesses.
        if !body.evidence.iter().any(|e| {
            e.kind == hir::checking::IssueKind::Syntax
                && evidence_dependencies(body, e).contains(&parameter.to_ascii_lowercase())
        }) {
            return;
        }
        let Some(piece) = body.rendered.pieces.get(piece) else {
            return;
        };
        let Some(range) = text::TextRange::new(
            piece.range.start() + key.offset as u32,
            piece.range.start() + (key.offset + label.len()) as u32,
        ) else {
            return;
        };
        let Some(property) = body
            .hir
            .property_at_key_range(range)
            .filter(|p| p.key == label)
        else {
            return;
        };
        if body
            .hir
            .definitions()
            .iter()
            .any(|def| def.range.start() <= range.start() && range.end() <= def.range.end())
        {
            return;
        }
        if body
            .hir
            .properties()
            .iter()
            .filter(|p| p.range.start() <= range.start() && range.end() <= p.range.end())
            .any(|p| {
                body.hir.field_fact_at(p.key_range).is_some_and(|fact| {
                    fact.fields
                        .iter()
                        .any(|id| ir_has_sensitive_control(snapshot.ir().field(*id)))
                })
            })
        {
            return;
        }
        let Some(fact) = body.hir.field_fact_at(property.key_range) else {
            return;
        };
        let [field] = fact.fields.as_slice() else {
            return;
        };
        let ir = snapshot.ir();
        let shape = if property.scalar.is_some() {
            rules::ir::Shape::Scalar
        } else {
            rules::ir::Shape::Block
        };
        let rule = ir.field(*field);
        if rule.def.is_some()
            || rule.control.is_some()
            || template_kind(ir, rule.key).is_some()
            || !matches!(
                ir.matcher(rule.key),
                Matcher::Ref(_) | Matcher::Scope(_) | Matcher::Link
            )
            || ir
                .schema(fact.schema)
                .patterns
                .iter()
                .any(|id| ir_semantic::has_field_query(ir, ir.field(*id).key))
            || !self.rename_allowed(snapshot, label, fact.schema, *field, shape)
        {
            return;
        }
        let Some(scope) = body
            .hir
            .scope_facts()
            .iter()
            .find(|scope| scope.range == range)
        else {
            return;
        };
        let Some(transition) = &scope.transition else {
            return;
        };
        self.bytes += key.text.len().saturating_mul(2) + 1024;
        self.entries.push(Arc::new(Group {
            signature: key,
            markers: markers.to_vec(),
            field: *field,
            shape,
            schema: fact.schema,
            state: scope.state.clone(),
            transition: transition.clone(),
            coverage: body.coverage.clone(),
        }));
    }
}

fn ir_has_sensitive_control(field: &rules::ir::Field) -> bool {
    field.control.as_ref().is_some_and(|control| {
        matches!(
            control.kind,
            rules::source::ControlKind::Switch
                | rules::source::ControlKind::Weighted
                | rules::source::ControlKind::Chance
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn rendered(text: String) -> hir::template_text::RenderedTemplate {
        let range = text::TextRange::new(0, text.len() as u32).unwrap();
        hir::template_text::RenderedTemplate {
            text,
            pieces: vec![hir::template_text::SourcePiece {
                origin: ("scripted_effect".into(), "test".into()),
                range,
                definition_range: range,
                script: true,
                parameters: BTreeSet::from(["body".into()]),
                binding_name: Some("body".into()),
                binding_source: None,
                hole: false,
                uncertain_text: false,
                structural_hole: false,
            }],
            ..Default::default()
        }
    }
    #[test]
    fn grouped_rejection_matches_full_checks_and_refuses_other_semantics() {
        let mut ir = (*game::eu4::first_party_ir().unwrap()).clone();
        for (kind, names) in [
            ("unit_type", vec!["unit_alpha", "unit_beta"]),
            ("province_id", vec!["10001", "10002"]),
        ] {
            let ty = ir.type_by_name(kind).unwrap();
            let names = names
                .into_iter()
                .map(|name| ir.strings.intern_verbatim(name))
                .collect::<Vec<_>>();
            ir.types[ty.index()].builtin = names.into_boxed_slice();
        }
        let ir = Arc::new(ir);
        let host = engine::AnalysisHost::with_ir(
            game::eu4::runtime_rules().unwrap(),
            game::eu4::profile(),
            ir,
        );
        let snapshot = host.snapshot();
        let facts = WorkspaceFacts {
            snapshot: &snapshot,
        };
        let ir = snapshot.ir();
        let schema =
            hir::template::template_body(ir, ir.type_by_name("scripted_effect").unwrap()).unwrap();
        let state = ScopeState::initial(hir::ScopeValue::known_single("country"));
        let markers = vec!["__hole".into()];
        for (old, new, tail) in [
            ("unit_alpha", "unit_beta", " = __hole = { size = 1 }"),
            ("10001", "10002", " = { __hole } = { size = 1 }"),
        ] {
            let value = format!("{old}{tail}");
            let body = hir::template_instance::instantiate_rendered_no_calls(
                ir,
                &facts,
                rendered(value.clone()),
                schema,
                state.clone(),
                &markers,
                &mut |_| false,
                &mut || Ok::<(), ()>(()),
            )
            .unwrap()
            .unwrap();
            let mut groups = Groups::new(&snapshot);
            groups.remember(
                &snapshot,
                normalize(&value, old).unwrap(),
                old,
                &markers,
                &body,
                "body",
                0,
            );
            assert_eq!(groups.entries.len(), 1, "{old}: {:?}", body.evidence);
            let candidate = format!("{new}{tail}");
            let coverage = groups
                .reuse(
                    &snapshot,
                    &normalize(&candidate, new).unwrap(),
                    new,
                    &markers,
                )
                .expect("proven equivalent rejection");
            let full = hir::template_instance::instantiate_rendered_no_calls(
                ir,
                &facts,
                rendered(candidate),
                schema,
                state.clone(),
                &markers,
                &mut |_| false,
                &mut || Ok::<(), ()>(()),
            )
            .unwrap()
            .unwrap();
            assert_eq!(coverage, full.coverage);
            assert!(
                full.evidence
                    .iter()
                    .any(|e| e.kind == hir::checking::IssueKind::Syntax)
            );
            for different in ["add_prestige", "ROOT", "missing_member"] {
                assert!(
                    groups
                        .reuse(
                            &snapshot,
                            &normalize(&format!("{different}{tail}"), different).unwrap(),
                            different,
                            &markers
                        )
                        .is_none()
                );
            }
            assert!(
                groups
                    .reuse(
                        &snapshot,
                        &normalize(&format!("{new}{tail} "), new).unwrap(),
                        new,
                        &markers
                    )
                    .is_none()
            );
        }
        for label in ["$dynamic$", "quoted name", "a=b", "", "weird/label"] {
            assert!(normalize(&format!("{label} = {{}}"), label).is_none());
        }
    }
}
