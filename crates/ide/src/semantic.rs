use rules::GameProfile;
use std::collections::HashSet;
use std::sync::Arc;

use engine::{
    AnalysisSnapshot, DocumentSource, DynamicDefinitionSummary, DynamicParameterSignature,
    SourceFileId, SourceRootKind,
};
use hir::HirFile;
use text::TextRange;

use crate::support::*;

thread_local! {
    /// Reusable key buffer for snapshot query-cache probes.
    ///
    /// Validation probes the rule-view, dynamic-resolution, and member-name caches per
    /// property and per rule; formatting a fresh owned key for every probe dominated
    /// steady-state allocator traffic. The buffer is borrowed only for the duration of
    /// the cache read, and miss paths run after the borrow ends.
    static QUERY_CACHE_PROBE_KEY: std::cell::RefCell<String> =
        const { std::cell::RefCell::new(String::new()) };
}

/// Probes the snapshot query cache with a key assembled from `parts` without allocating.
pub(crate) fn probe_query_cache<T: Send + Sync + 'static>(
    snapshot: &AnalysisSnapshot,
    revision: u64,
    parts: &[&str],
) -> Option<Arc<T>> {
    QUERY_CACHE_PROBE_KEY.with(|buffer| {
        let mut key = buffer.borrow_mut();
        key.clear();
        for part in parts {
            key.push_str(part);
        }
        snapshot.query_cache().get::<T>(revision, &key)
    })
}

pub(crate) fn dynamic_definition_summary(
    snapshot: &AnalysisSnapshot,
    owner_kind: &str,
    owner_name: &str,
) -> Option<DynamicDefinitionSummary> {
    resolve_dynamic_definition(snapshot, owner_kind, owner_name).map(|resolved| resolved.summary)
}

#[derive(Clone, Debug)]
pub(crate) struct ResolvedDynamicDefinition {
    pub(crate) summary: DynamicDefinitionSummary,
    pub(crate) body_context: String,
}

pub(crate) fn resolve_dynamic_definition(
    snapshot: &AnalysisSnapshot,
    owner_kind: &str,
    owner_name: &str,
) -> Option<ResolvedDynamicDefinition> {
    // The resolution scans every open overlay document, and it is invoked once per
    // (property, rule) during diagnostics, completion, and hover. Memoize per
    // (revision, kind, name) so a revision pays for the scan only once.
    let revision = snapshot.revision();
    if let Some(cached) = probe_query_cache::<Option<ResolvedDynamicDefinition>>(
        snapshot,
        revision,
        &["dynamic-definition:", owner_kind, ":", owner_name],
    ) {
        return cached.as_ref().clone();
    }
    // A worker still observing a superseded definition set must not rebuild:
    // its insert would be dropped, so the next probe would rerun the scan and
    // the resolution loop would burn a core. The worker's own results are
    // destined to be discarded with its revision.
    if snapshot
        .query_cache()
        .is_superseded(engine::CacheDomain::Definitions, revision)
    {
        return None;
    }
    let resolved = resolve_dynamic_definition_uncached(snapshot, owner_kind, owner_name);
    snapshot.query_cache().insert(
        revision,
        engine::CacheDomain::Definitions,
        format!("dynamic-definition:{owner_kind}:{owner_name}"),
        Arc::new(resolved.clone()),
    );
    resolved
}

fn resolve_dynamic_definition_uncached(
    snapshot: &AnalysisSnapshot,
    owner_kind: &str,
    owner_name: &str,
) -> Option<ResolvedDynamicDefinition> {
    let body_context = {
        let ir = snapshot.ir();
        let ty = ir.type_info(ir.type_by_name(owner_kind)?);
        ty.trait_impls
            .iter()
            .filter(|implementation| Some(implementation.trait_id) == ir.trait_by_name("Callable"))
            .find_map(|implementation| {
                implementation
                    .arguments
                    .iter()
                    .find_map(|(name, argument)| {
                        if ir.strings.resolve(*name) != "body" {
                            return None;
                        }
                        match argument {
                            rules::ir::TraitArgument::Text(body) => {
                                Some(ir.strings.resolve(*body).to_owned())
                            }
                            _ => None,
                        }
                    })
            })?
    };
    let mut overlay_candidates = Vec::new();
    for document in snapshot
        .documents()
        .values()
        .filter(|document| document.source() == DocumentSource::Overlay)
    {
        let Some(hir) = document.hir_handle() else {
            continue;
        };
        for definition in hir.definitions().iter().filter(|definition| {
            definition.kind.eq_ignore_ascii_case(owner_kind)
                && definition.name.eq_ignore_ascii_case(owner_name)
        }) {
            overlay_candidates.push(ResolvedDynamicDefinition {
                summary: dynamic_summary_in_hir(
                    &hir,
                    &definition.kind,
                    &definition.name,
                    definition.range,
                ),
                body_context: body_context.clone(),
            });
        }
    }
    if overlay_candidates.len() > 1 {
        return None;
    }
    if let Some(resolved) = overlay_candidates.pop() {
        return Some(resolved);
    }

    let definition = snapshot.index().active_definition(owner_kind, owner_name)?;
    let source_file = snapshot.source_files().get(&definition.file_id)?;
    let hidden_by_overlay = snapshot.documents().values().any(|document| {
        document.source() == DocumentSource::Overlay
            && document
                .path()
                .is_some_and(|path| path == source_file.physical_path.as_path())
    });
    if hidden_by_overlay {
        return None;
    }
    let summary = snapshot
        .index()
        .active_dynamic_definition(owner_kind, owner_name)
        .cloned()?;
    Some(ResolvedDynamicDefinition {
        summary,
        body_context,
    })
}

