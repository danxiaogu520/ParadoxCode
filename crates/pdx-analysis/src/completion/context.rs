use crate::semantic::*;
use crate::support::*;
use crate::{
    completion::dynamic_constraints::infer_dynamic_quoted_script_constraints,
    quoted_script::{QuotedScriptParse, QuotedScriptSession},
    types::{CancellationToken, Cancelled},
};
use pdx_engine::AnalysisSnapshot;
use pdx_engine::hir::HirFile;
use pdx_rules::{ProfileRootEntryInsertion, RuleShape};
use pdx_text::{LogicalPath, TextSize};

use crate::semantic::{
    cached_scope_fact_for_property, scope_context_from_hir, semantic_child_scope,
    semantic_initial_scope, semantic_root_context, semantic_rule_key_matches,
    semantic_scope_allows,
};

#[derive(Clone, Debug)]
pub(crate) struct SemanticCompletionContext {
    pub(crate) context: String,
    pub(crate) parent_path: Vec<std::sync::Arc<str>>,
    pub(crate) structural_containers: Vec<(String, Vec<std::sync::Arc<str>>)>,
    pub(crate) alternative_containers: Vec<SemanticCompletionContainer>,
    /// Keys already present in the active container.  Completion uses this to distinguish a
    /// missing required member from one that is already satisfied.
    pub(crate) existing_keys: Vec<String>,
    /// Whether this context was inferred from a dynamic-definition body rather than directly from
    /// the caller's syntax. Dynamic-inferred rules get the highest schema tier.
    pub(crate) dynamic_inferred: bool,
    /// Whether this container is a type's file-root entry scaffold (`root_entries`).
    ///
    /// Only this container suppresses single-instance (`max_occurs = 1`) keys that are already
    /// declared at the document root; ordinary containers keep offering declared keys.
    pub(crate) root_entry_container: bool,
    pub(crate) scope: ScopeContext,
    pub(crate) container_property: Option<ScriptProperty>,
    pub(crate) property: Option<ScriptProperty>,
    pub(crate) quoted_depth: usize,
    pub(crate) embedded_value_context: Option<bool>,
    /// Whether completion sits in a type-instance wrapper container without a concrete
    /// instance under the cursor. A wrapper such as `country_decisions = { … }` only accepts
    /// free-form instance names, so key candidates from the wrapped type must be suppressed.
    pub(crate) wrapper_container: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SemanticCompletionContainer {
    pub(crate) context: String,
    pub(crate) parent_path: Vec<std::sync::Arc<str>>,
    pub(crate) scope: ScopeContext,
}

#[cfg(test)]
pub(crate) fn semantic_completion_context(
    snapshot: &AnalysisSnapshot,
    input: &ParsedInput,
    position: TextSize,
) -> Option<SemanticCompletionContext> {
    crate::types::uncancelled(semantic_completion_context_with_cancellation(
        snapshot,
        input,
        position,
        &CancellationToken::new(),
    ))
}

pub(crate) fn semantic_completion_context_with_cancellation(
    snapshot: &AnalysisSnapshot,
    input: &ParsedInput,
    position: TextSize,
    cancellation: &CancellationToken,
) -> Result<Option<SemanticCompletionContext>, Cancelled> {
    cancellation.checkpoint()?;
    let mut quoted_scripts = QuotedScriptSession::new(cancellation);
    let ParsedContent::Text(parsed) = &input.parsed;
    for root in script_properties(input, parsed.root()) {
        let Some(context) = semantic_root_context(snapshot, &root.key, input.path.as_ref()) else {
            continue;
        };
        let Some(block_range) = root.block_range else {
            continue;
        };
        // Inclusive end: a cursor directly after an unfinished block (`key = { ` at end of
        // file) sits on the half-open range end and must still resolve to the block.
        if position < block_range.start() || position > block_range.end() {
            continue;
        }
        let scope = semantic_initial_scope(snapshot, input, &context, &root.key, root.key_range);
        // A type descriptor with `skip_root_paths` exposes the children at that path as type
        // instances. The HIR and diagnostics paths already keep those instance keys out of the
        // semantic parent path; completion must do the same before traversing the first child.
        let skip_type_instance =
            semantic_completion_skips_type_instances(snapshot, &context, &root.key);
        return semantic_completion_container(
            snapshot,
            SemanticCompletionContainerInput {
                hir: input.hir.as_deref(),
                context,
                parent_path: Vec::new(),
                structural_containers: Vec::new(),
                alternative_containers: Vec::new(),
                dynamic_inferred: false,
                container_property: None,
                skip_type_instance,
                properties: root.block,
                scope,
                position,
                quoted_depth: 0,
                embedded_value_context: None,
                root_entry_container: false,
            },
            &mut quoted_scripts,
        )
        .map(Some);
    }
    // No root key selected a type; the cursor sits on the document root of a still-empty file
    // or on a root gap. A type that declares `root_entries` offers its entry rules through the
    // ordinary semantic container so their shapes, values, and cardinality behave exactly like
    // every other rule (this replaces the former game-profile file-root entry table).
    if let Some(entry_context) = semantic_root_entry_context(snapshot, input.path.as_ref()) {
        let properties = script_properties(input, parsed.root());
        return semantic_completion_container(
            snapshot,
            SemanticCompletionContainerInput {
                hir: input.hir.as_deref(),
                context: entry_context,
                parent_path: Vec::new(),
                structural_containers: Vec::new(),
                alternative_containers: Vec::new(),
                dynamic_inferred: false,
                container_property: None,
                skip_type_instance: false,
                properties,
                scope: ScopeContext::new(snapshot.game_profile_handle()),
                position,
                quoted_depth: 0,
                embedded_value_context: None,
                root_entry_container: true,
            },
            &mut quoted_scripts,
        )
        .map(Some);
    }
    Ok(None)
}

/// Selects the file-root entry context for a path, when the document's type declares one.
///
/// The context is `root:{name}` per the descriptor's `root_entries`; entry rules are ordinary
/// semantic rules under that context. Enumerated type roots (for example on_actions) may use the
/// same container with their `type_root_keys` as the candidate source. Types without either a
/// declaration or an enumerated root set (for example missions, whose root series names are
/// free-form) never get a scaffold.
fn semantic_root_entry_context(
    snapshot: &AnalysisSnapshot,
    logical_path: Option<&LogicalPath>,
) -> Option<String> {
    let logical_path = logical_path?;
    snapshot
        .rules()
        .model()
        .semantic
        .type_descriptors
        .iter()
        .filter_map(|(type_name, descriptor)| {
            let name = descriptor.root_entries.as_deref()?;
            let context = format!("root:{name}");
            if !pdx_engine::hir::semantic_type_path_matches(descriptor, Some(logical_path)) {
                return None;
            }
            let has_rules = snapshot
                .rules()
                .semantic_rules_for_context(&context)
                .next()
                .is_some();
            let has_type_root_keys = snapshot
                .rules()
                .model()
                .semantic
                .type_root_keys
                .get(type_name)
                .is_some_and(|roots| !roots.is_empty());
            let has_profile_root_source = snapshot.game_profile().root_entry_spec(name).is_some();
            (has_rules || has_type_root_keys || has_profile_root_source)
                .then_some((type_name.clone(), context))
        })
        .map(|(_, context)| context)
        .next()
}

/// Returns whether a file-root entry container accepts bare scalar entries.
///
/// Bare root values (for example `westerngfx` in `graphicalculturetype.txt`) must stay on the
/// key-completion path even though the parser also exposes them as scalar values. Other root
/// containers continue to use the ordinary value-context detection.
pub(crate) fn semantic_root_entry_uses_bare_values(
    snapshot: &AnalysisSnapshot,
    context: &SemanticCompletionContext,
) -> bool {
    if !context.root_entry_container {
        return false;
    }
    let Some(entry_name) = context.context.strip_prefix("root:") else {
        return false;
    };
    snapshot
        .game_profile()
        .root_entry_spec(entry_name)
        .is_some_and(|spec| matches!(spec.insertion, ProfileRootEntryInsertion::Bare))
}

struct SemanticCompletionContainerInput<'a> {
    hir: Option<&'a HirFile>,
    context: String,
    parent_path: Vec<std::sync::Arc<str>>,
    structural_containers: Vec<(String, Vec<std::sync::Arc<str>>)>,
    alternative_containers: Vec<SemanticCompletionContainer>,
    dynamic_inferred: bool,
    container_property: Option<ScriptProperty>,
    skip_type_instance: bool,
    properties: Vec<ScriptProperty>,
    scope: ScopeContext,
    position: TextSize,
    quoted_depth: usize,
    embedded_value_context: Option<bool>,
    root_entry_container: bool,
}

