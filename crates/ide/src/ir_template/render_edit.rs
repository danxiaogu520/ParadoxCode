//! Incremental rendering for direct script insertions with no invocation edges.
//! Source maps and complete constraint checking are retained for every trial.
use super::*;
pub(crate) mod boundary;
mod reject_groups;
pub(super) enum RenderValidation {
    Body(Arc<BodyAnalysis>),
    Rejected(hir::analysis::AnalysisCoverage),
}
use rules::template::{TemplateInstruction, TemplateOperand};
use std::sync::Arc;

#[derive(Clone)]
pub(super) struct RenderEdit<'a> {
    groups: reject_groups::Groups,
    encoded: BTreeSet<(text::TextRange, bool, String, usize)>,
    encoded_bytes: usize,
    syntax_epoch: usize,
    #[cfg(test)]
    encoded_hits: usize,
    parameter: String,
    session: hir::template_instance::FragmentSession<'a, WorkspaceFacts<'a>>,
    source: Arc<hir::template_text::RenderedTemplate>,
    replacements: BTreeSet<usize>,
    schema: rules::ir::SchemaId,
    state: ScopeState,
    fixed_bytes: usize,
    reads: usize,
    matchers: Arc<Vec<MatcherId>>,
    can_call: BTreeMap<String, bool>,
}

fn call_matchers(snapshot: &AnalysisSnapshot) -> Arc<Vec<MatcherId>> {
    const KEY: &str = "template:call-matchers";
    if let Some(value) = snapshot.query_cache().get(snapshot.revision(), KEY) {
        return value;
    }
    let value = Arc::new(
        snapshot
            .ir()
            .fields
            .iter()
            .filter_map(|field| template_kind(snapshot.ir(), field.key).map(|_| field.key))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>(),
    );
    snapshot.query_cache().insert(
        snapshot.revision(),
        engine::CacheDomain::Index,
        KEY.into(),
        value.clone(),
    );
    value
}