fn dynamic_summary_in_hir(
    hir: &HirFile,
    kind: &str,
    name: &str,
    owner_range: TextRange,
) -> DynamicDefinitionSummary {
    let parameters = hir
        .parameter_definitions_for_owner(owner_range)
        .map(|definition| DynamicParameterSignature {
            name: definition.name.clone(),
            required: hir.parameter_is_required(owner_range, &definition.name),
        })
        .collect();
    DynamicDefinitionSummary {
        kind: Arc::from(kind),
        name: name.to_owned(),
        definition_range: owner_range,
        parameters,
        template: hir.dynamic_template(kind, name, owner_range).cloned(),
    }
}

/// Builds the canonical invocation snippet for a resolved dynamic definition signature. Snippet bodies
/// use relative indentation only; the client re-indents multi-line snippets to the insertion line.
///
/// A definition is scalar (`= yes` for effects, a boolean for triggers) if and
/// only if its body declares no `$PARAM$` at all; every parameterized
/// definition completes as a parameter block with one tabstop per
/// effectively-required parameter, the last of which doubles as the final
/// cursor position. An invocation block only accepts parameter assignments,
/// so the skeleton carries no trailing placeholder line.
pub(crate) fn scripted_definition_snippet(
    snapshot: &AnalysisSnapshot,
    kind_name: &str,
    definition_name: &str,
) -> String {
    let Some(summary) = dynamic_definition_summary(snapshot, kind_name, definition_name) else {
        return format!("{definition_name} = {{\n\t$0\n}}");
    };
    if summary.parameters.is_empty() {
        return format!("{definition_name} = yes");
    }
    let tabstops = summary
        .parameters
        .iter()
        .filter(|parameter| {
            crate::dynamic_rules::parameter_effectively_required(snapshot, &summary, parameter)
        })
        .map(|parameter| parameter.name.as_str())
        .collect::<Vec<_>>();
    if tabstops.is_empty() {
        return format!("{definition_name} = {{\n\t$0\n}}");
    }
    let inner_indent = "\t";
    let last = tabstops.len() - 1;
    let mut body = String::new();
    for (index, name) in tabstops.iter().enumerate() {
        // Tabstop numbering runs over the prefilled parameters only; the
        // final one doubles as the cursor's resting position.
        let stop = if index == last {
            "$0".to_owned()
        } else {
            format!("${}", index + 1)
        };
        body.push_str(&format!("{inner_indent}{name} = {stop}\n"));
    }
    format!("{definition_name} = {{\n{body}}}")
}

/// Field-level value semantics of one construct context, as declared by the
/// semantic rules: which exact-named leaf fields hold localisation keys and
/// which hold sprite names.
///
/// This is the query hover cards consume instead of hardcoding field names, so
/// presentation and validation can never drift apart: a field resolves in a
/// card exactly when the rule set types it there.
pub(crate) struct ConstructFieldSemantics {
    /// Exact-named leaf fields whose values are localisation keys.
    pub localisation_fields: Vec<String>,
    /// Exact-named leaf fields whose values are sprite names.
    pub sprite_fields: Vec<String>,
}

fn workspace_member_kinds(snapshot: &AnalysisSnapshot, type_name: &str) -> Vec<String> {
    let base = type_name
        .split_once('.')
        .map_or(type_name, |(kind, _)| kind);
    let mut kinds = vec![type_name.to_owned(), base.to_owned()];
    if let Some(alias) = snapshot.game_profile().member_kind_alias(base) {
        kinds.push(alias.to_owned());
    }
    kinds.sort_by_key(|kind| kind.to_ascii_lowercase());
    kinds.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
    kinds
}