fn semantic_completion_container(
    snapshot: &AnalysisSnapshot,
    input: SemanticCompletionContainerInput<'_>,
    quoted_scripts: &mut QuotedScriptSession<'_>,
) -> Result<SemanticCompletionContext, Cancelled> {
    let SemanticCompletionContainerInput {
        hir,
        context,
        parent_path,
        structural_containers,
        alternative_containers,
        dynamic_inferred,
        container_property,
        skip_type_instance,
        properties,
        scope,
        position,
        quoted_depth,
        embedded_value_context,
        root_entry_container,
    } = input;
    for property in &properties {
        let container_range = property.block_range.or_else(|| {
            property
                .quoted_source
                .as_ref()
                .and_then(|_| property.scalar.as_ref().map(|(_, range)| *range))
        });
        let Some(block_range) = container_range else {
            continue;
        };
        if !contains(block_range, position) {
            continue;
        }
        if skip_type_instance
            && semantic_completion_is_type_instance_child(snapshot, &context, &property.key)
        {
            return semantic_completion_container(
                snapshot,
                SemanticCompletionContainerInput {
                    hir,
                    context: context.clone(),
                    parent_path: parent_path.clone(),
                    structural_containers,
                    alternative_containers,
                    dynamic_inferred,
                    container_property: Some(property.clone()),
                    skip_type_instance: false,
                    properties: property.block.clone(),
                    scope: scope.clone(),
                    position,
                    quoted_depth,
                    embedded_value_context,
                    root_entry_container: false,
                },
                quoted_scripts,
            );
        }
        let transparent_wrapper = context.eq_ignore_ascii_case("trigger")
            && snapshot
                .game_profile()
                .is_transparent_scope_wrapper(&property.key);
        // Transition rules for this property come from the current container *and* the
        // structural containers. After a scope link such as `if` moves to its target, the
        // block's `parent_path` is reset, but clause rules like `limit` (parent_path
        // `["if"]`) live under the pushed structural path; without those, descending into
        // such a clause block yields no candidates. Each rule keeps the source path used to
        // compute its destination, mirroring the diagnostics structural/transition split.
        let next_rules = property_transition_rules(
            snapshot,
            &context,
            &parent_path,
            &structural_containers,
            &scope,
            property,
        );
        let next_rule_refs = next_rules.iter().map(|(rule, _)| *rule).collect::<Vec<_>>();
        let cached_child_fact = cached_scope_fact_for_property(CachedScopeFactInput {
            snapshot,
            hir,
            context: &context,
            parent_path: &parent_path,
            property,
            matching: &next_rule_refs,
            selected_alternative: None,
            scope: &scope,
            transparent_wrapper,
        });
        if let Some(fact) = cached_child_fact {
            let current = SemanticCompletionContainer {
                context: context.clone(),
                parent_path: parent_path.clone(),
                scope: scope.clone(),
            };
            let next = SemanticCompletionContainer {
                context: fact.context.clone(),
                parent_path: fact
                    .parent_path
                    .iter()
                    .map(|segment| pdx_engine::intern_shard_string(segment))
                    .collect(),
                scope: scope.clone(),
            };
            let structural_containers = completion_structural_containers(
                snapshot,
                &current,
                &property.key,
                transparent_wrapper,
                &next,
            );
            return semantic_completion_container(
                snapshot,
                SemanticCompletionContainerInput {
                    hir,
                    context: fact.context.clone(),
                    parent_path: fact
                        .parent_path
                        .iter()
                        .map(|segment| pdx_engine::intern_shard_string(segment))
                        .collect(),
                    structural_containers,
                    alternative_containers: Vec::new(),
                    dynamic_inferred,
                    container_property: Some(property.clone()),
                    skip_type_instance: false,
                    properties: property.block.clone(),
                    scope: scope_context_from_hir(snapshot.game_profile_handle(), &fact.state),
                    position,
                    quoted_depth,
                    embedded_value_context,
                    root_entry_container: false,
                },
                quoted_scripts,
            );
        }
        if property.block_range.is_none()
            && let Some(origin) = property.quoted_source.as_ref()
        {
            let argument_context = SemanticCompletionContext {
                context: context.clone(),
                parent_path: parent_path.clone(),
                structural_containers: structural_containers.clone(),
                alternative_containers: alternative_containers.clone(),
                existing_keys: property
                    .block
                    .iter()
                    .map(|child| child.key.to_string())
                    .collect(),
                dynamic_inferred,
                scope: scope.clone(),
                container_property: container_property.clone(),
                property: Some(property.clone()),
                quoted_depth,
                embedded_value_context,
                wrapper_container: skip_type_instance,
                root_entry_container: false,
            };
            let inferred = infer_dynamic_quoted_script_constraints(
                snapshot,
                &argument_context,
                property,
                quoted_scripts.cancellation(),
            )?;
            if let Some(primary) = inferred.first()
                && let QuotedScriptParse::Parsed(script) =
                    quoted_scripts.parse(origin.source(), quoted_depth)?
                && let Some(decoded_position) = origin.decoded_position(&script, position)
            {
                let (quoted_properties, _) = quoted_script_container(&script, origin);
                let value_context =
                    script_line_value_context(script.parsed().source(), decoded_position);
                let alternatives = inferred
                    .iter()
                    .skip(1)
                    .map(|site| SemanticCompletionContainer {
                        context: site.context.clone(),
                        parent_path: site.parent_path.clone(),
                        scope: site.scope.clone(),
                    })
                    .collect();
                return semantic_completion_container(
                    snapshot,
                    SemanticCompletionContainerInput {
                        hir: None,
                        context: primary.context.clone(),
                        parent_path: primary.parent_path.clone(),
                        structural_containers: Vec::new(),
                        alternative_containers: alternatives,
                        dynamic_inferred: true,
                        container_property: Some(property.clone()),
                        skip_type_instance: false,
                        properties: quoted_properties,
                        scope: primary.scope.clone(),
                        position,
                        quoted_depth: quoted_depth.saturating_add(1),
                        embedded_value_context: Some(value_context),
                        root_entry_container: false,
                    },
                    quoted_scripts,
                );
            }
        }
        let selected = semantic_transition_candidates(&next_rule_refs, None, property, &scope);
        let next_rules = next_rules
            .into_iter()
            .filter(|(rule, _)| selected.iter().any(|selected| selected.id == rule.id))
            .collect::<Vec<_>>();
        let quoted_script_rule = next_rules
            .iter()
            .find(|(rule, _)| matches!(rule.shape, RuleShape::QuotedScript))
            .map(|(rule, _)| *rule);
        let mut destinations = Vec::<SemanticCompletionContainer>::new();
        for (rule, source_path) in &next_rules {
            let (destination_context, destination_path) =
                rule.child_context.as_deref().map_or_else(
                    || {
                        let mut path = source_path.clone();
                        if !transparent_wrapper {
                            path.push(property.key.clone());
                        }
                        (context.clone(), path)
                    },
                    |child_context| (child_context.to_owned(), Vec::new()),
                );
            let destination = SemanticCompletionContainer {
                context: destination_context,
                parent_path: destination_path,
                scope: semantic_child_scope(snapshot, &scope, rule),
            };
            if !destinations
                .iter()
                .any(|known| semantic_completion_containers_equal(known, &destination))
            {
                destinations.push(destination);
            }
        }
        let primary = destinations.first().cloned().unwrap_or_else(|| {
            let mut path = parent_path.clone();
            if !transparent_wrapper {
                path.push(property.key.clone());
            }
            SemanticCompletionContainer {
                context: context.clone(),
                parent_path: path,
                scope: scope.clone(),
            }
        });
        let alternative_containers = destinations.into_iter().skip(1).collect::<Vec<_>>();
        let current = SemanticCompletionContainer {
            context: context.clone(),
            parent_path: parent_path.clone(),
            scope: scope.clone(),
        };
        let structural_containers = completion_structural_containers(
            snapshot,
            &current,
            &property.key,
            transparent_wrapper,
            &primary,
        );
        let quoted_script = if quoted_script_rule.is_some()
            && property.block_range.is_none()
            && let Some(origin) = property.quoted_source.as_ref()
        {
            match quoted_scripts.parse(origin.source(), quoted_depth)? {
                QuotedScriptParse::Parsed(script) => Some((origin, script)),
                QuotedScriptParse::Opaque | QuotedScriptParse::Limited(_) => None,
            }
        } else {
            None
        };
        if let Some((origin, script)) = quoted_script
            && let Some(decoded_position) = origin.decoded_position(&script, position)
        {
            let (quoted_properties, _) = quoted_script_container(&script, origin);
            let value_context =
                script_line_value_context(script.parsed().source(), decoded_position);
            return semantic_completion_container(
                snapshot,
                SemanticCompletionContainerInput {
                    hir: None,
                    context: primary.context,
                    parent_path: primary.parent_path,
                    structural_containers,
                    alternative_containers,
                    dynamic_inferred,
                    container_property: Some(property.clone()),
                    skip_type_instance: false,
                    properties: quoted_properties,
                    scope: primary.scope,
                    position,
                    quoted_depth: quoted_depth.saturating_add(1),
                    embedded_value_context: Some(value_context),
                    root_entry_container: false,
                },
                quoted_scripts,
            );
        }
        return semantic_completion_container(
            snapshot,
            SemanticCompletionContainerInput {
                hir,
                context: primary.context,
                parent_path: primary.parent_path,
                structural_containers,
                alternative_containers,
                dynamic_inferred,
                container_property: Some(property.clone()),
                skip_type_instance: false,
                properties: property.block.clone(),
                scope: primary.scope,
                position,
                quoted_depth,
                embedded_value_context,
                root_entry_container: false,
            },
            quoted_scripts,
        );
    }
    let existing_keys = properties
        .iter()
        .map(|property| property.key.to_string())
        .collect();
    let property = properties.into_iter().find(|property| {
        contains(property.range, position)
            // A value position directly after an unfinished `key = ` sits on the half-open
            // end of the property range; accept it so the value completion still fires.
            || (position >= property.key_range.start() && property.range.end() == position)
    });
    Ok(SemanticCompletionContext {
        context,
        parent_path,
        structural_containers,
        alternative_containers,
        existing_keys,
        dynamic_inferred,
        scope,
        container_property,
        property,
        quoted_depth,
        embedded_value_context,
        wrapper_container: skip_type_instance,
        root_entry_container,
    })
}

