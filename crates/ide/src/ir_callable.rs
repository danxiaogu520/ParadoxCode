//! Binding-aware Callable replay over the compiled schema tree.
//! Each usage is an intersection; overloads at one usage are alternatives.
use std::collections::{BTreeMap, BTreeSet};

use engine::AnalysisSnapshot;
use hir::{ScopeState, TemplateFragment, TemplateToken};
use rules::ir::{FieldId, Matcher, MatcherId};

use crate::ir_semantic::{self, WorkspaceFacts};
use crate::types::{CancellationToken, Cancelled};

pub(crate) use hir::callable::Domain;

#[derive(Clone, Debug)]
pub(crate) struct ParameterSite {
    pub(crate) origin: (String, String),
    pub(crate) domain: Domain,
    pub(crate) token: TemplateToken,
    pub(crate) state: ScopeState,
}

impl ParameterSite {
    pub(crate) fn rendered_value(&self, parameter: &str, value: &str) -> Option<String> {
        render_candidate(&self.token, parameter, value)
    }
    pub(crate) fn accepts_candidate(
        &self,
        snapshot: &AnalysisSnapshot,
        parameter: &str,
        value: &str,
    ) -> bool {
        if !self.accepts(snapshot, parameter, value) {
            return false;
        }
        let Some(rendered) = render_candidate(&self.token, parameter, value) else {
            return true;
        };
        let Domain::Value(matchers) = &self.domain else {
            return true;
        };
        matchers
            .iter()
            .any(|matcher| candidate_matches(snapshot, *matcher, &rendered, &self.state))
    }
    pub(crate) fn accepts(
        &self,
        snapshot: &AnalysisSnapshot,
        parameter: &str,
        value: &str,
    ) -> bool {
        let Some(rendered) = render_candidate(&self.token, parameter, value) else {
            return true;
        };
        let ir = snapshot.ir();
        match &self.domain {
            Domain::Value(matchers) => matchers.iter().any(|matcher| {
                rendered_value_matches(
                    snapshot,
                    *matcher,
                    &rendered,
                    &self.state,
                    affixes(&self.token, parameter)
                        .is_some_and(|(head, tail)| !head.is_empty() || !tail.is_empty()),
                )
            }),
            Domain::Key { schema, shape } => ir.lookup(*schema, &rendered, *shape).any(|id| {
                field_allowed(snapshot, id, &self.state)
                    && ir_semantic::matcher_matches(
                        ir,
                        ir.field(id).key,
                        &rendered,
                        &WorkspaceFacts { snapshot },
                    )
            }),
            Domain::Payload { .. } | Domain::Unresolved => true,
        }
    }

    pub(crate) fn candidates(
        &self,
        snapshot: &AnalysisSnapshot,
        parameter: &str,
        prefix: &str,
    ) -> Vec<String> {
        let ir = snapshot.ir();
        let Some((head, tail)) = affixes(&self.token, parameter) else {
            return Vec::new();
        };
        let rendered_prefix = format!("{head}{prefix}");
        let mut values = match &self.domain {
            Domain::Value(matchers) => matchers
                .iter()
                .flat_map(|matcher| {
                    ir_semantic::spellings_with_state(
                        ir,
                        *matcher,
                        snapshot,
                        &rendered_prefix,
                        Some(&self.state),
                    )
                })
                .collect::<Vec<_>>(),
            Domain::Key { schema, shape } => ir
                .fields(*schema)
                .into_iter()
                .filter(|id| {
                    ir.shape(*id) == Some(*shape) && field_allowed(snapshot, *id, &self.state)
                })
                .flat_map(|id| {
                    ir_semantic::spellings_with_state(
                        ir,
                        ir.field(id).key,
                        snapshot,
                        &rendered_prefix,
                        Some(&self.state),
                    )
                })
                .collect(),
            Domain::Payload { .. } => Vec::new(),
            Domain::Unresolved => ir
                .scopes
                .registers
                .iter()
                .map(|register| ir.strings().resolve(register.name).to_ascii_uppercase())
                .collect(),
        };
        values.retain_mut(|value| {
            if value.len() < head.len() + tail.len()
                || !value
                    .get(..head.len())
                    .is_some_and(|part| part.eq_ignore_ascii_case(&head))
                || !value
                    .get(value.len() - tail.len()..)
                    .is_some_and(|part| part.eq_ignore_ascii_case(&tail))
            {
                return false;
            }
            *value = value[head.len()..value.len() - tail.len()].to_owned();
            true
        });
        values
    }
}