/// Returns members visible at this snapshot, with open overlays replacing their backing files.
/// Dynamic definition names intentionally come from definitions, not from a static Vanilla name list.
pub(crate) fn effective_workspace_member_names(
    snapshot: &AnalysisSnapshot,
    type_name: &str,
) -> Vec<String> {
    workspace_member_names_cached(snapshot, type_name)
        .as_ref()
        .clone()
}

/// Returns the workspace member names for one type as a shared snapshot-owned vector.
///
/// The source index and live overlays are immutable for a snapshot revision.  Keeping this
/// normalized name vector in the snapshot query cache avoids rewalking every definition for each
/// completion request and gives the prefix index below one stable backing allocation.
fn workspace_member_names_cached(snapshot: &AnalysisSnapshot, type_name: &str) -> Arc<Vec<String>> {
    let revision = snapshot.revision();
    let lowered = lowered_type_name(type_name);
    if let Some(cached) = probe_query_cache::<Vec<String>>(
        snapshot,
        revision,
        &["workspace-member-names:", lowered.as_ref()],
    ) {
        return cached;
    }
    let names = Arc::new(effective_workspace_member_names_uncached(
        snapshot, type_name,
    ));
    snapshot.query_cache().insert(
        revision,
        engine::CacheDomain::Documents,
        format!("workspace-member-names:{}", lowered.as_ref()),
        Arc::clone(&names),
    );
    names
}

/// Lowercased spelling of a type name, borrowing when it is already lowercase.
fn lowered_type_name(type_name: &str) -> std::borrow::Cow<'_, str> {
    if type_name.bytes().any(|byte| byte.is_ascii_uppercase()) {
        std::borrow::Cow::Owned(type_name.to_ascii_lowercase())
    } else {
        std::borrow::Cow::Borrowed(type_name)
    }
}

fn effective_workspace_member_names_uncached(
    snapshot: &AnalysisSnapshot,
    type_name: &str,
) -> Vec<String> {
    let hidden_files = overlay_file_ids(snapshot);
    let kinds = workspace_member_kinds(snapshot, type_name);
    let mut names = Vec::new();
    for kind in &kinds {
        names.extend(
            snapshot
                .index()
                .definitions_for_kind_with_state(kind)
                .filter(|(definition, active)| {
                    *active
                        && !hidden_files.contains(&definition.file_id)
                        && completion_source_file_allowed(snapshot, definition.file_id)
                })
                .map(|(definition, _active)| definition.name.to_string()),
        );
    }
    for document in snapshot
        .documents()
        .values()
        .filter(|document| document.source() == DocumentSource::Overlay)
    {
        if !completion_overlay_allowed(snapshot) {
            break;
        }
        let Some(hir) = document.hir_handle() else {
            continue;
        };
        names.extend(
            hir.definitions()
                .iter()
                .filter(|definition| {
                    kinds
                        .iter()
                        .any(|kind| definition.kind.eq_ignore_ascii_case(kind))
                })
                .map(|definition| definition.name.to_string()),
        );
    }
    names.sort_by_key(|name| name.to_ascii_lowercase());
    names.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
    names
}

/// Compact iteration index for the localisation namespace.
///
/// Localisation keys are commonly the largest symbol set in an EU4 workspace (Vanilla plus a
/// mod can contain hundreds of thousands of entries).  Keeping the labels in one contiguous
/// blob avoids the pointer chasing of a `Vec<String>` during did-you-mean scans.  The
/// index is deliberately query-only: labels remain owned by the snapshot's ordinary workspace
/// index, and a corrupt/missing cache can always rebuild this derived view.
pub(crate) struct LocalisationKeyIndex {
    blob: String,
    offsets: Vec<usize>,
    length_order: Vec<(usize, usize)>,
}

impl LocalisationKeyIndex {
    fn new(names: &[String]) -> Self {
        let mut sorted = names.iter().map(String::as_str).collect::<Vec<_>>();
        sorted.sort_by(|left, right| {
            left.to_ascii_lowercase()
                .cmp(&right.to_ascii_lowercase())
                .then_with(|| left.cmp(right))
        });
        sorted.dedup_by(|left, right| left.eq_ignore_ascii_case(right));

        let capacity = sorted.iter().map(|name| name.len()).sum();
        let mut blob = String::with_capacity(capacity);
        let mut offsets = Vec::with_capacity(sorted.len() + 1);
        let mut length_order = Vec::with_capacity(sorted.len());
        for (index, name) in sorted.into_iter().enumerate() {
            offsets.push(blob.len());
            blob.push_str(name);
            length_order.push((name.chars().count(), index));
        }
        offsets.push(blob.len());
        length_order.sort_unstable();
        Self {
            blob,
            offsets,
            length_order,
        }
    }