fn semantic_completion_skips_type_instances(
    snapshot: &AnalysisSnapshot,
    context: &str,
    root_key: &str,
) -> bool {
    let Some(type_name) = context.strip_prefix("type:") else {
        return false;
    };
    snapshot
        .rules()
        .model()
        .semantic
        .type_descriptors
        .get(type_name)
        .is_some_and(|descriptor| {
            descriptor.skip_root_paths.iter().any(|path| {
                path.first().is_some_and(|head| {
                    head.eq_ignore_ascii_case("any") || head.eq_ignore_ascii_case(root_key)
                })
            })
        })
}

fn semantic_completion_is_type_instance_child(
    snapshot: &AnalysisSnapshot,
    context: &str,
    child_key: &str,
) -> bool {
    let Some(type_name) = context.strip_prefix("type:") else {
        return false;
    };
    let Some(descriptor) = snapshot
        .rules()
        .model()
        .semantic
        .type_descriptors
        .get(type_name)
    else {
        return false;
    };
    descriptor
        .type_key_filter
        .as_ref()
        .is_none_or(|(values, negate)| {
            values
                .iter()
                .any(|value| value.eq_ignore_ascii_case(child_key))
                != *negate
        })
}

fn script_line_value_context(source: &str, position: TextSize) -> bool {
    let offset = usize::try_from(position)
        .unwrap_or(source.len())
        .min(source.len());
    let line_start = source[..offset].rfind('\n').map_or(0, |index| index + 1);
    let line = &source[line_start..offset];
    let equals = line.rfind('=');
    let open = line.rfind('{');
    equals.is_some_and(|equals| open.is_none_or(|open| equals > open))
}

