//! Definition-site cycle detection for scripted macros.
//!
//! The expansion-time guard reports a cycle at the call site where recursion
//! first repeats, which can sit far from every participating definition. This
//! module answers the definition-side question instead: which scripted-macro
//! *definitions* can never finish expanding because they participate in a call
//! cycle. The call graph is built from the lowered macro templates (literal
//! keys are static calls) plus invocation argument bindings (a `$param$` used
//! in key position is a dynamic call whose target comes from each call site),
//! then reduced to strongly connected components. Every definition inside a
//! non-trivial component is reported at its own definition site, so a cycle is
//! rejected when defined, like a recursive `fn` in Rust.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use pdx_engine::hir::{
    MacroTemplate, MacroTemplateFragment, MacroTemplateItem, MacroTemplateToken, MacroTemplateValue,
};
use pdx_engine::{AnalysisSnapshot, DocumentId, DocumentSource, SourceFileId};
use pdx_parser::FileFormat;
use pdx_text::TextRange;

use crate::macro_expansion::scalar_argument_bindings;
use crate::semantic::{probe_query_cache, resolve_macro_definition, scripted_macro_type};
use crate::support::{
    ParsedContent, ParsedInput, ScriptProperty, input_for_document, input_for_source_file,
    script_properties,
};
use crate::types::{CancellationToken, Cancelled, Diagnostic, DiagnosticCode};

/// Cache key for the workspace-wide cycle report inside the snapshot query cache.
const CYCLE_CACHE_KEY: &str = "macro-cycle-graph";

/// Workspace-wide result: every cyclic macro definition keyed by
/// `(kind lowercased, name lowercased)` with its definition-site message.
#[derive(Clone, Debug, Default)]
pub(crate) struct MacroCycleReport {
    entries: BTreeMap<(String, String), String>,
}

impl MacroCycleReport {
    pub(crate) fn message(&self, kind: &str, name: &str) -> Option<&str> {
        self.entries
            .get(&(kind.to_ascii_lowercase(), name.to_ascii_lowercase()))
            .map(String::as_str)
    }
}

/// Returns definition-site cycle diagnostics for the macros defined in `input`.
pub(crate) fn macro_cycle_diagnostics(
    snapshot: &AnalysisSnapshot,
    input: &ParsedInput,
    cancellation: &CancellationToken,
) -> Result<Vec<Diagnostic>, Cancelled> {
    cancellation.checkpoint()?;
    if input.format != FileFormat::Script {
        return Ok(Vec::new());
    }
    let Some(hir) = input.hir.as_deref() else {
        return Ok(Vec::new());
    };
    let report = macro_cycle_report(snapshot, cancellation)?;
    if report.entries.is_empty() {
        return Ok(Vec::new());
    }
    let mut diagnostics = Vec::new();
    for definition in hir.definitions() {
        if !scripted_macro_type(snapshot, &definition.kind) {
            continue;
        }
        let Some(message) = report.message(&definition.kind, definition.name.as_str()) else {
            continue;
        };
        diagnostics.push(Diagnostic::new(
            DiagnosticCode::MacroExpansionCycle,
            DiagnosticCode::MacroExpansionCycle.severity(),
            definition.selection_range,
            message.to_owned(),
        ));
    }
    Ok(diagnostics)
}

/// True when `(kind, name)` already has a definition-site cycle diagnostic, so
/// the expansion-time guard can stay silent for the same root cause.
pub(crate) fn macro_cycle_reported(snapshot: &AnalysisSnapshot, kind: &str, name: &str) -> bool {
    let revision = snapshot.revision();
    probe_query_cache::<MacroCycleReport>(snapshot, revision, &[CYCLE_CACHE_KEY])
        .is_some_and(|report| report.message(kind, name).is_some())
}

pub(crate) fn macro_cycle_report(
    snapshot: &AnalysisSnapshot,
    cancellation: &CancellationToken,
) -> Result<Arc<MacroCycleReport>, Cancelled> {
    let revision = snapshot.revision();
    if let Some(cached) =
        probe_query_cache::<MacroCycleReport>(snapshot, revision, &[CYCLE_CACHE_KEY])
    {
        return Ok(cached);
    }
    let report = build_cycle_report(snapshot, cancellation)?;
    let report = Arc::new(report);
    snapshot.query_cache().insert(
        revision,
        pdx_engine::CacheDomain::Documents,
        CYCLE_CACHE_KEY.to_owned(),
        report.clone(),
    );
    Ok(report)
}