    /// Visits only character lengths that can have the requested edit distance.
    /// Labels remain unique under ASCII case folding, so length order cannot
    /// change the unique minimum selected by the suggestion query.
    pub(crate) fn candidates(&self, name: &str, max: usize) -> impl Iterator<Item = &str> {
        let length = name.chars().count();
        let first = self
            .length_order
            .partition_point(|(len, _)| *len < length.saturating_sub(max));
        let last = self
            .length_order
            .partition_point(|(len, _)| *len <= length.saturating_add(max));
        self.length_order[first..last]
            .iter()
            .map(|(_, index)| self.key(*index))
    }

    fn key(&self, index: usize) -> &str {
        &self.blob[self.offsets[index]..self.offsets[index + 1]]
    }
}

/// Returns the compact localisation-key index for this immutable snapshot revision.
pub(crate) fn localisation_key_index(snapshot: &AnalysisSnapshot) -> Arc<LocalisationKeyIndex> {
    let revision = snapshot.revision();
    let key = "localisation-key-index";
    if let Some(cached) = snapshot
        .query_cache()
        .get::<LocalisationKeyIndex>(revision, key)
    {
        return cached;
    }
    let names = workspace_member_names_cached(snapshot, "localisation");
    let index = Arc::new(LocalisationKeyIndex::new(names.as_ref()));
    snapshot.query_cache().insert(
        revision,
        engine::CacheDomain::Documents,
        key.to_owned(),
        Arc::clone(&index),
    );
    index
}

pub(crate) fn dynamic_definition_type(snapshot: &AnalysisSnapshot, type_name: &str) -> bool {
    let ir = snapshot.ir();
    ir.type_by_name(type_name).is_some_and(|id| {
        ir.type_info(id)
            .trait_impls
            .iter()
            .any(|implementation| Some(implementation.trait_id) == ir.trait_by_name("Callable"))
    })
}

/// Per-revision view of every definition identity visible to membership queries.
///
/// `workspace_member` is called once per (rule, property) pair — hundreds of times per
/// document. The previous per-call memo key (`format!` + two lowercase strings + a global
/// cache probe per query) cost more than the membership check itself. The index-derived
/// name counts live in the Index cache domain; definitions hidden by open overlays are
/// tracked separately in the Documents domain so overlay edits stay correct without
/// rebuilding the workspace-wide set on every keystroke.
pub(crate) struct WorkspaceMembership {
    /// Definition kind spelling as stored by the workspace index.
    kinds: rustc_hash::FxHashMap<Box<str>, rustc_hash::FxHashMap<Box<str>, u32>>,
    /// Lowercased kind -> name suffixes probed in addition to the raw member spelling.
    suffixes: rustc_hash::FxHashMap<Box<str>, Vec<Box<str>>>,
    /// Prebuilt per-type-name views for every name the rule set can query.
    views: rustc_hash::FxHashMap<Box<str>, Arc<MemberKindView>>,
}

/// Per-type-name resolution of the kind spellings and name suffixes probed by
/// [`WorkspaceMembership::contains`].
///
/// Hot-path type names come from the rule set (key/value matchers and `<name>`
/// parent-path entries) plus a handful of literals, so every membership check
/// resolves through a prebuilt view instead of re-running the kind-alias scan,
/// case folding, and suffix formatting per property. Resolution inputs are
/// profile-static, so views are exact per revision and only need rebuilding
/// with the rule set.
struct MemberKindView {
    /// Kind spellings exactly as `WorkspaceMembership::kinds` keys store them.
    kinds: Arc<[Box<str>]>,
    /// The same kinds lowercased, for the overlay-positive check.
    kinds_lower: Arc<[Box<str>]>,
    /// Name suffixes probed as `{member}{suffix}` against every kind.
    suffixes: Arc<[Box<str>]>,
}

impl MemberKindView {
    fn resolve(
        profile: &GameProfile,
        suffixes: &rustc_hash::FxHashMap<Box<str>, Vec<Box<str>>>,
        type_name: &str,
    ) -> Self {
        let base = type_name
            .split_once('.')
            .map_or(type_name, |(kind, _)| kind);
        let mut kinds: Vec<Box<str>> = vec![Box::from(type_name)];
        if base != type_name {
            kinds.push(Box::from(base));
        }
        if let Some(alias) = profile.member_kind_alias(base)
            && !kinds.iter().any(|kind| kind.eq_ignore_ascii_case(alias))
        {
            kinds.push(Box::from(alias));
        }
        let mut resolved_suffixes: Vec<Box<str>> = Vec::new();
        for kind in &kinds {
            let kind_lower = kind.to_ascii_lowercase();
            if let Some(list) = suffixes.get(kind_lower.as_str()) {
                resolved_suffixes.extend(list.iter().cloned());
            }
        }
        let kinds_lower = kinds
            .iter()
            .map(|kind| Box::from(kind.to_ascii_lowercase().as_str()))
            .collect();
        Self {
            kinds: kinds.into(),
            kinds_lower,
            suffixes: resolved_suffixes.into(),
        }
    }
}