impl<'a> RenderEdit<'a> {
    pub(super) fn new(
        snapshot: &'a AnalysisSnapshot,
        input: &crate::support::ParsedInput,
        projection: &ConsumptionProjection,
    ) -> Option<Self> {
        #[cfg(test)]
        if !super::REUSE_RENDERING.get() {
            return None;
        }
        let scalar = projection.scalar.as_ref().filter(|scalar| scalar.quoted)?;
        projection.quote_map.as_ref()?;
        let original = input.hir.as_deref()?;
        let invocation = original.property_at_key_range(projection.invocation)?;
        let arguments = original
            .properties_in_range(invocation.range)
            .filter(|arg| {
                arg.path.len() == invocation.path.len() + 1
                    && arg.path.starts_with(&invocation.path)
            })
            .collect::<Vec<_>>();
        // Shadowed duplicate bindings cannot be edited by replacing the active use.
        if arguments
            .iter()
            .filter(|arg| arg.key.eq_ignore_ascii_case(&projection.parameter))
            .count()
            != 1
        {
            return None;
        }
        let field = original.field_fact_at(invocation.key_range)?;
        let kind = field
            .fields
            .iter()
            .find_map(|id| template_kind(snapshot.ir(), snapshot.ir().field(*id).key))?;
        let ty = snapshot.ir().type_by_name(&kind)?;
        let template =
            rules::ir::SymbolFacts::template(&WorkspaceFacts { snapshot }, ty, &invocation.key)?;
        let rendered = &projection.body.rendered;
        if !rendered.calls.is_empty()
            || !rendered.trial_holes.is_empty()
            || !rendered.unresolved_bindings.is_empty()
            || !rendered.frontiers.is_empty()
            || !rendered.coverage.is_complete()
        {
            return None;
        }
        let lengths = arguments
            .iter()
            .filter_map(|arg| {
                let scalar = arg.scalar.as_ref()?;
                Some((
                    arg.key.to_ascii_lowercase(),
                    original
                        .syntax()
                        .text(scalar.range)?
                        .len()
                        .max(scalar.value.len()),
                ))
            })
            .collect::<BTreeMap<_, _>>();
        let mut reads = 0usize;
        let mut fixed_bytes = template.source.len();
        let mut static_keys = Vec::new();
        let mut uses = BTreeSet::new();
        for block in &template.program.blocks {
            for instruction in block.iter() {
                let mut inspect = |token: &TemplateToken, insertion: bool| -> Option<()> {
                    for part in &token.fragments {
                        if let TemplateFragment::Parameter { name, range } = part {
                            if name.eq_ignore_ascii_case(&projection.parameter) {
                                if !insertion || token.quoted || token.fragments.len() != 1 {
                                    return None;
                                }
                                uses.insert(*range);
                                reads += 1;
                            } else {
                                fixed_bytes = fixed_bytes.saturating_add(
                                    lengths
                                        .get(&name.to_ascii_lowercase())
                                        .copied()
                                        .unwrap_or(0)
                                        .saturating_mul(4)
                                        .saturating_add(32),
                                );
                            }
                        }
                    }
                    Some(())
                };
                match instruction {
                    TemplateInstruction::Consume(token) => inspect(token, true)?,
                    TemplateInstruction::Recover(token) => inspect(token, false)?,
                    TemplateInstruction::When { .. } => {}
                    TemplateInstruction::Dispatch(property) => {
                        if property.key.quoted
                            || property
                                .key
                                .fragments
                                .iter()
                                .any(|p| matches!(p, TemplateFragment::Parameter { .. }))
                        {
                            return None;
                        }
                        static_keys.push(
                            property
                                .key
                                .fragments
                                .iter()
                                .filter_map(|p| match p {
                                    TemplateFragment::Literal(value) => Some(value.as_str()),
                                    _ => None,
                                })
                                .collect::<String>(),
                        );
                        if let TemplateOperand::Scalar(token) = &property.value {
                            inspect(token, false)?;
                        }
                    }
                }
            }
        }
        if reads == 0 {
            return None;
        }
        let mut replacements = BTreeSet::new();
        let mut end = 0;
        for (i, piece) in rendered.pieces.iter().enumerate() {
            if piece.range.start() != end
                || piece.origin.0 != template.kind.as_ref()
                || piece.origin.1 != template.name
            {
                return None;
            }
            end = piece.range.end();
            if piece.binding_name.is_some() && piece.binding_source.is_none() {
                return None;
            }
            if piece
                .parameters
                .contains(&projection.parameter.to_ascii_lowercase())
            {
                let map = piece.binding_source.as_ref()?;
                if !piece.script
                    || !piece
                        .binding_name
                        .as_ref()
                        .is_some_and(|name| name.eq_ignore_ascii_case(&projection.parameter))
                    || !map.parameter.eq_ignore_ascii_case(&projection.parameter)
                    || map.quote_layers != 0
                    || map.borrowed_carrier
                    || !uses.contains(&piece.definition_range)
                    || rendered
                        .text
                        .get(piece.range.start() as usize..piece.range.end() as usize)
                        != Some(scalar.value.as_str())
                    || map.offsets.len() != scalar.value.len() + 1
                    || !map
                        .offsets
                        .iter()
                        .enumerate()
                        .all(|(i, offset)| *offset as usize == i)
                {
                    return None;
                }
                replacements.insert(i);
            }
        }
        if end as usize != rendered.text.len() || replacements.is_empty() {
            return None;
        }
        let mut plan = Self {
            groups: reject_groups::Groups::new(snapshot),
            encoded: BTreeSet::new(),
            encoded_bytes: 0,
            syntax_epoch: 0,
            #[cfg(test)]
            encoded_hits: 0,
            parameter: projection.parameter.to_ascii_lowercase(),
            session: hir::template_instance::FragmentSession::new(
                snapshot.ir(),
                WorkspaceFacts { snapshot },
                rendered.text.clone().into(),
            ),
            source: Arc::new(rendered.clone()),
            replacements,
            schema: hir::template::template_body(snapshot.ir(), ty)?,
            state: invocation_state(original, invocation),
            fixed_bytes,
            reads,
            matchers: call_matchers(snapshot),
            can_call: BTreeMap::new(),
        };
        // No fixed call may erase a source span before the direct insertion.
        if static_keys.iter().any(|key| plan.can_call(snapshot, key)) {
            return None;
        }
        Some(plan)
    }