fn candidate_matches(
    snapshot: &AnalysisSnapshot,
    matcher: MatcherId,
    value: &str,
    state: &ScopeState,
) -> bool {
    let ir = snapshot.ir();
    match ir.matcher(matcher) {
        Matcher::Loc => crate::semantic::workspace_member(snapshot, "localisation", value),
        Matcher::Union(items) => items
            .iter()
            .any(|item| candidate_matches(snapshot, *item, value, state)),
        _ => ir_semantic::matcher_matches_with_state(
            snapshot,
            ir,
            matcher,
            value,
            &WorkspaceFacts { snapshot },
            state,
        ),
    }
}

fn rendered_value_matches(
    snapshot: &AnalysisSnapshot,
    matcher: MatcherId,
    value: &str,
    state: &ScopeState,
    affixed: bool,
) -> bool {
    let ir = snapshot.ir();
    match ir.matcher(matcher) {
        Matcher::Ref(rules::ir::RefTarget::Type { type_id, .. }) if affixed => {
            let info = ir.type_info(*type_id);
            (info.builtin.is_empty()
                && !crate::semantic::workspace_kind_has_members(
                    snapshot,
                    ir.strings().resolve(info.name),
                ))
                || ir_semantic::matcher_matches_with_state(
                    snapshot,
                    ir,
                    matcher,
                    value,
                    &WorkspaceFacts { snapshot },
                    state,
                )
        }
        Matcher::Union(items) => items
            .iter()
            .any(|item| rendered_value_matches(snapshot, *item, value, state, affixed)),
        _ => ir_semantic::matcher_matches_with_state(
            snapshot,
            ir,
            matcher,
            value,
            &WorkspaceFacts { snapshot },
            state,
        ),
    }
}

fn field_allowed(snapshot: &AnalysisSnapshot, id: FieldId, state: &ScopeState) -> bool {
    let ir = snapshot.ir();
    ir.field(id).scope.as_ref().is_none_or(|effect| {
        effect.scopes_in.is_empty()
            || state
                .current
                .first()
                .is_none_or(|current| ir_semantic::scope_allows(ir, current, &effect.scopes_in))
    })
}