/// `(kind, folded name)` definition counts hidden by open overlays, per document revision.
struct OverlayHiddenCounts(rustc_hash::FxHashMap<Box<str>, rustc_hash::FxHashMap<Box<str>, u32>>);

/// Thread-local fast path for one snapshot-derived view.
///
/// The key is `(host identity, revision)`. Revision numbers are only unique
/// within a single [`AnalysisHost`]: without the host id, a second host on the
/// same thread (test harnesses, vanilla or dependency scans) that reaches the
/// same revision number would reuse this slot and observe the first host's
/// view — stale membership data reports live definitions as absent.
struct SnapshotFastPath<T> {
    entry: std::cell::RefCell<Option<((u64, u64), T)>>,
}

impl<T> SnapshotFastPath<T> {
    const fn new() -> Self {
        Self {
            entry: std::cell::RefCell::new(None),
        }
    }

    /// Returns a clone of the cached value when it was stored for this exact
    /// snapshot (same host, same revision).
    fn get_cloned(&self, snapshot: &AnalysisSnapshot, clone: impl FnOnce(&T) -> T) -> Option<T> {
        let key = (snapshot.host_identity(), snapshot.revision());
        self.entry
            .borrow()
            .as_ref()
            .filter(|(seen, _)| *seen == key)
            .map(|(_, value)| clone(value))
    }

    /// Runs `f` against the entry stored for this exact snapshot, installing
    /// `init` first when the slot belongs to another host or revision.
    fn with_entry<R>(
        &self,
        snapshot: &AnalysisSnapshot,
        init: impl FnOnce() -> T,
        f: impl FnOnce(&mut T) -> R,
    ) -> R {
        let key = (snapshot.host_identity(), snapshot.revision());
        let mut entry = self.entry.borrow_mut();
        if entry.as_ref().is_none_or(|(seen, _)| *seen != key) {
            *entry = Some((key, init()));
        }
        let (_, value) = entry.as_mut().expect("entry installed above");
        f(value)
    }

    /// Stores `value` for this snapshot, replacing an entry from any other
    /// host or revision.
    fn set(&self, snapshot: &AnalysisSnapshot, value: T) {
        let key = (snapshot.host_identity(), snapshot.revision());
        *self.entry.borrow_mut() = Some((key, value));
    }
}

fn overlay_hidden_counts(snapshot: &AnalysisSnapshot) -> Arc<OverlayHiddenCounts> {
    thread_local! {
        static SLOT: SnapshotFastPath<Arc<OverlayHiddenCounts>> = const { SnapshotFastPath::new() };
    }
    let revision = snapshot.revision();
    if let Some(cached) = SLOT.with(|slot| slot.get_cloned(snapshot, Arc::clone)) {
        return cached;
    }
    let cache_key = "workspace-membership-hidden";
    if let Some(cached) = snapshot
        .query_cache()
        .get::<OverlayHiddenCounts>(revision, cache_key)
    {
        SLOT.with(|slot| slot.set(snapshot, Arc::clone(&cached)));
        return cached;
    }
    let mut counts = OverlayHiddenCounts(rustc_hash::FxHashMap::default());
    for file_id in overlay_file_ids(snapshot) {
        for (definition, active) in snapshot.index().definitions_for_file_with_state(file_id) {
            if !active || !completion_source_file_allowed(snapshot, file_id) {
                continue;
            }
            let folded = snapshot.index().definition_name_key(&definition.name);
            counts
                .0
                .entry(Box::from(definition.kind.as_ref()))
                .or_default()
                .entry(Box::from(folded.as_ref()))
                .and_modify(|count| *count += 1)
                .or_insert(1);
        }
    }
    let counts = Arc::new(counts);
    snapshot.query_cache().insert(
        revision,
        engine::CacheDomain::Documents,
        cache_key.to_owned(),
        counts.clone(),
    );
    counts
}

impl WorkspaceMembership {
    /// Whether one `(kind, name)` pair has a definition not hidden by an overlay.
    fn indexed(&self, hidden: &OverlayHiddenCounts, kind: &str, folded: &str) -> bool {
        let Some(total) = self
            .kinds
            .get::<str>(kind)
            .and_then(|names| names.get::<str>(folded))
        else {
            return false;
        };
        let hidden_count = hidden
            .0
            .get::<str>(kind)
            .and_then(|names| names.get::<str>(folded))
            .copied()
            .unwrap_or(0);
        *total > hidden_count
    }