    pub(super) fn syntax_epoch(&self) -> usize {
        self.syntax_epoch
    }
    #[cfg(test)]
    pub(super) fn encoded_hits(&self) -> usize {
        self.encoded_hits
    }
    pub(super) fn remember_encoded(&mut self, item: &crate::CompletionItem) {
        if self.encoded.len() >= 32 {
            return;
        }
        let Some((text, offset)) = reject_groups::encoded_key(&item.insert_text, &item.label)
        else {
            return;
        };
        if self.encoded_bytes.saturating_add(text.len()) > 4 * 1024 * 1024 {
            return;
        }
        let bytes = text.len();
        if self
            .encoded
            .insert((item.replacement_range, item.is_snippet, text, offset))
        {
            self.encoded_bytes += bytes;
        }
    }
    /// Reuses precisely the existing no-call syntax-rename proof before
    /// rebuilding/decoding the unchanged quoted carrier. A single tabstop and
    /// conservative byte bounds preserve every earlier parser/render guard.
    pub(super) fn reject_encoded(
        &mut self,
        snapshot: &AnalysisSnapshot,
        input: &crate::support::ParsedInput,
        projection: &ConsumptionProjection,
        item: &crate::CompletionItem,
        prefix: Option<&str>,
    ) -> bool {
        let Some(prefix) = prefix else {
            return false;
        };
        if item.label.contains(prefix) {
            return false;
        }
        let dollars = item.insert_text.bytes().filter(|b| *b == b'$').count();
        if dollars != usize::from(item.is_snippet) {
            return false;
        }
        if item.is_snippet
            && !item
                .insert_text
                .split_once('$')
                .is_some_and(|(_, tail)| tail.starts_with(|c: char| c.is_ascii_digit()))
        {
            return false;
        }
        // snippet_trial replaces at most one $number by prefix_number; quoting
        // and decoding cannot enlarge this bound, including escaped dollars.
        let trial_bytes = item
            .insert_text
            .len()
            .saturating_mul(2)
            .saturating_add(prefix.len() + 1);
        let budget = parser::ScriptParseBudget::default().bytes;
        let argument_bytes = projection
            .scalar
            .as_ref()
            .map_or(0, |s| s.range.len() as usize)
            .saturating_add(trial_bytes);
        if argument_bytes > 64 * 1024
            || input.source.len().saturating_add(trial_bytes) > budget
            || self.fixed_bytes.saturating_add(
                self.reads
                    .saturating_mul(argument_bytes.saturating_mul(4).saturating_add(32)),
            ) >= budget
        {
            return false;
        }
        let Some((text, offset)) = reject_groups::encoded_key(&item.insert_text, &item.label)
        else {
            return false;
        };
        let rejected =
            self.encoded
                .contains(&(item.replacement_range, item.is_snippet, text, offset))
                && !self.can_call(snapshot, &item.label);
        #[cfg(test)]
        if rejected {
            self.encoded_hits += 1;
        }
        rejected
    }

    fn can_call(&mut self, snapshot: &AnalysisSnapshot, key: &str) -> bool {
        can_call_cached(snapshot, &self.matchers, &mut self.can_call, key)
    }

