//! Syntax rejection proofs that survive expansion of literal Template bodies.
//! No semantic frontier is invented: callers use these only after the protocol
//! response is already known to be incomplete from a fully checked candidate.
use super::*;
use parser::{CstKind, CstNode};

type BoundaryKey = (String, String, BTreeMap<String, String>, BTreeSet<String>);
#[derive(Default)]
struct Boundaries(std::sync::Mutex<BoundaryEntries>);
#[derive(Default)]
struct BoundaryEntries {
    entries: BTreeMap<BoundaryKey, Option<Bound>>,
    bytes: usize,
}
#[derive(Clone, Copy)]
struct Bound {
    bytes: usize,
    nodes: usize,
    depth: usize,
}

fn cache(snapshot: &AnalysisSnapshot) -> Arc<Boundaries> {
    const KEY: &str = "template:literal-boundaries";
    if let Some(cache) = snapshot.query_cache().get(snapshot.revision(), KEY) {
        return cache;
    }
    let cache = Arc::new(Boundaries::default());
    snapshot.query_cache().insert(
        snapshot.revision(),
        engine::CacheDomain::Definitions,
        KEY.into(),
        cache.clone(),
    );
    snapshot
        .query_cache()
        .get(snapshot.revision(), KEY)
        .unwrap_or(cache)
}

fn kinds(snapshot: &AnalysisSnapshot) -> Arc<BTreeSet<String>> {
    if let Some(kinds) = snapshot.ir().prepared_template_kinds() {
        return kinds;
    }
    let ir = snapshot.ir();
    let Some(template) = ir.trait_by_name("Template") else {
        return Arc::new(BTreeSet::new());
    };
    Arc::new(
        ir.types
            .iter()
            .filter(|ty| {
                ty.trait_impls
                    .iter()
                    .any(|implementation| implementation.trait_id == template)
            })
            .map(|ty| ir.strings().resolve(ty.name).to_owned())
            .collect(),
    )
}

pub(crate) fn prewarm<E>(
    snapshot: &AnalysisSnapshot,
    kind: &str,
    name: &str,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    let cache = cache(snapshot);
    let mut active = BTreeSet::new();
    let mut nodes = 32768usize;
    safe(
        snapshot,
        &cache,
        &kinds(snapshot),
        kind,
        name,
        &BTreeMap::new(),
        &BTreeSet::new(),
        &mut active,
        &mut nodes,
        checkpoint,
    )
    .map(|value| value.is_some())
}