/// One live scripted-macro definition in the call graph.
struct MacroNode {
    kind: String,
    name: String,
    /// Lowercased parameter names used in key position inside the template.
    dynamic_key_params: Vec<String>,
}

fn build_cycle_report(
    snapshot: &AnalysisSnapshot,
    cancellation: &CancellationToken,
) -> Result<MacroCycleReport, Cancelled> {
    cancellation.checkpoint()?;
    // Node names come from both live surfaces: the workspace index (file-backed
    // definitions) and open overlay documents, matching resolve order.
    let mut candidates: Vec<(String, String)> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for definition in snapshot.index().definitions_iter() {
        if !scripted_macro_type(snapshot, &definition.kind) {
            continue;
        }
        if seen.insert((
            definition.kind.to_ascii_lowercase(),
            definition.name.to_ascii_lowercase(),
        )) {
            candidates.push((definition.kind.to_string(), definition.name.to_string()));
        }
    }
    for document in snapshot
        .documents()
        .values()
        .filter(|document| document.source() == DocumentSource::Overlay)
    {
        let Some(hir) = document.hir_handle() else {
            continue;
        };
        for definition in hir.definitions() {
            if !scripted_macro_type(snapshot, &definition.kind) {
                continue;
            }
            if seen.insert((
                definition.kind.to_ascii_lowercase(),
                definition.name.to_ascii_lowercase(),
            )) {
                candidates.push((definition.kind.clone(), definition.name.clone()));
            }
        }
    }
    if candidates.is_empty() {
        return Ok(MacroCycleReport::default());
    }

    // Resolve every candidate to its live definition. Ambiguous or unresolved
    // names are skipped: the expansion-time guard still covers them.
    let mut nodes: Vec<MacroNode> = Vec::new();
    let mut templates: Vec<MacroTemplate> = Vec::new();
    for (kind, name) in &candidates {
        cancellation.checkpoint()?;
        let Some(resolved) = resolve_macro_definition(snapshot, kind, name) else {
            continue;
        };
        let Some(template) = resolved.summary.template else {
            continue;
        };
        let mut dynamic_key_params = Vec::new();
        collect_dynamic_key_params(&template.items, &mut dynamic_key_params);
        nodes.push(MacroNode {
            kind: kind.clone(),
            name: resolved.summary.name.clone(),
            dynamic_key_params,
        });
        templates.push(template);
    }
    let index_of = |kind: &str, name: &str| -> Option<usize> {
        let kind = kind.to_ascii_lowercase();
        let name = name.to_ascii_lowercase();
        nodes.iter().position(|node| {
            node.kind.eq_ignore_ascii_case(&kind) && node.name.eq_ignore_ascii_case(&name)
        })
    };

    // Static edges: a property key that is one literal naming a same-kind macro.
    let mut edges: Vec<Vec<usize>> = vec![Vec::new(); nodes.len()];
    let mut needs_bindings = false;
    for (source, node) in nodes.iter().enumerate() {
        needs_bindings |= !node.dynamic_key_params.is_empty();
        let mut callees = Vec::new();
        collect_literal_callees(&templates[source].items, &mut callees);
        for callee in callees {
            if let Some(target) = index_of(&node.kind, &callee)
                && !edges[source].contains(&target)
            {
                edges[source].push(target);
            }
        }
    }

    // Dynamic edges: for every `$param$` used as a key, each call-site binding
    // that renders the key to a same-kind macro name adds that edge. Only
    // computed when at least one template actually uses a parameter in key
    // position, so plain-parameter macros never trigger a workspace scan.
    if needs_bindings {
        let mut bindings = HashMap::<(String, String), Vec<BTreeMap<String, String>>>::new();
        collect_call_site_bindings(snapshot, &nodes, &mut bindings, cancellation)?;
        for (source, node) in nodes.iter().enumerate() {
            if node.dynamic_key_params.is_empty() {
                continue;
            }
            let key = (
                node.kind.to_ascii_lowercase(),
                node.name.to_ascii_lowercase(),
            );
            let Some(sites) = bindings.get(&key) else {
                continue;
            };
            let mut rendered = Vec::new();
            for site_bindings in sites {
                collect_rendered_param_keys(&templates[source].items, site_bindings, &mut rendered);
            }
            for callee in rendered {
                if let Some(target) = index_of(&node.kind, &callee)
                    && !edges[source].contains(&target)
                {
                    edges[source].push(target);
                }
            }
        }
    }

    let report = report_from_graph(&nodes, &edges);
    Ok(report)
}

