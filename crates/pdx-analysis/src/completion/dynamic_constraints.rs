//! Replay of dynamic-definition argument constraints from report rows.
//!
//! The per-query symbolic instantiation this module used to run is gone:
//! every usage site of a parameter is derived once per revision into
//! [`DynamicRuleRow`] site rows (see `dynamic_rules.rs`), and this module
//! replays them against the caller's live invocation — pruning inactive
//! conditional branches by the caller's bindings, replaying the site's
//! scope chain onto the caller's scope (gates checked where they were
//! crossed, fan-out groups evaluated level-wide so a scope-excluded
//! container's fallback rows arm exactly when every branch was gated out),
//! filtering matcher alternatives by their own allowed scopes, and following
//! forwarding edges into callee rows instead of inlining the callee.
//! Completion and diagnostics thereby consume one derivation and cannot
//! drift.

use std::collections::{BTreeMap, HashSet};

use pdx_engine::AnalysisSnapshot;
use pdx_rules::{KeyMatcher, ValueMatcher};

use crate::dynamic_rules::{
    DynamicAffixSegment, DynamicAffixedSiteRow, DynamicForwardSiteRow, DynamicForwardValue,
    DynamicKeyRenderSiteRow, DynamicQuotedSiteRow, DynamicRuleRow, DynamicScopeStep,
    DynamicSiteGuard, DynamicValueSiteRow, dynamic_rule_row,
};
use crate::semantic::{
    enum_members, semantic_rule_key_matches, semantic_scope_allows, workspace_member_index,
};
use crate::support::{ScopeContext, ScriptProperty};
use crate::types::{CancellationToken, Cancelled};

use super::{SemanticCompletionContext, semantic_rules_for_completion};

#[derive(Clone, Debug)]
pub(crate) struct DynamicValueConstraintSite {
    pub(crate) matchers: Vec<ValueMatcher>,
    pub(crate) scope: ScopeContext,
}

#[derive(Clone, Debug)]
pub(crate) struct DynamicQuotedScriptConstraintSite {
    pub(crate) context: String,
    pub(crate) parent_path: Vec<std::sync::Arc<str>>,
    pub(crate) scope: ScopeContext,
}

/// One usage site where the completed parameter renders into a statement key:
/// the argument value becomes a key (whole or inside literal affixes), so the
/// candidate set is derived from the rule keys valid at that site. Both
/// affixes empty means the parameter is the entire key (a wildcard dispatch —
/// any command name of the site context is acceptable).
#[derive(Clone, Debug)]
pub(crate) struct DynamicKeyRenderSite {
    pub(crate) prefix: String,
    pub(crate) suffix: String,
    pub(crate) context: String,
    pub(crate) parent_path: Vec<std::sync::Arc<str>>,
    pub(crate) scope: ScopeContext,
}

#[derive(Clone, Debug, Default)]
struct DynamicArgumentConstraints {
    values: Vec<DynamicValueConstraintSite>,
    quoted_scripts: Vec<DynamicQuotedScriptConstraintSite>,
    key_renders: Vec<DynamicKeyRenderSite>,
    /// True when at least one usage site constrains the parameter value to a
    /// non-enumerable shape (numbers, opaque strings, dynamic sets): no item
    /// can be offered, and callers must treat the value as constrained.
    unenumerable: bool,
}

/// One caller-bound argument value: `Target` is the parameter being
/// completed or validated, `Concrete` a written scalar, `Unknown` a block
/// value. Presence in the bindings map is what conditional pruning checks,
/// so `Unknown` still counts as bound.
#[derive(Clone, Debug)]
enum BoundArgument {
    Concrete(String),
    Target,
    Unknown,
}

/// The inferred completion story for a dynamic parameter's value.
#[derive(Clone, Debug, Default)]
pub(crate) struct DynamicValueConstraints {
    pub(crate) sites: Vec<DynamicValueConstraintSite>,
    pub(crate) key_renders: Vec<DynamicKeyRenderSite>,
    /// At least one usage site constrains the value to a non-enumerable
    /// shape, so nothing can be offered yet the value is not free-form.
    pub(crate) unenumerable: bool,
}