    pub(super) fn validate(
        &mut self,
        snapshot: &AnalysisSnapshot,
        edit: &ArgumentEdit,
        label: &str,
        markers: &[String],
        cancellation: &CancellationToken,
        known_incomplete: bool,
    ) -> Result<Option<RenderValidation>, Cancelled> {
        cancellation.checkpoint()?;
        if edit.value.contains('$')
            || self.fixed_bytes.saturating_add(
                self.reads
                    .saturating_mul(edit.value.len().saturating_mul(4).saturating_add(32)),
            ) >= parser::ScriptParseBudget::default().bytes
        {
            return Ok(None);
        }
        let possible_call = self.can_call(snapshot, label);
        if possible_call && !known_incomplete {
            return Ok(None);
        }
        let normalized = (self.replacements.len() == 1)
            .then(|| reject_groups::normalize(&edit.value, label))
            .flatten();
        if known_incomplete
            && !possible_call
            && normalized
                .as_ref()
                .is_some_and(|key| self.groups.syntax_reuse(key, markers))
        {
            self.syntax_epoch = self.syntax_epoch.saturating_add(1);
            return Ok(Some(RenderValidation::Rejected(Default::default())));
        }
        if let Some(key) = &normalized
            && let Some(coverage) = self.groups.reuse(snapshot, key, label, markers)
        {
            return Ok(Some(RenderValidation::Rejected(coverage)));
        }
        let mut rendered = self.source.as_ref().clone();
        rendered.text.clear();
        for (i, piece) in rendered.pieces.iter_mut().enumerate() {
            let original = &self.source.pieces[i];
            let value = if self.replacements.contains(&i) {
                &edit.value
            } else {
                &self.source.text[original.range.start() as usize..original.range.end() as usize]
            };
            let start = rendered.text.len() as u32;
            rendered.text.push_str(value);
            piece.range =
                text::TextRange::new(start, rendered.text.len() as u32).expect("bounded rendering");
            if self.replacements.contains(&i) {
                piece.uncertain_text = false;
                let map = piece.binding_source.as_mut().expect("proven direct map");
                map.offsets = (0..=edit.value.len() as u32).collect::<Vec<_>>().into();
            }
        }
        if possible_call {
            if let Some(key) = &normalized
                && let Some(piece) = self
                    .replacements
                    .first()
                    .and_then(|i| rendered.pieces.get(*i))
                && let Some(range) = text::TextRange::new(
                    piece.range.start() + key.offset as u32,
                    piece.range.start() + (key.offset + label.len()) as u32,
                )
                && boundary::rejects(
                    snapshot,
                    &rendered,
                    range,
                    label,
                    &self.parameter,
                    markers,
                    cancellation,
                )?
            {
                return Ok(Some(RenderValidation::Rejected(Default::default())));
            }
            return Ok(None);
        }
        let schema = self.schema;
        let state = self.state.clone();
        let matchers = &self.matchers;
        let cache = &mut self.can_call;
        let body = self.session.instantiate(
            rendered,
            schema,
            state,
            markers,
            &mut |key| can_call_cached(snapshot, matchers, cache, key),
            &mut || cancellation.checkpoint(),
        )?;
        if let Some(body) = body {
            let body = Arc::new(body);
            if let Some(key) = normalized
                && let Some(piece) = self.replacements.first()
            {
                self.groups.remember(
                    snapshot,
                    key,
                    label,
                    markers,
                    &body,
                    &self.parameter,
                    *piece,
                );
            }
            Ok(Some(RenderValidation::Body(body)))
        } else {
            Ok(None)
        }
    }
}

fn can_call_cached(
    snapshot: &AnalysisSnapshot,
    matchers: &[MatcherId],
    cache: &mut BTreeMap<String, bool>,
    key: &str,
) -> bool {
    if let Some(value) = cache.get(key) {
        return *value;
    }
    let ir = snapshot.ir();
    let value = matchers.iter().any(|matcher| {
        // A missing selector in a context-free probe cannot rule out a
        // context-dependent union branch in the actual container.
        ir_semantic::has_field_query(ir, *matcher)
            || ir.scalar_outcome(*matcher, key, &WorkspaceFacts { snapshot }) != Some(false)
    });
    cache.insert(key.into(), value);
    value
}