#[allow(clippy::too_many_arguments)]
fn safe<E>(
    snapshot: &AnalysisSnapshot,
    cache: &Boundaries,
    kinds: &BTreeSet<String>,
    kind: &str,
    name: &str,
    bindings: &BTreeMap<String, String>,
    present: &BTreeSet<String>,
    active: &mut BTreeSet<BoundaryKey>,
    nodes: &mut usize,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<Option<Bound>, E> {
    checkpoint()?;
    let key = (
        kind.to_ascii_lowercase(),
        name.to_ascii_lowercase(),
        bindings.clone(),
        present.clone(),
    );
    if let Some(value) = cache.0.lock().expect("boundary cache").entries.get(&key) {
        return Ok(*value);
    }
    if active.len() >= 64 || *nodes == 0 || !active.insert(key.clone()) {
        return Ok(None);
    }
    *nodes -= 1;
    let result = (|| {
        let facts = WorkspaceFacts { snapshot };
        let Some(ty) = snapshot.ir().type_by_name(kind) else {
            return Ok(None);
        };
        let Some(template) = rules::ir::SymbolFacts::template(&facts, ty, name) else {
            return Ok(Some(Bound {
                bytes: 0,
                nodes: 0,
                depth: 0,
            }));
        };
        if template.source.is_empty() || template.source.len() > 64 * 1024 {
            return Ok(None);
        }
        // Bare, delimiter-free arguments render identically under every scalar
        // or script classification; quoted/raw-carrier ambiguity falls back.
        if bindings.values().any(|value| !bare_argument(value)) {
            return Ok(None);
        }
        let rendered = hir::template_text::render(&template, bindings, present, 64 * 1024);
        if !rendered.coverage.is_known() || !rendered.frontiers.is_empty() {
            return Ok(None);
        }
        let parsed = match parser::parse_script_bounded(
            &rendered.text,
            parser::ScriptParseBudget {
                bytes: 64 * 1024,
                depth: 96,
                ..Default::default()
            },
            checkpoint,
        )? {
            Ok(parsed) => parsed,
            Err(_) => return Ok(None),
        };
        if !parsed
            .root()
            .children()
            .any(|n| n.kind() == CstKind::Property)
            || !parsed.errors().is_empty()
            || parsed.root().children().any(|node| {
                !matches!(
                    node.kind(),
                    CstKind::Property | CstKind::Comment | CstKind::Bom
                )
            })
        {
            return Ok(None);
        }
        if let Some(last) = parsed
            .root()
            .children()
            .next_back()
            .filter(|node| node.kind() == CstKind::Comment)
            && !rendered.text[last.range().end() as usize..].contains('\n')
        {
            return Ok(None);
        }
        let mut bound = Bound {
            bytes: rendered.text.len().max(1),
            nodes: parsed.tree().node_count(),
            depth: 0,
        };
        let mut nested_depth = 0;
        let mut pending = vec![(parsed.root(), 0usize)];
        while let Some((node, depth)) = pending.pop() {
            checkpoint()?;
            if *nodes == 0 {
                return Ok(None);
            }
            *nodes -= 1;
            if node.kind() == CstKind::Property {
                let Some(key) = node
                    .children()
                    .find(|n| n.kind() == CstKind::Key)
                    .and_then(|n| parsed.text(n.range()))
                else {
                    return Ok(None);
                };
                if key.starts_with('"') {
                    return Ok(None);
                }
                let key = key.to_owned();
                let Some((bindings, present)) =
                    call_arguments(snapshot, kinds, &parsed, node, &key)
                else {
                    return Ok(None);
                };
                for kind in kinds {
                    let Some(child) = safe(
                        snapshot, cache, kinds, kind, &key, &bindings, &present, active, nodes,
                        checkpoint,
                    )?
                    else {
                        return Ok(None);
                    };
                    bound.bytes = bound.bytes.saturating_add(child.bytes);
                    bound.nodes = bound
                        .nodes
                        .saturating_add(child.nodes)
                        .saturating_add(8 * usize::from(child.bytes > 0));
                    nested_depth = nested_depth.max(child.depth);
                    if bound.bytes > 64 * 1024
                        || bound.nodes >= parser::ScriptParseBudget::default().nodes
                    {
                        return Ok(None);
                    }
                }
            }
            let depth = depth
                + usize::from(matches!(
                    node.kind(),
                    CstKind::Block | CstKind::ParameterBlock
                ));
            bound.depth = bound.depth.max(depth);
            pending.extend(node.children().map(|node| (node, depth)));
        }
        bound.depth = bound.depth.saturating_add(nested_depth);
        if bound.depth > 96 {
            return Ok(None);
        }
        Ok(Some(bound))
    })();
    active.remove(&key);
    if let Ok(value) = result {
        let mut cache = cache.0.lock().expect("boundary cache");
        let bytes = 256usize
            .saturating_add(key.0.len())
            .saturating_add(key.1.len())
            .saturating_add(
                key.2
                    .iter()
                    .map(|(name, value)| 128 + name.len() + value.len())
                    .sum::<usize>(),
            )
            .saturating_add(key.3.iter().map(|name| 64 + name.len()).sum::<usize>());
        if cache.entries.len() < 16384
            && cache.bytes.saturating_add(bytes) <= 8 * 1024 * 1024
            && !cache.entries.contains_key(&key)
        {
            cache.bytes += bytes;
            cache.entries.insert(key, value);
        }
    }
    result
}

fn bare_argument(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b':' | b'.' | b'-'))
}
fn call_arguments(
    snapshot: &AnalysisSnapshot,
    kinds: &BTreeSet<String>,
    parsed: &parser::ParsedFile,
    property: CstNode<'_>,
    key: &str,
) -> Option<(BTreeMap<String, String>, BTreeSet<String>)> {
    let facts = WorkspaceFacts { snapshot };
    let callable = kinds.iter().any(|kind| {
        snapshot
            .ir()
            .type_by_name(kind)
            .and_then(|ty| rules::ir::SymbolFacts::template(&facts, ty, key))
            .is_some()
    });
    if callable {
        arguments(parsed, property)
    } else {
        Some((BTreeMap::new(), BTreeSet::new()))
    }
}