    fn contains(
        &self,
        snapshot: &AnalysisSnapshot,
        hidden: &OverlayHiddenCounts,
        members: &OverlayMembers,
        type_name: &str,
        member: &str,
    ) -> bool {
        // Rule-derived type names resolve through prebuilt views; arbitrary kinds from
        // resolution queries fall back to the derivation below.
        let Some(view) = self.views.get::<str>(type_name) else {
            return self.contains_slow(snapshot, hidden, members, type_name, member);
        };
        let index = snapshot.index();
        for kind in view.kinds.iter() {
            let folded = index.definition_name_key(member);
            if self.indexed(hidden, kind, folded.as_ref()) {
                return true;
            }
        }
        for suffix in view.suffixes.iter() {
            let candidate = format!("{member}{suffix}");
            for kind in view.kinds.iter() {
                let folded = index.definition_name_key(&candidate);
                if self.indexed(hidden, kind, folded.as_ref()) {
                    return true;
                }
            }
        }
        if !completion_overlay_allowed(snapshot) {
            return false;
        }
        if members.names.is_empty() {
            return false;
        }
        let mut candidates = vec![member.to_ascii_lowercase()];
        candidates.extend(
            view.suffixes
                .iter()
                .map(|suffix| format!("{member}{suffix}").to_ascii_lowercase()),
        );
        candidates.iter().any(|name| {
            view.kinds_lower
                .iter()
                .any(|kind| members.names.contains(&(kind.to_string(), name.clone())))
        })
    }

    fn contains_slow(
        &self,
        snapshot: &AnalysisSnapshot,
        hidden: &OverlayHiddenCounts,
        members: &OverlayMembers,
        type_name: &str,
        member: &str,
    ) -> bool {
        let profile = snapshot.game_profile();
        let base = type_name
            .split_once('.')
            .map_or(type_name, |(kind, _)| kind);
        // The kind set matches `workspace_member_kinds` up to case-insensitive dedup; the
        // boolean result is order-insensitive, so no sorted materialisation is needed.
        let mut kinds: Vec<&str> = vec![type_name];
        if base != type_name {
            kinds.push(base);
        }
        if let Some(alias) = profile.member_kind_alias(base)
            && !kinds.iter().any(|kind| kind.eq_ignore_ascii_case(alias))
        {
            kinds.push(alias);
        }
        let mut names = vec![member.to_owned()];
        for kind in &kinds {
            let kind_lower = kind.to_ascii_lowercase();
            if let Some(suffixes) = self.suffixes.get(kind_lower.as_str()) {
                for suffix in suffixes {
                    names.push(format!("{member}{suffix}"));
                }
            }
        }
        if names.iter().any(|name| {
            kinds.iter().any(|kind| {
                let folded = snapshot.index().definition_name_key(name);
                self.indexed(hidden, kind, folded.as_ref())
            })
        }) {
            return true;
        }
        if !completion_overlay_allowed(snapshot) {
            return false;
        }
        if members.names.is_empty() {
            return false;
        }
        names.iter().any(|name| {
            kinds.iter().any(|kind| {
                members
                    .names
                    .contains(&(kind.to_ascii_lowercase(), name.to_ascii_lowercase()))
            })
        })
    }
}

fn membership_view_type_names(ir: &rules::ir::RulesIr) -> Vec<String> {
    ir.types
        .iter()
        .map(|info| ir.strings().resolve(info.name).to_owned())
        .collect()
}