pub(crate) fn infer_dynamic_value_constraints(
    snapshot: &AnalysisSnapshot,
    context: &SemanticCompletionContext,
    target: &ScriptProperty,
    _cancellation: &CancellationToken,
) -> Result<DynamicValueConstraints, Cancelled> {
    let constraints = infer_dynamic_argument_constraints(snapshot, context, target);
    Ok(DynamicValueConstraints {
        sites: constraints.values,
        key_renders: constraints.key_renders,
        unenumerable: constraints.unenumerable,
    })
}

pub(crate) fn infer_dynamic_quoted_script_constraints(
    snapshot: &AnalysisSnapshot,
    context: &SemanticCompletionContext,
    target: &ScriptProperty,
    _cancellation: &CancellationToken,
) -> Result<Vec<DynamicQuotedScriptConstraintSite>, Cancelled> {
    Ok(infer_dynamic_argument_constraints(snapshot, context, target).quoted_scripts)
}

fn infer_dynamic_argument_constraints(
    snapshot: &AnalysisSnapshot,
    context: &SemanticCompletionContext,
    target: &ScriptProperty,
) -> DynamicArgumentConstraints {
    let Some(invocation) = context.container_property.as_ref() else {
        return DynamicArgumentConstraints::default();
    };
    let Some((owner_kind, owner_name, caller_scope)) =
        dynamic_parameter_owner(snapshot, context, target, invocation)
    else {
        return DynamicArgumentConstraints::default();
    };
    let bindings = invocation_bindings(invocation, Some(target));
    infer_argument_constraints(snapshot, &owner_kind, &owner_name, &bindings, &caller_scope)
}

/// Per-render-site (context, parent_path, scope) for one quoted payload
/// argument, so diagnostics can validate the argument's script exactly where
/// the definition body splices it. This is the same shared inference
/// completion consumes; `target` is the argument being validated.
pub(crate) fn infer_dynamic_quoted_payload_sites(
    snapshot: &AnalysisSnapshot,
    owner_kind: &str,
    owner_name: &str,
    invocation: &ScriptProperty,
    target: &ScriptProperty,
    caller_scope: &ScopeContext,
    _cancellation: &CancellationToken,
) -> Result<Vec<DynamicQuotedScriptConstraintSite>, Cancelled> {
    let bindings = invocation_bindings(invocation, Some(target));
    Ok(
        infer_argument_constraints(snapshot, owner_kind, owner_name, &bindings, caller_scope)
            .quoted_scripts,
    )
}

fn infer_argument_constraints(
    snapshot: &AnalysisSnapshot,
    owner_kind: &str,
    owner_name: &str,
    bindings: &BTreeMap<String, BoundArgument>,
    caller_scope: &ScopeContext,
) -> DynamicArgumentConstraints {
    let Some(row) = dynamic_rule_row(snapshot, owner_kind, owner_name) else {
        return DynamicArgumentConstraints::default();
    };
    let Some(target_parameter) = bindings
        .iter()
        .find(|(_, value)| matches!(value, BoundArgument::Target))
        .map(|(name, _)| name.clone())
    else {
        return DynamicArgumentConstraints::default();
    };
    let mut replayer = RowReplayer {
        snapshot,
        bindings: bindings.clone(),
        visited: Vec::new(),
        values: Vec::new(),
        quoted_scripts: Vec::new(),
        key_renders: Vec::new(),
        unenumerable: false,
    };
    replayer.replay_parameter(&row, &target_parameter, caller_scope);
    DynamicArgumentConstraints {
        values: replayer.values,
        quoted_scripts: replayer.quoted_scripts,
        key_renders: replayer.key_renders,
        unenumerable: replayer.unenumerable,
    }
}