fn arguments(
    parsed: &parser::ParsedFile,
    property: CstNode<'_>,
) -> Option<(BTreeMap<String, String>, BTreeSet<String>)> {
    let mut bindings = BTreeMap::new();
    let mut present = BTreeSet::new();
    for block in property
        .children()
        .filter(|n| n.kind() == CstKind::Value)
        .flat_map(|n| n.children())
        .filter(|n| n.kind() == CstKind::Block)
    {
        for argument in block.children().filter(|n| n.kind() == CstKind::Property) {
            let name = argument
                .children()
                .find(|n| n.kind() == CstKind::Key)
                .and_then(|n| parsed.text(n.range()))?;
            if !bare_argument(name) {
                return None;
            }
            let name = name.to_ascii_lowercase();
            present.insert(name.clone());
            let value = argument
                .children()
                .find(|n| n.kind() == CstKind::Value)?
                .children()
                .next()?;
            if value.kind() == CstKind::Block {
                continue;
            }
            if value.kind() != CstKind::BareValue {
                return None;
            }
            let value = parsed.text(value.range())?;
            if !bare_argument(value) {
                return None;
            }
            bindings.insert(name, value.to_owned());
        }
    }
    Some((bindings, present))
}

fn complete_value(parsed: &parser::ParsedFile, property: CstNode<'_>) -> bool {
    let Some(value) = property.children().find(|n| n.kind() == CstKind::Value) else {
        return false;
    };
    let Some(last) = value.children().next_back() else {
        return false;
    };
    match last.kind() {
        CstKind::BareValue => true,
        CstKind::QuotedString => parsed
            .text(last.range())
            .is_some_and(|text| text.len() >= 2 && text.ends_with('"')),
        CstKind::Block => parsed
            .text(last.range())
            .is_some_and(|text| text.ends_with('}')),
        _ => false,
    }
}