fn collect_call_site_bindings(
    snapshot: &AnalysisSnapshot,
    nodes: &[MacroNode],
    bindings: &mut HashMap<(String, String), Vec<BTreeMap<String, String>>>,
    cancellation: &CancellationToken,
) -> Result<(), Cancelled> {
    // Only macros that use a parameter in key position need their call sites.
    let wanted: std::collections::HashSet<(String, String)> = nodes
        .iter()
        .filter(|node| !node.dynamic_key_params.is_empty())
        .map(|node| {
            (
                node.kind.to_ascii_lowercase(),
                node.name.to_ascii_lowercase(),
            )
        })
        .collect();
    if wanted.is_empty() {
        return Ok(());
    }
    // Referencing sites are enumerated from index references and overlay HIR;
    // each file is parsed at most once and searched for invocation properties.
    let mut file_sites: Vec<(SourceFileId, String, String, TextRange)> = Vec::new();
    for reference in snapshot.index().references_iter() {
        if !scripted_macro_type(snapshot, &reference.kind) {
            continue;
        }
        let key = (
            reference.kind.to_ascii_lowercase(),
            reference.name.to_ascii_lowercase(),
        );
        if wanted.contains(&key) {
            file_sites.push((
                reference.file_id,
                reference.kind.to_string(),
                reference.name.to_string(),
                reference.range,
            ));
        }
    }
    let mut overlay_sites: Vec<(DocumentId, String, String, TextRange)> = Vec::new();
    for document in snapshot
        .documents()
        .values()
        .filter(|document| document.source() == DocumentSource::Overlay)
    {
        let Some(hir) = document.hir_handle() else {
            continue;
        };
        for reference in hir.references() {
            if !scripted_macro_type(snapshot, &reference.kind) {
                continue;
            }
            let key = (
                reference.kind.to_ascii_lowercase(),
                reference.name.to_ascii_lowercase(),
            );
            if wanted.contains(&key) {
                overlay_sites.push((
                    document.id().clone(),
                    reference.kind.clone(),
                    reference.name.clone(),
                    reference.range,
                ));
            }
        }
    }
    // Group by carrier so each file/document is parsed once.
    let mut by_file: HashMap<SourceFileId, Vec<usize>> = HashMap::new();
    for (index, (file, _, _, _)) in file_sites.iter().enumerate() {
        by_file.entry(*file).or_default().push(index);
    }
    for (file, indices) in by_file {
        cancellation.checkpoint()?;
        let Some(input) = input_for_source_file(snapshot, file) else {
            continue;
        };
        let properties = root_properties(&input);
        for index in indices {
            let (_, kind, name, range) = &file_sites[index];
            if let Some(invocation) = find_property_by_key_range(&properties, *range) {
                bindings
                    .entry((kind.to_ascii_lowercase(), name.to_ascii_lowercase()))
                    .or_default()
                    .push(scalar_argument_bindings(invocation));
            }
        }
    }
    let mut by_document: HashMap<DocumentId, Vec<usize>> = HashMap::new();
    for (index, (document, _, _, _)) in overlay_sites.iter().enumerate() {
        by_document.entry(document.clone()).or_default().push(index);
    }
    for (document, indices) in by_document {
        cancellation.checkpoint()?;
        let Some(input) = input_for_document(snapshot, &document) else {
            continue;
        };
        let properties = root_properties(&input);
        for index in indices {
            let (_, kind, name, range) = &overlay_sites[index];
            if let Some(invocation) = find_property_by_key_range(&properties, *range) {
                bindings
                    .entry((kind.to_ascii_lowercase(), name.to_ascii_lowercase()))
                    .or_default()
                    .push(scalar_argument_bindings(invocation));
            }
        }
    }
    Ok(())
}

fn root_properties(input: &ParsedInput) -> Vec<ScriptProperty> {
    let ParsedContent::Text(parsed) = &input.parsed;
    script_properties(input, parsed.root())
}