pub(crate) fn dynamic_parameter_owner(
    snapshot: &AnalysisSnapshot,
    context: &SemanticCompletionContext,
    target: &ScriptProperty,
    invocation: &ScriptProperty,
) -> Option<(String, String, ScopeContext)> {
    semantic_rules_for_completion(snapshot, context)
        .into_iter()
        .find_map(|candidate| {
            let KeyMatcher::Enum(enum_name) = &candidate.rule.key else {
                return None;
            };
            if !enum_name.eq_ignore_ascii_case("scripted_effect_params")
                || !semantic_rule_key_matches(
                    snapshot,
                    candidate.rule,
                    candidate.parent_path,
                    &target.key,
                )
                || !semantic_scope_allows(candidate.rule, candidate.scope)
            {
                return None;
            }
            let owner_kind = candidate
                .rule
                .parent_path
                .last()?
                .strip_prefix('<')?
                .strip_suffix('>')?;
            if !crate::semantic::dynamic_definition_type(snapshot, owner_kind) {
                return None;
            }
            let owner_name = candidate.parent_path.last()?;
            if !invocation.key.eq_ignore_ascii_case(owner_name) {
                return None;
            }
            Some((
                owner_kind.to_owned(),
                owner_name.to_string(),
                candidate.scope.clone(),
            ))
        })
}

fn invocation_bindings(
    invocation: &ScriptProperty,
    target: Option<&ScriptProperty>,
) -> BTreeMap<String, BoundArgument> {
    let mut bindings = BTreeMap::new();
    for argument in &invocation.block {
        let value = if target.is_some_and(|target| target.range == argument.range) {
            BoundArgument::Target
        } else if let Some((value, _)) = &argument.scalar {
            BoundArgument::Concrete(value.to_string())
        } else {
            BoundArgument::Unknown
        };
        bindings.insert(argument.key.to_ascii_lowercase(), value);
    }
    bindings
}