fn map_sites(sites: Vec<hir::callable::ParameterSite>) -> Vec<ParameterSite> {
    sites
        .into_iter()
        .map(|site| ParameterSite {
            origin: site.origin,
            domain: site.domain,
            token: site.token,
            state: site.state,
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn parameter_sites(
    snapshot: &AnalysisSnapshot,
    kind: &str,
    name: &str,
    parameter: &str,
    bindings: &BTreeMap<String, String>,
    state: ScopeState,
    cancellation: &CancellationToken,
) -> Result<Vec<ParameterSite>, Cancelled> {
    hir::callable::parameter_sites(
        snapshot.ir(),
        &WorkspaceFacts { snapshot },
        kind,
        name,
        parameter,
        bindings,
        state,
        &mut || cancellation.checkpoint(),
    )
    .map(map_sites)
}
pub(crate) fn definition_parameter_sites(
    snapshot: &AnalysisSnapshot,
    kind: &str,
    name: &str,
    parameter: &str,
    cancellation: &CancellationToken,
) -> Result<Vec<ParameterSite>, Cancelled> {
    hir::callable::definition_parameter_sites(
        snapshot.ir(),
        &WorkspaceFacts { snapshot },
        kind,
        name,
        parameter,
        &mut || cancellation.checkpoint(),
    )
    .map(map_sites)
}
#[allow(clippy::too_many_arguments)]
pub(crate) fn parameter_symbol_sites(
    snapshot: &AnalysisSnapshot,
    kind: &str,
    name: &str,
    parameter: &str,
    bindings: &BTreeMap<String, String>,
    state: ScopeState,
    cancellation: &CancellationToken,
) -> Result<Vec<ParameterSite>, Cancelled> {
    hir::callable::parameter_symbol_sites(
        snapshot.ir(),
        &WorkspaceFacts { snapshot },
        kind,
        name,
        parameter,
        bindings,
        state,
        &mut || cancellation.checkpoint(),
    )
    .map(map_sites)
}
pub(crate) fn missing_parameters(
    snapshot: &AnalysisSnapshot,
    kind: &str,
    name: &str,
    bindings: &BTreeMap<String, String>,
    state: ScopeState,
    cancellation: &CancellationToken,
) -> Result<BTreeSet<String>, Cancelled> {
    hir::callable::missing_parameters(
        snapshot.ir(),
        &WorkspaceFacts { snapshot },
        kind,
        name,
        bindings,
        state,
        &mut || cancellation.checkpoint(),
    )
}
pub(crate) fn callable_kind(ir: &rules::ir::RulesIr, matcher: MatcherId) -> Option<String> {
    hir::callable::callable_kind(ir, matcher)
}

fn render_candidate(token: &TemplateToken, parameter: &str, value: &str) -> Option<String> {
    token
        .fragments
        .iter()
        .map(|fragment| match fragment {
            TemplateFragment::Literal(text) => Some(text.as_str()),
            TemplateFragment::Parameter { name, .. } if name.eq_ignore_ascii_case(parameter) => {
                Some(value)
            }
            _ => None,
        })
        .collect::<Option<Vec<_>>>()
        .map(|parts| parts.concat())
}

fn affixes(token: &TemplateToken, parameter: &str) -> Option<(String, String)> {
    let mut head = String::new();
    let mut tail = String::new();
    let mut found = false;
    for fragment in &token.fragments {
        match fragment {
            TemplateFragment::Literal(text) => {
                if found {
                    tail.push_str(text);
                } else {
                    head.push_str(text);
                }
            }
            TemplateFragment::Parameter { name, .. }
                if !found && name.eq_ignore_ascii_case(parameter) =>
            {
                found = true
            }
            _ => return None,
        }
    }
    found.then_some((head, tail))
}

pub(crate) fn invocation_bindings(
    hir: &hir::HirFile,
    invocation: &hir::HirProperty,
) -> BTreeMap<String, String> {
    hir.properties_in_range(invocation.range)
        .filter(|property| {
            property.path.len() == invocation.path.len() + 1
                && property.path.starts_with(&invocation.path)
                && invocation.range.start() <= property.range.start()
                && property.range.end() <= invocation.range.end()
        })
        .map(|property| {
            (
                property.key.to_ascii_lowercase(),
                property
                    .scalar
                    .as_ref()
                    .map_or(String::new(), |scalar| scalar.value.to_string()),
            )
        })
        .collect()
}

pub(crate) fn invocation_state(hir: &hir::HirFile, invocation: &hir::HirProperty) -> ScopeState {
    hir.field_fact_at(invocation.key_range)
        .and_then(|field| {
            hir.schema_facts()
                .iter()
                .filter(|fact| {
                    fact.schema == field.schema
                        && crate::support::contains(fact.range, invocation.key_range.start())
                })
                .min_by_key(|fact| fact.range.len())
        })
        .map(|fact| fact.state.clone())
        .unwrap_or(ScopeState {
            root: hir::ScopeValue::Unknown,
            current: vec![hir::ScopeValue::Unknown],
            from: Vec::new(),
            previous: Vec::new(),
        })
}