/// Returns the shared membership view for this snapshot revision.
///
/// Validation probes membership per (rule, property); the snapshot query cache's read
/// lock and type-erased downcast dominated that path once 13 workers shared it. Each
/// worker thread memoizes the `Arc` for the revision it is validating — the underlying
/// map stays a single shared allocation, and a revision change invalidates the slot.
fn workspace_membership(snapshot: &AnalysisSnapshot) -> Arc<WorkspaceMembership> {
    thread_local! {
        static SLOT: SnapshotFastPath<Arc<WorkspaceMembership>> = const { SnapshotFastPath::new() };
    }
    let revision = snapshot.revision();
    if let Some(cached) = SLOT.with(|slot| slot.get_cloned(snapshot, Arc::clone)) {
        return cached;
    }
    let cache_key = "workspace-membership";
    if let Some(cached) = snapshot
        .query_cache()
        .get::<WorkspaceMembership>(revision, cache_key)
    {
        return cached;
    }
    let mut kinds: rustc_hash::FxHashMap<Box<str>, rustc_hash::FxHashMap<Box<str>, u32>> =
        rustc_hash::FxHashMap::default();
    for (definition, active) in snapshot.index().definition_identities() {
        if !active || !completion_source_file_allowed(snapshot, definition.file_id) {
            continue;
        }
        let folded = snapshot.index().definition_name_key(&definition.name);
        *kinds
            .entry(Box::from(definition.kind.as_ref()))
            .or_default()
            .entry(Box::from(folded.as_ref()))
            .or_insert(0) += 1;
    }
    let mut suffixes: rustc_hash::FxHashMap<Box<str>, Vec<Box<str>>> =
        rustc_hash::FxHashMap::default();
    for rule in &snapshot.game_profile().member_name_suffixes {
        for kind in &rule.kinds {
            suffixes
                .entry(Box::from(kind.to_ascii_lowercase().as_str()))
                .or_default()
                .push(Box::from(rule.suffix.as_str()));
        }
    }
    let views = membership_view_type_names(snapshot.ir())
        .into_iter()
        .map(|type_name| {
            let view = MemberKindView::resolve(snapshot.game_profile(), &suffixes, &type_name);
            (Box::from(type_name.as_str()), Arc::new(view))
        })
        .collect();
    let membership = Arc::new(WorkspaceMembership {
        kinds,
        suffixes,
        views,
    });
    snapshot.query_cache().insert(
        revision,
        engine::CacheDomain::Index,
        cache_key.to_owned(),
        Arc::clone(&membership),
    );
    membership
}

/// The three membership views one `workspace_member` check needs, fetched together.
///
/// Validation calls `workspace_member` per (rule, property); fetching all views through
/// one revision-keyed thread-local slot keeps the check to a single borrow plus `Arc`
/// clones, with no query-cache lock on the steady-state path. The underlying views stay
/// single shared allocations stored in the snapshot query cache.
struct MembershipBundle {
    membership: Arc<WorkspaceMembership>,
    hidden: Arc<OverlayHiddenCounts>,
    members: Arc<OverlayMembers>,
}

fn membership_bundle(snapshot: &AnalysisSnapshot) -> Arc<MembershipBundle> {
    thread_local! {
        static SLOT: SnapshotFastPath<Arc<MembershipBundle>> = const { SnapshotFastPath::new() };
    }
    if let Some(cached) = SLOT.with(|slot| slot.get_cloned(snapshot, Arc::clone)) {
        return cached;
    }
    let bundle = Arc::new(MembershipBundle {
        membership: workspace_membership(snapshot),
        hidden: overlay_hidden_counts(snapshot),
        members: overlay_members(snapshot),
    });
    SLOT.with(|slot| slot.set(snapshot, Arc::clone(&bundle)));
    bundle
}

/// Upper bound on memoized workspace-membership probes per thread. The memo
/// only trades memory for repeat-probe speed, so clearing it when full loses
/// no correctness — the next probe of a cleared pair recomputes.
const WORKSPACE_MEMBER_MEMO_CAP: usize = 1 << 16;

/// type-name -> (lowercase member -> membership answer) for the memo below.
type WorkspaceMemberMemo = rustc_hash::FxHashMap<Box<str>, rustc_hash::FxHashMap<Box<str>, bool>>;

thread_local! {
    /// Repeat-probe memo for [`workspace_member`], keyed by host identity and
    /// snapshot revision. Rule matching probes the same `(type, member)` pairs
    /// for every property (dynamic keys, scope fields, variable names), and
    /// answers depend only on the immutable snapshot, so one computation per
    /// distinct pair per revision is enough. Nested maps keep every probe
    /// allocation-free for already-lowercase members.
    static WORKSPACE_MEMBER_MEMO: SnapshotFastPath<WorkspaceMemberMemo> =
        const { SnapshotFastPath::new() };
}

pub(crate) fn workspace_member(snapshot: &AnalysisSnapshot, type_name: &str, member: &str) -> bool {
    let lowered = if member.bytes().any(|byte| byte.is_ascii_uppercase()) {
        std::borrow::Cow::Owned(member.to_ascii_lowercase())
    } else {
        std::borrow::Cow::Borrowed(member)
    };
    let memoized = WORKSPACE_MEMBER_MEMO.with(|slot| {
        slot.with_entry(snapshot, rustc_hash::FxHashMap::default, |memo| {
            memo.get::<str>(type_name)
                .and_then(|members| members.get::<str>(lowered.as_ref()))
                .copied()
        })
    });
    if let Some(answer) = memoized {
        return answer;
    }
    let bundle = membership_bundle(snapshot);
    let answer =
        bundle
            .membership
            .contains(snapshot, &bundle.hidden, &bundle.members, type_name, member);
    WORKSPACE_MEMBER_MEMO.with(|slot| {
        slot.with_entry(snapshot, rustc_hash::FxHashMap::default, |memo| {
            let members = memo.entry(Box::from(type_name)).or_default();
            if members.len() < WORKSPACE_MEMBER_MEMO_CAP {
                members.insert(Box::from(lowered.as_ref()), answer);
            }
        })
    });
    answer
}