/// One site row of any type, flattened for the two-pass replay.
enum SiteRef<'r> {
    Value(&'r DynamicValueSiteRow),
    Affixed(&'r DynamicAffixedSiteRow),
    KeyRender(&'r DynamicKeyRenderSiteRow),
    Quoted(&'r DynamicQuotedSiteRow),
    Forward(&'r DynamicForwardSiteRow),
}

impl SiteRef<'_> {
    fn chain(&self) -> &[DynamicScopeStep] {
        match self {
            SiteRef::Value(site) => &site.chain,
            SiteRef::Affixed(site) => &site.chain,
            SiteRef::KeyRender(site) => &site.chain,
            SiteRef::Quoted(site) => &site.chain,
            SiteRef::Forward(site) => &site.chain,
        }
    }

    fn guards(&self) -> &[DynamicSiteGuard] {
        match self {
            SiteRef::Value(site) => &site.guards,
            SiteRef::Affixed(site) => &site.guards,
            SiteRef::KeyRender(site) => &site.guards,
            SiteRef::Quoted(site) => &site.guards,
            SiteRef::Forward(site) => &site.guards,
        }
    }

    fn fallback_groups(&self) -> &[u64] {
        match self {
            SiteRef::Value(site) => &site.fallback_groups,
            SiteRef::Affixed(site) => &site.fallback_groups,
            SiteRef::KeyRender(site) => &site.fallback_groups,
            SiteRef::Quoted(site) => &site.fallback_groups,
            SiteRef::Forward(site) => &site.fallback_groups,
        }
    }
}

/// Outcome of replaying one row's chain: the folded scope, whether every
/// gate admitted the scope at its step, and the fan-out groups whose gates
/// admitted. Every gated fan-out branch stamps a witness gate (group-tagged,
/// possibly scope-empty) on each row under it, so a group survives a replay
/// when at least one witness gate admits.
struct ChainReplay {
    scope: ScopeContext,
    admitted: bool,
    admitted_groups: Vec<u64>,
}

/// Replays one parameter's site rows (and its forwarding edges) into
/// caller-side constraint sites.
struct RowReplayer<'a> {
    snapshot: &'a AnalysisSnapshot,
    bindings: BTreeMap<String, BoundArgument>,
    /// Dynamic definitions already entered through forwarding edges; a cycle
    /// repeats no rows (genuine definition cycles are also rejected at the
    /// definition site).
    visited: Vec<(String, String)>,
    values: Vec<DynamicValueConstraintSite>,
    quoted_scripts: Vec<DynamicQuotedScriptConstraintSite>,
    key_renders: Vec<DynamicKeyRenderSite>,
    unenumerable: bool,
}

impl<'a> RowReplayer<'a> {
    /// Two passes per definition level, mirroring the symbolic walker's
    /// alternative filtering exactly. The first replays every row's chain
    /// and collects which fan-out groups survived — some gated branch
    /// admitted the replayed scope at its witness gate; the second keeps
    /// rows whose chain fully admitted and whose fallback branches did not
    /// arm, recursing through forwarding edges into callee levels (each
    /// level evaluating its own groups). Guards take no part in group
    /// survival: a branch row and its fallback twin share guards, so both
    /// would drop together anyway.
    fn replay_parameter(
        &mut self,
        row: &DynamicRuleRow,
        parameter: &str,
        entry_scope: &ScopeContext,
    ) {
        let Some(parameter) = row
            .parameters
            .iter()
            .find(|candidate| candidate.name.eq_ignore_ascii_case(parameter))
        else {
            return;
        };
        let sites: Vec<SiteRef<'_>> = parameter
            .value_sites
            .iter()
            .map(SiteRef::Value)
            .chain(parameter.affixed_value_sites.iter().map(SiteRef::Affixed))
            .chain(parameter.key_render_sites.iter().map(SiteRef::KeyRender))
            .chain(parameter.quoted_sites.iter().map(SiteRef::Quoted))
            .chain(parameter.forward_sites.iter().map(SiteRef::Forward))
            .collect();
        let mut surviving: HashSet<u64> = HashSet::new();
        let mut folded: Vec<ChainReplay> = Vec::with_capacity(sites.len());
        for site in &sites {
            let replay = self.replay_chain(site.chain(), entry_scope);
            surviving.extend(replay.admitted_groups.iter().copied());
            folded.push(replay);
        }
        for (site, replay) in sites.iter().zip(folded) {
            if !replay.admitted {
                continue;
            }
            if site
                .fallback_groups()
                .iter()
                .any(|group| surviving.contains(group))
            {
                continue;
            }
            if !self.guards_active(site.guards()) {
                continue;
            }
            match site {
                SiteRef::Value(site) => self.emit_value_site(site, &replay.scope),
                SiteRef::Affixed(site) => self.emit_affixed_site(site, &replay.scope),
                SiteRef::KeyRender(site) => self.emit_key_render_site(site, &replay.scope),
                SiteRef::Quoted(site) => self.emit_quoted_site(site, &replay.scope),
                SiteRef::Forward(site) => self.emit_forward(site, replay.scope),
            }
        }
    }

    /// A conditional branch is active when its guard parameter is bound
    /// (any value, block included) — or unbound, for a negated guard.
    fn guards_active(&self, guards: &[DynamicSiteGuard]) -> bool {
        guards.iter().all(|guard| {
            let present = self.bindings.contains_key(&guard.name);
            present != guard.negated
        })
    }

    /// Replays one row's chain onto the entry scope: transitions apply,
    /// gates check the scope current at their step — the exact order the
    /// symbolic walker checked each descent.
    fn replay_chain(&self, chain: &[DynamicScopeStep], entry: &ScopeContext) -> ChainReplay {
        let mut scope = entry.clone();
        let mut admitted = true;
        let mut admitted_groups = Vec::new();
        for step in chain {
            match step {
                DynamicScopeStep::Transition(transition) => {
                    scope = transition.apply(self.snapshot, &scope);
                }
                DynamicScopeStep::Gate(gate) => {
                    let gate_admits = gate.allowed_scopes.is_empty()
                        || gate.allowed_scopes.iter().any(|expected| {
                            scope.profile.scopes_compatible(&scope.current, expected)
                        });
                    if gate_admits {
                        if let Some(group) = gate.group {
                            admitted_groups.push(group);
                        }
                    } else {
                        admitted = false;
                    }
                }
            }
        }
        ChainReplay {
            scope,
            admitted,
            admitted_groups,
        }
    }

    fn emit_value_site(&mut self, site: &DynamicValueSiteRow, scope: &ScopeContext) {
        let mut raw: Vec<ValueMatcher> = site
            .matchers
            .iter()
            .zip(&site.matcher_scopes)
            .filter(|(_, allowed)| {
                allowed.is_empty()
                    || allowed
                        .iter()
                        .any(|expected| scope.profile.scopes_compatible(&scope.current, expected))
            })
            .map(|(matcher, _)| matcher.clone())
            .collect();
        raw.sort_by_key(|matcher| format!("{matcher:?}"));
        raw.dedup();
        let mut matchers: Vec<ValueMatcher> = raw
            .iter()
            .filter(|matcher| is_completion_constraint(matcher))
            .cloned()
            .collect();
        matchers.sort_by_key(|matcher| format!("{matcher:?}"));
        matchers.dedup();
        if !matchers.is_empty() {
            self.values.push(DynamicValueConstraintSite {
                matchers,
                scope: scope.clone(),
            });
        } else if !raw.is_empty() {
            // Every constraint at this site is a non-enumerable shape
            // (numbers, opaque strings): the parameter is not free-form, but
            // no item can be offered.
            self.unenumerable = true;
        }
    }

    fn emit_affixed_site(&mut self, site: &DynamicAffixedSiteRow, scope: &ScopeContext) {
        let Some((prefix, suffix)) =
            self.render_affixes(&site.prefix_segments, &site.suffix_segments)
        else {
            return;
        };
        let raw: Vec<ValueMatcher> = site
            .matchers
            .iter()
            .zip(&site.matcher_scopes)
            .filter(|(_, allowed)| {
                allowed.is_empty()
                    || allowed
                        .iter()
                        .any(|expected| scope.profile.scopes_compatible(&scope.current, expected))
            })
            .map(|(matcher, _)| matcher.clone())
            .collect();
        match affixed_value_matcher_domain(self.snapshot, &raw, &prefix, &suffix) {
            Some(domain) if !domain.is_empty() => {
                self.values.push(DynamicValueConstraintSite {
                    matchers: domain,
                    scope: scope.clone(),
                });
            }
            // Members exist but none carries both affixes: the site
            // constrains, yet nothing can be offered.
            Some(_) => self.unenumerable = true,
            // Open reference domains cannot constrain the bare argument.
            None => {}
        }
    }

    fn emit_key_render_site(&mut self, site: &DynamicKeyRenderSiteRow, scope: &ScopeContext) {
        let Some((prefix, suffix)) =
            self.render_affixes(&site.prefix_segments, &site.suffix_segments)
        else {
            return;
        };
        if self.key_renders.iter().any(|known| {
            known.prefix.eq_ignore_ascii_case(&prefix)
                && known.suffix.eq_ignore_ascii_case(&suffix)
                && known.context.eq_ignore_ascii_case(&site.context)
                && known.parent_path.len() == site.parent_path.len()
                && known
                    .parent_path
                    .iter()
                    .zip(&site.parent_path)
                    .all(|(left, right)| left.eq_ignore_ascii_case(right))
                && known.scope == *scope
        }) {
            return;
        }
        self.key_renders.push(DynamicKeyRenderSite {
            prefix,
            suffix,
            context: site.context.clone(),
            parent_path: site.parent_path.clone(),
            scope: scope.clone(),
        });
    }

    fn emit_quoted_site(&mut self, site: &DynamicQuotedSiteRow, scope: &ScopeContext) {
        if self.quoted_scripts.iter().any(|known| {
            known.context.eq_ignore_ascii_case(&site.context)
                && known.parent_path == site.parent_path
                && known.scope == *scope
        }) {
            return;
        }
        self.quoted_scripts.push(DynamicQuotedScriptConstraintSite {
            context: site.context.clone(),
            parent_path: site.parent_path.clone(),
            scope: scope.clone(),
        });
    }

    /// Follows one forwarding edge into the callee's rows: the callee enters
    /// at the caller's replayed scope at the call statement (already folded
    /// in pass 1), with parameter bindings translated from the recorded
    /// arguments. Edges whose callee parameter key is itself rendered from a
    /// `$param$` cannot name the target parameter and are not replayed.
    fn emit_forward(&mut self, forward: &DynamicForwardSiteRow, callee_scope: ScopeContext) {
        let Some(callee_parameter) = &forward.parameter else {
            return;
        };
        let identity = (
            forward.kind.to_ascii_lowercase(),
            forward.name.to_ascii_lowercase(),
        );
        if self.visited.contains(&identity) {
            return;
        }
        let Some(callee) = dynamic_rule_row(self.snapshot, &forward.kind, &forward.name) else {
            return;
        };
        self.visited.push(identity);
        let caller_bindings = std::mem::take(&mut self.bindings);
        for binding in &forward.bindings {
            let value = match &binding.value {
                DynamicForwardValue::Opaque => BoundArgument::Unknown,
                DynamicForwardValue::Segments(segments) => {
                    match self.translate_segments(segments) {
                        Some(value) => value,
                        // Unrenderable mixed tokens still bind (presence is
                        // what the callee's guards check).
                        None => BoundArgument::Unknown,
                    }
                }
            };
            self.bindings
                .insert(binding.name.to_ascii_lowercase(), value);
        }
        self.replay_parameter(&callee, callee_parameter, &callee_scope);
        self.bindings = caller_bindings;
        self.visited.pop();
    }

    /// Renders a whole forwarded-argument value: a lone parameter passes its
    /// binding through (the completed parameter stays `Target` one level
    /// down), otherwise every parameter segment must be concretely bound.
    fn translate_segments(&self, segments: &[DynamicAffixSegment]) -> Option<BoundArgument> {
        if let [DynamicAffixSegment::Parameter(name)] = segments {
            return self
                .bindings
                .get(&name.to_ascii_lowercase())
                .cloned()
                .or(Some(BoundArgument::Unknown));
        }
        let mut text = String::new();
        for segment in segments {
            match segment {
                DynamicAffixSegment::Literal(literal) => text.push_str(literal),
                DynamicAffixSegment::Parameter(name) => {
                    match self.bindings.get(&name.to_ascii_lowercase()) {
                        Some(BoundArgument::Concrete(value)) => text.push_str(value),
                        _ => return None,
                    }
                }
            }
        }
        Some(BoundArgument::Concrete(text))
    }

    /// Renders affix segments with the caller's bindings; any parameter
    /// segment that is not concretely bound (including the completed target
    /// itself appearing twice) makes the site unknowable and drops it.
    fn render_affixes(
        &self,
        prefix_segments: &[DynamicAffixSegment],
        suffix_segments: &[DynamicAffixSegment],
    ) -> Option<(String, String)> {
        let render = |segments: &[DynamicAffixSegment]| -> Option<String> {
            let mut text = String::new();
            for segment in segments {
                match segment {
                    DynamicAffixSegment::Literal(literal) => text.push_str(literal),
                    DynamicAffixSegment::Parameter(name) => {
                        match self.bindings.get(&name.to_ascii_lowercase()) {
                            Some(BoundArgument::Concrete(value)) => text.push_str(value),
                            _ => return None,
                        }
                    }
                }
            }
            Some(text)
        };
        Some((render(prefix_segments)?, render(suffix_segments)?))
    }
}

fn is_completion_constraint(matcher: &ValueMatcher) -> bool {
    !matches!(
        matcher,
        ValueMatcher::AnyScalar
            | ValueMatcher::Int { .. }
            | ValueMatcher::Float { .. }
            | ValueMatcher::Date
            | ValueMatcher::DynamicSet(_)
            | ValueMatcher::Filepath
            | ValueMatcher::Opaque(_)
    )
}

/// Caps the stripped-member derivation so an open-ended site cannot turn one
/// parameter into an unbounded candidate sweep.
const MAX_AFFIXED_VALUE_MEMBERS: usize = 512;

/// Derives the bare-argument candidates for a value-position render site
/// (`type = $RT$_rebels`): enumerable matchers contribute their members with
/// the literal affixes stripped, so `catholic_rebels` yields `catholic`.
/// Returns `None` when the site cannot prove a closed value domain — every
/// matcher an open reference type (`name = $X$_loyal` renders a freshly
/// generated modifier name) — leaving the argument unconstrained. Otherwise
/// open-world matchers (numbers, opaque strings) contribute nothing and a
/// member without both affixes cannot have been rendered there and is
/// dropped; the result holds exact matchers only and may be empty (the site
/// constrains but nothing can be offered).
fn affixed_value_matcher_domain(
    snapshot: &AnalysisSnapshot,
    matchers: &[ValueMatcher],
    prefix: &str,
    suffix: &str,
) -> Option<Vec<ValueMatcher>> {
    if !matchers
        .iter()
        .any(|matcher| matches!(matcher, ValueMatcher::Exact(_) | ValueMatcher::Enum(_)))
    {
        return None;
    }
    let mut members: Vec<String> = Vec::new();
    for matcher in matchers {
        match matcher {
            ValueMatcher::Exact(value) => {
                if let Some(stripped) = strip_value_affixes(value, prefix, suffix) {
                    push_unique_member(&mut members, stripped);
                }
            }
            ValueMatcher::Enum(name) => {
                if let Some(static_members) = enum_members(snapshot, name) {
                    for member in static_members {
                        if let Some(stripped) = strip_value_affixes(member, prefix, suffix) {
                            push_unique_member(&mut members, stripped);
                        }
                    }
                }
                // An enum name can double as a workspace kind; those members
                // are accepted by the matcher too and stay derivable.
                for member in workspace_member_index(snapshot, name).select("") {
                    if let Some(stripped) = strip_value_affixes(&member, prefix, suffix) {
                        push_unique_member(&mut members, stripped);
                    }
                }
            }
            ValueMatcher::Type(name) => {
                for member in workspace_member_index(snapshot, name).select("") {
                    if let Some(stripped) = strip_value_affixes(&member, prefix, suffix) {
                        push_unique_member(&mut members, stripped);
                    }
                }
            }
            _ => continue,
        }
        if members.len() > MAX_AFFIXED_VALUE_MEMBERS {
            return None;
        }
    }
    Some(members.into_iter().map(ValueMatcher::Exact).collect())
}

/// Strips one pair of literal affixes from a rendered value, keeping the
/// parameter's own (non-empty) contribution.
fn strip_value_affixes(value: &str, prefix: &str, suffix: &str) -> Option<String> {
    if value.len() <= prefix.len() + suffix.len() {
        return None;
    }
    let suffix_start = value.len() - suffix.len();
    if !value.is_char_boundary(prefix.len()) || !value.is_char_boundary(suffix_start) {
        return None;
    }
    let head = &value[..prefix.len()];
    let middle = &value[prefix.len()..suffix_start];
    let tail = &value[suffix_start..];
    (head.eq_ignore_ascii_case(prefix) && tail.eq_ignore_ascii_case(suffix))
        .then(|| middle.to_owned())
}

fn push_unique_member(members: &mut Vec<String>, member: String) {
    if !members
        .iter()
        .any(|known| known.eq_ignore_ascii_case(&member))
    {
        members.push(member);
    }
}