fn find_property_by_key_range(
    properties: &[ScriptProperty],
    key_range: TextRange,
) -> Option<&ScriptProperty> {
    properties
        .iter()
        .find(|property| property.key_range == key_range)
        .or_else(|| {
            properties
                .iter()
                .filter_map(|property| find_property_by_key_range(&property.block, key_range))
                .next()
        })
}

fn collect_dynamic_key_params(items: &[MacroTemplateItem], params: &mut Vec<String>) {
    for item in items {
        match item {
            MacroTemplateItem::Property(property) => {
                if token_has_parameter(&property.key)
                    && let Some(name) = first_parameter_name(&property.key)
                    && !params.iter().any(|param| param.eq_ignore_ascii_case(name))
                {
                    params.push(name.to_ascii_lowercase());
                }
                if let MacroTemplateValue::Block { items, .. } = &property.value {
                    collect_dynamic_key_params(items, params);
                }
            }
            MacroTemplateItem::Conditional(conditional) => {
                collect_dynamic_key_params(&conditional.items, params);
            }
            MacroTemplateItem::BareValue(_) => {}
        }
    }
}

fn collect_literal_callees(items: &[MacroTemplateItem], callees: &mut Vec<String>) {
    for item in items {
        match item {
            MacroTemplateItem::Property(property) => {
                if let [MacroTemplateFragment::Literal(key)] = property.key.fragments.as_slice() {
                    let key = key.trim();
                    if !key.is_empty()
                        && !callees
                            .iter()
                            .any(|callee| callee.eq_ignore_ascii_case(key))
                    {
                        callees.push(key.to_owned());
                    }
                }
                if let MacroTemplateValue::Block { items, .. } = &property.value {
                    collect_literal_callees(items, callees);
                }
            }
            MacroTemplateItem::Conditional(conditional) => {
                collect_literal_callees(&conditional.items, callees);
            }
            MacroTemplateItem::BareValue(_) => {}
        }
    }
}

/// Keys that contain a parameter fragment, rendered with one call-site binding
/// set; parameters without a binding leave the key unrenderable and skipped.
fn collect_rendered_param_keys(
    items: &[MacroTemplateItem],
    bindings: &BTreeMap<String, String>,
    rendered: &mut Vec<String>,
) {
    for item in items {
        match item {
            MacroTemplateItem::Property(property) => {
                if token_has_parameter(&property.key)
                    && let Some(key) = render_token(&property.key, bindings)
                    && !rendered
                        .iter()
                        .any(|candidate| candidate.eq_ignore_ascii_case(&key))
                {
                    rendered.push(key);
                }
                if let MacroTemplateValue::Block { items, .. } = &property.value {
                    collect_rendered_param_keys(items, bindings, rendered);
                }
            }
            MacroTemplateItem::Conditional(conditional) => {
                collect_rendered_param_keys(&conditional.items, bindings, rendered);
            }
            MacroTemplateItem::BareValue(_) => {}
        }
    }
}

fn token_has_parameter(token: &MacroTemplateToken) -> bool {
    token
        .fragments
        .iter()
        .any(|fragment| matches!(fragment, MacroTemplateFragment::Parameter { .. }))
}

fn first_parameter_name(token: &MacroTemplateToken) -> Option<&str> {
    token.fragments.iter().find_map(|fragment| match fragment {
        MacroTemplateFragment::Parameter { name, .. } => Some(name.as_str()),
        _ => None,
    })
}

fn render_token(token: &MacroTemplateToken, bindings: &BTreeMap<String, String>) -> Option<String> {
    let mut rendered = String::new();
    for fragment in &token.fragments {
        match fragment {
            MacroTemplateFragment::Literal(literal) => rendered.push_str(literal),
            MacroTemplateFragment::Parameter { name, .. } => {
                let value = bindings.get(&name.to_ascii_lowercase())?;
                rendered.push_str(unquote(value).trim());
            }
        }
    }
    let rendered = rendered.trim().to_owned();
    if rendered.is_empty() {
        None
    } else {
        Some(rendered)
    }
}

fn unquote(value: &str) -> &str {
    value
        .strip_prefix('"')
        .and_then(|inner| inner.strip_suffix('"'))
        .unwrap_or(value)
}