/// Returns whether an indexed definition's source layer is enabled for completion members.
/// Resolution, diagnostics, navigation, and hover intentionally continue to see every layer;
/// this preference only narrows the candidate list offered while typing.
pub(crate) fn completion_source_file_allowed(
    snapshot: &AnalysisSnapshot,
    file_id: SourceFileId,
) -> bool {
    let Some(file) = snapshot.source_files().get(&file_id) else {
        return false;
    };
    let Some(root) = snapshot
        .source_roots()
        .iter()
        .find(|root| root.id == file.root_id)
    else {
        return false;
    };
    snapshot.completion_source_layer_enabled(root.kind)
}

/// Open overlays are always owned by the Project layer for completion filtering.
pub(crate) fn completion_overlay_allowed(snapshot: &AnalysisSnapshot) -> bool {
    snapshot.completion_source_layer_enabled(SourceRootKind::Project)
}

pub(crate) fn workspace_kind_has_members(snapshot: &AnalysisSnapshot, type_name: &str) -> bool {
    let kinds = workspace_member_kinds(snapshot, type_name);
    kinds
        .iter()
        .any(|kind| !workspace_member_names_cached(snapshot, kind).is_empty())
}

/// Lowercased overlay definition identity, built once per snapshot revision and shared by every
/// workspace-membership check in that revision.
#[derive(Default)]
pub(crate) struct OverlayMembers {
    /// Lowercased `(kind, name)` pairs of every overlay definition.
    pub(crate) names: HashSet<(String, String)>,
    /// Lowercased kinds present in any overlay definition.
    pub(crate) kinds: HashSet<String>,
}

/// Returns the overlay definition view for this revision, computing it at most once.
///
/// Like the membership view, each worker thread memoizes the shared `Arc` for the
/// revision it observes so membership checks skip the query-cache lock entirely.
pub(crate) fn overlay_members(snapshot: &AnalysisSnapshot) -> Arc<OverlayMembers> {
    thread_local! {
        static SLOT: SnapshotFastPath<Arc<OverlayMembers>> = const { SnapshotFastPath::new() };
    }
    let revision = snapshot.revision();
    if let Some(cached) = SLOT.with(|slot| slot.get_cloned(snapshot, Arc::clone)) {
        return cached;
    }
    let key = "overlay-members";
    if let Some(cached) = snapshot.query_cache().get::<OverlayMembers>(revision, key) {
        SLOT.with(|slot| slot.set(snapshot, Arc::clone(&cached)));
        return cached;
    }
    let mut members = OverlayMembers::default();
    for document in snapshot
        .documents()
        .values()
        .filter(|document| document.source() == DocumentSource::Overlay)
        .filter_map(|document| document.hir_handle())
    {
        for definition in document.definitions() {
            let kind = definition.kind.to_ascii_lowercase();
            members.kinds.insert(kind.clone());
            members
                .names
                .insert((kind, definition.name.to_ascii_lowercase()));
        }
    }
    let members = Arc::new(members);
    snapshot.query_cache().insert(
        revision,
        engine::CacheDomain::Documents,
        key.to_owned(),
        Arc::clone(&members),
    );
    members
}

#[cfg(test)]
mod tests {
    use super::LocalisationKeyIndex;

    #[test]
    fn localisation_length_candidates_preserve_unique_suggestions() {
        let names = [
            "Name",
            "name",
            "country_flag",
            "count",
            "cat",
            "bat",
            "éab",
            "Éab",
            "国ab",
            "é",
            "a_much_longer_key",
        ]
        .map(str::to_owned);
        let index = LocalisationKeyIndex::new(&names);
        for query in [
            "NAME",
            "naem",
            "cont",
            "rat",
            "éab",
            "国ab",
            "Éabc",
            "country_falg",
            "missing",
        ] {
            assert_eq!(
                crate::suggest::best_suggestion(query, index.candidates(query, 2)),
                crate::suggest::best_suggestion(
                    query,
                    (0..index.offsets.len() - 1).map(|id| index.key(id))
                ),
                "{query}"
            );
            assert!(
                index.candidates(query, 2).all(|name| name
                    .chars()
                    .count()
                    .abs_diff(query.chars().count())
                    <= 2)
            );
        }
    }
}