pub(super) fn rejects(
    snapshot: &AnalysisSnapshot,
    rendered: &hir::template_text::RenderedTemplate,
    key_range: text::TextRange,
    label: &str,
    parameter: &str,
    markers: &[String],
    cancellation: &CancellationToken,
) -> Result<bool, Cancelled> {
    let progress =
        parser::parse_script_prefix_bounded(&rendered.text, Default::default(), &mut || {
            cancellation.checkpoint()
        })?;
    if progress.frontier.is_some() {
        return Ok(false);
    }
    let Some(parsed) = progress.parsed else {
        return Ok(false);
    };
    let mut pending = vec![parsed.root()];
    let mut site = None;
    while let Some(parent) = pending.pop() {
        let children = parent.children().collect::<Vec<_>>();
        for (i, property) in children
            .iter()
            .copied()
            .enumerate()
            .filter(|(_, n)| n.kind() == CstKind::Property)
        {
            if property.children().any(|n| {
                n.kind() == CstKind::Key
                    && n.range() == key_range
                    && parsed.text(n.range()) == Some(label)
            }) {
                if children[..i]
                    .iter()
                    .rev()
                    .find(|n| !matches!(n.kind(), CstKind::Comment | CstKind::Bom))
                    .is_some_and(|previous| {
                        previous.kind() != CstKind::Property || !complete_value(&parsed, *previous)
                    })
                {
                    return Ok(false);
                }
                site = Some(property);
                break;
            }
        }
        if site.is_some() {
            break;
        }
        pending.extend(children);
    }
    let Some(property) = site.filter(|p| complete_value(&parsed, *p)) else {
        return Ok(false);
    };
    let witness = parsed.errors().iter().any(|error| {
        error.kind == parser::SyntaxErrorKind::UnexpectedToken
            && parsed.text(error.range) == Some("=")
            && error.range.start() >= property.range().end()
            && parsed.source()[property.range().end() as usize..error.range.start() as usize]
                .chars()
                .all(char::is_whitespace)
            && rendered.dependencies(error.range).contains(parameter)
            && !rendered.has_hole(error.range)
            && !rendered.has_uncertain_text(error.range)
            && !rendered.has_unresolved_structure(error.range)
            && !markers.iter().any(|marker| {
                rendered.text.match_indices(marker).any(|(start, _)| {
                    start < error.range.end() as usize
                        && (error.range.start() as usize) < start + marker.len()
                })
            })
    });
    if !witness {
        return Ok(false);
    }
    let cache = cache(snapshot);
    let kinds = kinds(snapshot);
    let mut expanded_bytes = rendered.text.len();
    let mut expanded_nodes = parsed.tree().node_count();
    let mut expansion_depth = 0;
    // A changed sibling selector can activate an earlier invocation too.
    // Prove every potential call in this rendered container, including
    // calls in argument blocks when their parent is not selected as a call.
    let mut properties = vec![parsed.root()];
    while let Some(node) = properties.pop() {
        cancellation.checkpoint()?;
        properties.extend(node.children());
        if node.kind() != CstKind::Property {
            continue;
        }
        let Some(key) = node
            .children()
            .find(|n| n.kind() == CstKind::Key)
            .and_then(|n| parsed.text(n.range()))
        else {
            return Ok(false);
        };
        if key.starts_with('"') {
            return Ok(false);
        }
        let Some((bindings, present)) = call_arguments(snapshot, &kinds, &parsed, node, key) else {
            return Ok(false);
        };
        for kind in kinds.iter() {
            let mut active = BTreeSet::new();
            let mut nodes = 32768;
            let Some(bound) = safe(
                snapshot,
                &cache,
                &kinds,
                kind,
                key,
                &bindings,
                &present,
                &mut active,
                &mut nodes,
                &mut || cancellation.checkpoint(),
            )?
            else {
                return Ok(false);
            };
            expanded_bytes = expanded_bytes.saturating_add(bound.bytes);
            expanded_nodes = expanded_nodes
                .saturating_add(bound.nodes)
                .saturating_add(8 * usize::from(bound.bytes > 0));
            expansion_depth = expansion_depth.max(bound.depth);
        }
    }
    // Closed property sequences retain their internal CST when spliced into
    // a container. Count every possible callee occurrence and eight extra
    // boundary/recovery nodes per splice, including memoized DAG repetitions.
    // Both text and node work remain below the ordinary parser/renderer caps.
    let mut pending = vec![(parsed.root(), 0usize)];
    let mut source_depth = 0;
    while let Some((node, depth)) = pending.pop() {
        let depth = depth
            + usize::from(matches!(
                node.kind(),
                CstKind::Block | CstKind::ParameterBlock
            ));
        source_depth = source_depth.max(depth);
        pending.extend(node.children().map(|node| (node, depth)));
    }
    Ok(expanded_bytes < parser::ScriptParseBudget::default().bytes
        && expanded_nodes < parser::ScriptParseBudget::default().nodes
        && source_depth.saturating_add(expansion_depth)
            < parser::ScriptParseBudget::default().depth)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rules::ir::SymbolFacts;

    fn rendered(source: &str) -> hir::template_text::RenderedTemplate {
        let range = text::TextRange::new(0, source.len() as u32).unwrap();
        hir::template_text::RenderedTemplate {
            text: source.into(),
            pieces: vec![hir::template_text::SourcePiece {
                origin: ("scripted_effect".into(), "embedded".into()),
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
    fn literal_boundary_rejection_matches_expansion_and_refuses_uncertain_boundaries() {
        let mut host = engine::AnalysisHost::with_ir(
            game::eu4::runtime_rules().unwrap(),
            game::eu4::profile(),
            game::eu4::first_party_ir().unwrap(),
        );
        let definitions = "embedded = { $BODY$ } literal = { add_prestige = 1 } literal_quoted = { set_variable = { which = \"counter\" value = 1 } } chain = { literal = yes } with_argument = { add_prestige = $AMOUNT$ } empty = { } dangling = { leftover } parameterized = { $OTHER$ } guarded = { [[OTHER] add_prestige = 1 ] } cycle = { cycle = yes } comment = { add_prestige = 1 # trailing\n }";
        let id =
            engine::DocumentId::new("file:///boundary/common/scripted_effects/definitions.txt");
        host.open_document(id.clone(), 1, definitions.into(), None)
            .unwrap();
        let snapshot = host.snapshot();
        let facts = WorkspaceFacts {
            snapshot: &snapshot,
        };
        let ty = snapshot.ir().type_by_name("scripted_effect").unwrap();
        let template = facts.template(ty, "embedded").unwrap();
        let schema = hir::template::template_body(snapshot.ir(), ty).unwrap();
        for name in ["literal", "literal_quoted", "chain", "comment"] {
            assert!(
                prewarm(&snapshot, "scripted_effect", name, &mut || Ok::<(), ()>(())).unwrap(),
                "{name}"
            );
            let source = format!("{name} = yes = {{ size = 1 }}");
            let range = text::TextRange::new(0, name.len() as u32).unwrap();
            assert!(
                rejects(
                    &snapshot,
                    &rendered(&source),
                    range,
                    name,
                    "body",
                    &[],
                    &CancellationToken::new()
                )
                .unwrap(),
                "{name}"
            );
            let bindings = BTreeMap::from([("body".into(), source.clone())]);
            let full = hir::template_instance::instantiate(
                snapshot.ir(),
                &facts,
                template.clone(),
                &bindings,
                &bindings,
                &BTreeSet::from(["body".into()]),
                schema,
                ScopeState::initial(hir::ScopeValue::known_single("country")),
                &[],
                hir::template_instance::InstanceGoal::Validation,
                &mut || Ok::<(), ()>(()),
            )
            .unwrap();
            assert!(full.coverage.is_complete(), "{name}");
            assert!(
                full.evidence
                    .iter()
                    .any(|e| e.kind == hir::checking::IssueKind::Syntax
                        && evidence_dependencies(&full, e).contains("body")),
                "{name}"
            );
        }
        let with_args = "with_argument = { AMOUNT = __trial } = { size = 1 }";
        assert!(
            rejects(
                &snapshot,
                &rendered(with_args),
                text::TextRange::new(0, 13).unwrap(),
                "with_argument",
                "body",
                &[],
                &CancellationToken::new()
            )
            .unwrap()
        );
        for args in [
            "AMOUNT = \"quoted\"",
            "AMOUNT = { broken }",
            "OTHER = unused",
        ] {
            let source = format!("with_argument = {{ {args} }} = {{}}");
            assert!(
                !rejects(
                    &snapshot,
                    &rendered(&source),
                    text::TextRange::new(0, 13).unwrap(),
                    "with_argument",
                    "body",
                    &[],
                    &CancellationToken::new()
                )
                .unwrap(),
                "{source}"
            );
        }
        for name in ["empty", "dangling", "parameterized", "guarded", "cycle"] {
            assert!(
                !prewarm(&snapshot, "scripted_effect", name, &mut || Ok::<(), ()>(())).unwrap(),
                "{name}"
            );
            let source = format!("{name} = yes = {{ size = 1 }}");
            assert!(
                !rejects(
                    &snapshot,
                    &rendered(&source),
                    text::TextRange::new(0, name.len() as u32).unwrap(),
                    name,
                    "body",
                    &[],
                    &CancellationToken::new()
                )
                .unwrap(),
                "{name}"
            );
        }
        for source in [
            "literal = { dangling = yes } = {}",
            "dangling = yes literal = yes = {}",
            "orphan literal = yes = {}",
            "literal = yes",
            "literal = yes # = {}",
        ] {
            let start = source.find("literal").unwrap() as u32;
            assert!(
                !rejects(
                    &snapshot,
                    &rendered(source),
                    text::TextRange::new(start, start + 7).unwrap(),
                    "literal",
                    "body",
                    &[],
                    &CancellationToken::new()
                )
                .unwrap(),
                "{source}"
            );
        }
        let source = rendered("literal = yes = {}");
        assert!(
            !rejects(
                &snapshot,
                &source,
                text::TextRange::new(0, 7).unwrap(),
                "literal",
                "other",
                &[],
                &CancellationToken::new()
            )
            .unwrap()
        );
        let mut source = source;
        source.pieces[0].uncertain_text = true;
        assert!(
            !rejects(
                &snapshot,
                &source,
                text::TextRange::new(0, 7).unwrap(),
                "literal",
                "body",
                &[],
                &CancellationToken::new()
            )
            .unwrap()
        );
        // Large literal output fits the real node budget even when a byte-to-
        // node multiplier would be too pessimistic. Repeated DAG expansions
        // must still be charged per occurrence rather than per cached node.
        let mut large = definitions.to_owned()
            + " many = { "
            + &"add_prestige = 1 ".repeat(800)
            + " } dag0 = { x=1 }";
        for n in 1..18 {
            large.push_str(&format!(
                " dag{n} = {{ dag{} = yes dag{} = yes }}",
                n - 1,
                n - 1
            ));
        }
        host.apply_document_changes(&id, 2, &[engine::TextChange::full(large)])
            .unwrap();
        let expanded = host.snapshot();
        let source = "many = yes = { size = 1 }";
        assert!(
            rejects(
                &expanded,
                &rendered(source),
                text::TextRange::new(0, 4).unwrap(),
                "many",
                "body",
                &[],
                &CancellationToken::new()
            )
            .unwrap()
        );
        let full_facts = WorkspaceFacts {
            snapshot: &expanded,
        };
        let bindings = BTreeMap::from([("body".into(), source.into())]);
        let full = hir::template_instance::instantiate(
            expanded.ir(),
            &full_facts,
            full_facts.template(ty, "embedded").unwrap(),
            &bindings,
            &bindings,
            &BTreeSet::from(["body".into()]),
            schema,
            ScopeState::initial(hir::ScopeValue::known_single("country")),
            &[],
            hir::template_instance::InstanceGoal::Validation,
            &mut || Ok::<(), ()>(()),
        )
        .unwrap();
        assert!(full.coverage.is_complete());
        assert!(
            full.evidence
                .iter()
                .any(|e| e.kind == hir::checking::IssueKind::Syntax
                    && evidence_dependencies(&full, e).contains("body"))
        );
        let source = "dag17 = yes = { size = 1 }";
        assert!(
            !rejects(
                &expanded,
                &rendered(source),
                text::TextRange::new(0, 5).unwrap(),
                "dag17",
                "body",
                &[],
                &CancellationToken::new()
            )
            .unwrap()
        );
        // Changes to a definition invalidate both positive and negative summaries.
        host.apply_document_changes(
            &id,
            3,
            &[engine::TextChange::full(definitions.replace(
                "literal = { add_prestige = 1 }",
                "literal = { $OTHER$ }",
            ))],
        )
        .unwrap();
        assert!(
            !prewarm(
                &host.snapshot(),
                "scripted_effect",
                "literal",
                &mut || Ok::<(), ()>(())
            )
            .unwrap()
        );
    }
}