pub(crate) fn semantic_completion_containers_equal(
    left: &SemanticCompletionContainer,
    right: &SemanticCompletionContainer,
) -> bool {
    left.context.eq_ignore_ascii_case(&right.context)
        && left.parent_path.len() == right.parent_path.len()
        && left
            .parent_path
            .iter()
            .zip(&right.parent_path)
            .all(|(left, right)| left.eq_ignore_ascii_case(right))
        && left.scope == right.scope
}

/// Returns the transition rules that can drive one property's block, drawing on both the
/// current container and the structural containers carried by the enclosing block. After a
/// scope link such as `if` moves to its target, `parent_path` is reset, so clause rules like
/// `limit` (parent_path `["if"]`) are only reachable through the pushed structural path. Each
/// rule is paired with the source path used to compute its destination.
pub(crate) fn property_transition_rules<'rule>(
    snapshot: &'rule AnalysisSnapshot,
    context: &str,
    parent_path: &[std::sync::Arc<str>],
    structural_containers: &[(String, Vec<std::sync::Arc<str>>)],
    scope: &ScopeContext,
    property: &ScriptProperty,
) -> Vec<(&'rule pdx_rules::SemanticRule, Vec<std::sync::Arc<str>>)> {
    let mut out = Vec::new();
    let mut seen = Vec::new();
    for (source_context, source_path) in structural_containers
        .iter()
        .map(|(container_context, container_path)| {
            (container_context.as_str(), container_path.as_slice())
        })
        .chain(std::iter::once((context, parent_path)))
    {
        for rule in semantic_rules_for_container(snapshot, source_context, source_path, scope) {
            if matches!(rule.shape, RuleShape::LeafValue)
                || !semantic_rule_key_matches(snapshot, rule, source_path, &property.key)
                || !semantic_scope_allows(rule, scope)
                || seen.iter().any(|id| id == &rule.id)
            {
                continue;
            }
            seen.push(rule.id.clone());
            out.push((rule, source_path.to_vec()));
        }
    }
    out
}