fn report_from_graph(nodes: &[MacroNode], edges: &[Vec<usize>]) -> MacroCycleReport {
    let components = tarjan_scc(edges);
    let mut entries = BTreeMap::new();
    for component in components {
        let cyclic = component.len() > 1 || {
            let node = component[0];
            edges[node].contains(&node)
        };
        if !cyclic {
            continue;
        }
        let chain = cycle_chain(&component, edges);
        for &index in &component {
            // Rotate the cycle walk so each definition's message starts at
            // that definition: `ping -> pong -> ping` on `ping`, and the
            // rotated walk on `pong`.
            let start = chain
                .iter()
                .position(|&member| member == index)
                .unwrap_or(0);
            let rotated = chain
                .iter()
                .cycle()
                .skip(start)
                .take(chain.len())
                .map(|&member| nodes[member].name.as_str())
                .chain(std::iter::once(nodes[index].name.as_str()))
                .collect::<Vec<_>>()
                .join(" -> ");
            entries.insert(
                (
                    nodes[index].kind.to_ascii_lowercase(),
                    nodes[index].name.to_ascii_lowercase(),
                ),
                format!(
                    "scripted macro `{}` is part of a definition cycle: {rotated}",
                    nodes[index].name
                ),
            );
        }
    }
    MacroCycleReport { entries }
}

/// Iterative Tarjan strongly-connected components.
fn tarjan_scc(edges: &[Vec<usize>]) -> Vec<Vec<usize>> {
    let count = edges.len();
    let mut index = vec![usize::MAX; count];
    let mut low = vec![0usize; count];
    let mut on_stack = vec![false; count];
    let mut stack: Vec<usize> = Vec::new();
    let mut components = Vec::new();
    let mut next_index = 0usize;
    for root in 0..count {
        if index[root] != usize::MAX {
            continue;
        }
        index[root] = next_index;
        low[root] = next_index;
        next_index += 1;
        stack.push(root);
        on_stack[root] = true;
        let mut call_stack: Vec<(usize, usize)> = vec![(root, 0)];
        while let Some(&mut (node, ref mut cursor)) = call_stack.last_mut() {
            if *cursor < edges[node].len() {
                let child = edges[node][*cursor];
                *cursor += 1;
                if index[child] == usize::MAX {
                    index[child] = next_index;
                    low[child] = next_index;
                    next_index += 1;
                    stack.push(child);
                    on_stack[child] = true;
                    call_stack.push((child, 0));
                } else if on_stack[child] {
                    low[node] = low[node].min(index[child]);
                }
            } else {
                call_stack.pop();
                if let Some(&(parent, _)) = call_stack.last() {
                    low[parent] = low[parent].min(low[node]);
                }
                if low[node] == index[node] {
                    let mut component = Vec::new();
                    while let Some(top) = stack.pop() {
                        on_stack[top] = false;
                        component.push(top);
                        if top == node {
                            break;
                        }
                    }
                    components.push(component);
                }
            }
        }
    }
    components
}

/// One concrete cycle inside a strongly connected component, following only
/// intra-component edges: a self-loop yields `[node]`, otherwise the returned
/// path is closed by repeating its first member at report time.
fn cycle_chain(component: &[usize], edges: &[Vec<usize>]) -> Vec<usize> {
    let member: std::collections::HashSet<usize> = component.iter().copied().collect();
    let start = component[0];
    if edges[start].contains(&start) {
        return vec![start];
    }
    let mut path = vec![start];
    let mut on_path = std::collections::HashSet::from([start]);
    let mut cursor = vec![0usize];
    while !path.is_empty() {
        let head = *path.last().expect("path is non-empty");
        let position = path.len() - 1;
        let edge_list = &edges[head];
        if cursor[position] < edge_list.len() {
            let child = edge_list[cursor[position]];
            cursor[position] += 1;
            if !member.contains(&child) {
                continue;
            }
            if on_path.contains(&child) {
                let cycle_start = path
                    .iter()
                    .position(|&node| node == child)
                    .expect("on_path");
                return path.split_off(cycle_start);
            }
            on_path.insert(child);
            path.push(child);
            cursor.push(0);
        } else {
            let node = path.pop().expect("path is non-empty");
            on_path.remove(&node);
            cursor.pop();
        }
    }
    // Unreachable for a strongly connected component with more than one member;
    // fall back to the member list so the message never panics.
    component.to_vec()
}