pub(crate) fn completion_structural_containers(
    snapshot: &AnalysisSnapshot,
    current: &SemanticCompletionContainer,
    property_key: &str,
    transparent_wrapper: bool,
    next: &SemanticCompletionContainer,
) -> Vec<(String, Vec<std::sync::Arc<str>>)> {
    let mut structural_path = current.parent_path.clone();
    if !transparent_wrapper {
        structural_path.push(pdx_engine::intern_shard_string(property_key));
    }
    let destination_is_structural = next.context.eq_ignore_ascii_case(&current.context)
        && next.parent_path.len() == structural_path.len()
        && next
            .parent_path
            .iter()
            .zip(&structural_path)
            .all(|(left, right)| left.eq_ignore_ascii_case(right));
    if destination_is_structural
        || semantic_rules_for_container(
            snapshot,
            &current.context,
            &structural_path,
            &current.scope,
        )
        .is_empty()
    {
        Vec::new()
    } else {
        vec![(current.context.clone(), structural_path)]
    }
}

pub(crate) fn semantic_rules_for_container<'a>(
    snapshot: &'a AnalysisSnapshot,
    context: &str,
    parent_path: &[std::sync::Arc<str>],
    _scope: &ScopeContext,
) -> Vec<&'a pdx_rules::SemanticRule> {
    let mut candidates = snapshot
        .rules()
        .semantic_rules_for_context(context)
        .collect::<Vec<_>>();
    if let Some(type_name) = context.strip_prefix("type:") {
        candidates.extend(
            snapshot
                .rules()
                .semantic_rules_for_context(&format!("root:{type_name}")),
        );
        candidates.sort_by(|left, right| left.id.cmp(&right.id));
    }
    candidates
        .into_iter()
        .filter(|rule| semantic_parent_path_matches(snapshot, &rule.parent_path, parent_path))
        .collect()
}
