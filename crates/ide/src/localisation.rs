//! Scripted-localisation indexing and localisation lookup queries.
//!
//! CWTools treats scripted localisation as a path-driven namespace: definitions are read from
//! `name = ...` fields below a scripted-localisation directory, even when the active ruleset does
//! not describe that directory as a typed definition.  The engine lowers those facts into the
//! ordinary `defined_text` symbol family; this module adds the snapshot query and the editor
//! behaviour that consumes it.

use std::collections::BTreeSet;
use std::sync::Arc;

use engine::{AnalysisSnapshot, DocumentSource, SourceFileId};
use parser::{CstKind, CstNode, FileFormat};
use text::{TextRange, TextSize};

use crate::resolution::{
    ResolutionDefinition, effective_localisation_candidate, localisation_language,
    semantic_data_with_cancellation, symbol_candidates_for_hover, text_range_within,
};
use crate::support::{
    ParsedContent, ParsedInput, input_for_document, input_for_source_file,
    truncate_localisation_preview,
};
use crate::types::{CancellationToken, Cancelled, Location};

const SCRIPTED_LOCALISATION_NAMES_CACHE_KEY: &str = "scripted-localisation-names";

/// Returns the scripted-localisation names visible in an immutable snapshot.
#[must_use]
pub fn scripted_localisation_names(snapshot: &AnalysisSnapshot) -> Vec<String> {
    crate::types::uncancelled(scripted_localisation_names_with_cancellation(
        snapshot,
        &CancellationToken::new(),
    ))
}

/// Returns scripted-localisation names while observing cooperative cancellation.
///
/// Names are sourced from active indexed `defined_text` definitions whose file path contains a
/// profile-declared scripted-localisation directory.  Open overlays hide their backing shard and
/// contribute their own HIR definitions, so the result follows the same precedence view as other
/// workspace queries.  A completed result is cached by snapshot revision.
pub fn scripted_localisation_names_with_cancellation(
    snapshot: &AnalysisSnapshot,
    cancellation: &CancellationToken,
) -> Result<Vec<String>, Cancelled> {
    scripted_localisation_names_cached_with_cancellation(snapshot, cancellation)
        .map(|names| names.as_ref().clone())
}

fn scripted_localisation_names_cached_with_cancellation(
    snapshot: &AnalysisSnapshot,
    cancellation: &CancellationToken,
) -> Result<Arc<Vec<String>>, Cancelled> {
    cancellation.checkpoint()?;
    let revision = snapshot.revision();
    if let Some(cached) = snapshot
        .query_cache()
        .get::<Vec<String>>(revision, SCRIPTED_LOCALISATION_NAMES_CACHE_KEY)
    {
        return Ok(cached);
    }

    let hidden_files = crate::support::overlay_file_ids(snapshot);
    let profile = snapshot.game_profile();
    let mut names = Vec::new();
    // Partition by the profile path before touching the definition buckets.  A large Vanilla
    // index can contain hundreds of thousands of `defined_text` records, while scripted
    // localisation normally occupies only a small set of dedicated files.  Walking those
    // shards directly mirrors CWTools' per-file TypeIndex and keeps a cold query proportional to
    // the relevant files rather than the entire symbol table.
    for (file_index, file) in snapshot.source_files().values().enumerate() {
        if file_index & 31 == 0 {
            cancellation.checkpoint()?;
        }
        if hidden_files.contains(&file.id)
            || !profile.is_scripted_localisation_path(file.logical_path.as_str())
        {
            continue;
        }
        for (definition_index, (definition, active)) in snapshot
            .index()
            .definitions_for_file_with_state(file.id)
            .enumerate()
        {
            if definition_index & 255 == 0 {
                cancellation.checkpoint()?;
            }
            if active && definition.kind.eq_ignore_ascii_case("defined_text") {
                names.push(definition.name.to_string());
            }
        }
    }

    for (index, document) in snapshot.documents().values().enumerate() {
        if index & 31 == 0 {
            cancellation.checkpoint()?;
        }
        if document.source() != DocumentSource::Overlay {
            continue;
        }
        let Some(input) = input_for_document(snapshot, document.id()) else {
            continue;
        };
        let Some(path) = input.path.as_ref() else {
            continue;
        };
        if !profile.is_scripted_localisation_path(path.as_str()) {
            continue;
        }
        let Some(hir) = input.hir.as_deref() else {
            continue;
        };
        for (definition_index, definition) in hir.definitions().iter().enumerate() {
            if definition_index & 255 == 0 {
                cancellation.checkpoint()?;
            }
            if definition.kind.eq_ignore_ascii_case("defined_text") {
                names.push(definition.name.to_string());
            }
        }
    }

    names.sort_by(|left, right| {
        left.to_ascii_lowercase()
            .cmp(&right.to_ascii_lowercase())
            .then_with(|| left.cmp(right))
    });
    names.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
    let names = Arc::new(names);
    snapshot.query_cache().insert(
        revision,
        engine::CacheDomain::Documents,
        SCRIPTED_LOCALISATION_NAMES_CACHE_KEY.to_owned(),
        Arc::clone(&names),
    );
    Ok(names)
}

/// How a localisation key filter is matched against definition names.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum LocalisationKeyMatch {
    /// Case-insensitive substring (legacy behaviour).
    #[default]
    Substring,
    /// Case-insensitive equality; a key lookup returns zero or one hit and never truncates.
    Exact,
    /// Case-insensitive anchored prefix; enumerates one dotted key family.
    Prefix,
}

impl LocalisationKeyMatch {
    fn matches(self, name: &str, query: &str) -> bool {
        let name = name.to_ascii_lowercase();
        let query = query.to_ascii_lowercase();
        match self {
            LocalisationKeyMatch::Substring => name.contains(query.as_str()),
            LocalisationKeyMatch::Exact => name == query,
            LocalisationKeyMatch::Prefix => name.starts_with(query.as_str()),
        }
    }
}

/// One localisation definition site that matched a search query.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalisationSearchHit {
    /// The localisation key (`name: "value"`'s name).
    pub key: String,
    /// The most recent language header preceding the entry, when one was present.
    pub language: Option<String>,
    /// The bounded decoded preview value of this definition site, when retained.
    pub value: Option<String>,
    /// File containing the definition.
    pub file_id: SourceFileId,
    /// Range of the whole localisation entry in the file.
    pub range: TextRange,
}

/// A bounded localisation search result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalisationSearchResult {
    /// Matched definition sites, in deterministic index order.
    pub hits: Vec<LocalisationSearchHit>,
    /// Whether further matches existed beyond the requested limit.
    pub truncated: bool,
}

/// Searches indexed localisation definitions by key and/or value substring.
///
/// The search walks the persisted workspace index (Vanilla, dependency, and project files on
/// disk); open-but-unsaved editor overlays are outside this view, mirroring the on-disk
/// lifetime of a mod under development. One key yields one hit — the index marks
/// every definition in the highest layer active, so keys defined once per
/// language must be deduplicated here — sited at the target language's
/// effective definition (English as the navigation fallback): keys defined
/// only in other languages stay listed so they remain findable, but carry no
/// value. Key matching follows `key_match` (substring by default); value
/// matching runs against the bounded decoded preview, case-insensitively, so
/// it only ever sees the target language.
pub fn localisation_search_with_cancellation(
    snapshot: &AnalysisSnapshot,
    key: Option<&str>,
    value: Option<&str>,
    key_match: LocalisationKeyMatch,
    limit: usize,
    cancellation: &CancellationToken,
) -> Result<LocalisationSearchResult, Cancelled> {
    localisation_search_page_with_cancellation(
        snapshot,
        key,
        value,
        key_match,
        0,
        limit,
        cancellation,
    )
}

/// Searches a deterministic localisation page without materializing the preceding hits.
pub fn localisation_search_page_with_cancellation(
    snapshot: &AnalysisSnapshot,
    key: Option<&str>,
    value: Option<&str>,
    key_match: LocalisationKeyMatch,
    offset: usize,
    limit: usize,
    cancellation: &CancellationToken,
) -> Result<LocalisationSearchResult, Cancelled> {
    let mut matched = 0usize;
    let value_query = value.map(str::to_ascii_lowercase);
    let mut hits = Vec::new();
    let mut seen_keys = BTreeSet::new();
    let mut truncated = false;
    cancellation.checkpoint()?;
    for (index, definition) in snapshot.index().definitions_iter().enumerate() {
        if index & 1023 == 0 {
            cancellation.checkpoint()?;
        }
        if !definition.active || !definition.kind.eq_ignore_ascii_case("localisation") {
            continue;
        }
        if let Some(query) = key
            && !key_match.matches(&definition.name, query)
        {
            continue;
        }
        if !seen_keys.insert(definition.name.to_string()) {
            continue;
        }
        let candidates =
            symbol_candidates_for_hover(snapshot, "localisation", &definition.name, cancellation)?;
        let Some(winner) = effective_localisation_candidate(&candidates) else {
            continue;
        };
        let Some(file_id) = winner.location.file else {
            continue;
        };
        let preview = snapshot.localisation_preview(file_id, winner.location.range);
        if let Some(query) = value_query.as_deref() {
            let Some(preview) = preview else {
                continue;
            };
            if !preview.value.to_ascii_lowercase().contains(query) {
                continue;
            }
        }
        matched += 1;
        if matched <= offset {
            continue;
        }
        if hits.len() == limit {
            truncated = true;
            break;
        }
        let language = preview
            .and_then(|preview| preview.language.clone())
            .or_else(|| {
                localisation_language(winner.location.path.as_ref())
                    .map(|language| format!("l_{language}"))
            });
        hits.push(LocalisationSearchHit {
            key: definition.name.to_string(),
            language,
            value: preview.map(|preview| preview.value.clone()),
            file_id,
            range: winner.location.range,
        });
    }
    Ok(LocalisationSearchResult { hits, truncated })
}

/// The `$NAME$` fragment at a position inside a localisation value. The inner
/// text is either a nested localisation key reference or a placeholder the
/// displaying context binds (`$WHO$`, `$VAL$` — vanilla binds these in the
/// engine, not in script). Returns the full fragment including both `$`
/// delimiters and the inner name; `$$` escapes and fragment bodies with
/// whitespace or command syntax are rejected.
pub(crate) fn localisation_key_reference_fragment(
    input: &ParsedInput,
    position: TextSize,
) -> Option<(TextRange, String)> {
    if input.format != FileFormat::Localisation {
        return None;
    }
    let offset = usize::try_from(position).ok()?.min(input.source.len());
    if !input.source.is_char_boundary(offset) {
        return None;
    }
    let open = input.source[..offset].rfind('$')?;
    if input.source[open + 1..offset].contains('$') {
        return None;
    }
    let close = offset + input.source[offset..].find('$')?;
    let name = input.source.get(open + 1..close)?;
    if name.is_empty() || name.len() > 128 {
        return None;
    }
    if !name
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || character == '_' || character == '.')
    {
        return None;
    }
    let range = TextRange::new(u32::try_from(open).ok()?, u32::try_from(close + 1).ok()?)?;
    Some((range, name.to_owned()))
}

/// Renders the preview value of a localisation definition, preferring the snapshot cache and
/// falling back to a bounded CST lookup in the backing file.
pub(crate) fn localisation_preview(
    snapshot: &AnalysisSnapshot,
    definition: &ResolutionDefinition,
) -> Option<(Option<String>, String)> {
    if definition.location.document.is_none()
        && let Some(file) = definition.location.file
    {
        let range = definition.location.range;
        let preview = snapshot.localisation_preview(file, range).or_else(|| {
            let entries = snapshot.file_state(file)?.cached_localisation_previews()?;
            let index = entries
                .binary_search_by_key(&range, |(range, _)| *range)
                .ok()?;
            Some(&entries[index].1)
        });
        if let Some(preview) = preview {
            return Some((preview.language.clone(), preview.value.clone()));
        }
    }
    let (language, value) = localisation_text_from_source(snapshot, definition)?;
    Some((language, truncate_localisation_preview(&value)))
}

/// Full text for an explicitly opened preview, including unsaved localisations.
pub(crate) fn localisation_text_from_source(
    snapshot: &AnalysisSnapshot,
    definition: &ResolutionDefinition,
) -> Option<(Option<String>, String)> {
    let input = definition
        .location
        .document
        .as_ref()
        .and_then(|document| input_for_document(snapshot, document))
        .or_else(|| {
            definition
                .location
                .file
                .and_then(|file| crate::support::syntax_input_for_source_file(snapshot, file))
        })?;
    let ParsedContent::Text(parsed) = &input.parsed;
    let entry = find_cst_node(
        parsed.root(),
        CstKind::LocalisationEntry,
        definition.location.range,
    )?;
    let value_node = entry.children().find(|child| {
        matches!(
            child.kind(),
            CstKind::LocalisationString | CstKind::UnquotedValue
        )
    })?;
    let raw = parsed.text(value_node.range())?.trim();
    let value = raw
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .unwrap_or(raw);
    // Mirror the engine preview derivation: transcoded (escaped) values decode to
    // the readable text the game renders; readable values pass through unchanged.
    let value = transcode::decode_value(value);
    let mut language = None;
    for node in parsed.root().children() {
        if node.range().start() > entry.range().start() {
            break;
        }
        if node.kind() == CstKind::LanguageHeader
            && let Some(value) = node
                .children()
                .find(|child| child.kind() == CstKind::LocalisationKey)
                .and_then(|child| parsed.text(child.range()))
        {
            language = Some(value.trim().to_owned());
        }
    }
    Some((language, value))
}

/// Finds the target-language preview of a localisation key. Candidates arrive
/// sorted by priority descending then read order ascending, so per language
/// the effective value is the *latest* definition of the highest layer — the
/// game's later-load override semantics. Only the workspace preview language
/// (the first configured preference, English by default) survives the final
/// filter; previews whose language is unknown (no language header) pass
/// through unchanged.
#[derive(Clone, Debug)]
pub(crate) struct LocalisationPreview {
    language: Option<String>,
    pub(crate) value: String,
    pub(crate) location: Location,
}

pub(crate) fn localisation_previews_for_name(
    snapshot: &AnalysisSnapshot,
    name: &str,
    cancellation: &CancellationToken,
) -> Result<Vec<LocalisationPreview>, Cancelled> {
    let mut previews: Vec<LocalisationPreview> = Vec::new();
    let mut priorities: Vec<u64> = Vec::new();
    for candidate in symbol_candidates_for_hover(snapshot, "localisation", name, cancellation)? {
        let (language, value) = localisation_preview(snapshot, &candidate).unwrap_or_else(|| {
            (
                localisation_language(candidate.location.path.as_ref()),
                String::new(),
            )
        });
        let claimed = previews.iter().position(|preview| {
            preview.language.as_deref().is_some_and(|known| {
                language
                    .as_deref()
                    .is_some_and(|current| known.eq_ignore_ascii_case(current))
            })
        });
        let preview = LocalisationPreview {
            language,
            value,
            location: crate::resolution::definition_selection_location(&candidate),
        };
        match claimed {
            // Same layer, later read order: the successor overrides the pick.
            Some(index) if candidate.priority == priorities[index] => {
                previews[index] = preview;
            }
            // Lower layer or an unseen language: keep the existing pick / append.
            Some(_) => {}
            None => {
                previews.push(preview);
                priorities.push(candidate.priority);
            }
        }
    }
    let target = snapshot.localisation_preview_language();
    previews.retain(|preview| vfs::preview_language_is_target(preview.language.as_deref(), target));
    Ok(previews)
}

/// One rendered localisation preview line: the resolved text for one key
/// candidate in the target language.  The label names the binding field (or
/// the explicit source field) that produced the key, when known.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LocalisationPreviewRow {
    pub label: Option<String>,
    pub key: String,
    pub value: String,
    pub location: Option<Location>,
}

/// Maximum number of field groups shown. Qualified fields within a group share
/// one slot, so all ranks and titles of a government name can be shown.
const MAX_PREVIEW_FIELD_GROUPS: usize = 8;

/// Finds the localisation previews for a symbol definition.
///
/// Type-instance localisation mappings are indexed as ordinary localisation references at the
/// instance's source range.  Looking those references up from the resolved definition lets a
/// hover over `event = foo.1` (or another typed symbol use) show the same preview as hovering its
/// generated localisation key. Declared bindings generate keys such as `$_title`
/// and `building_$` for every definition of their type.
///
/// Every strategy contributes its resolvable rows instead of the first match winning: a
/// definition may carry an explicit `title` while its `desc` template also resolves, and the
/// hover shows both. An existing same-name key can supply a preview for any
/// definition without resolvable localisation bindings or fields.
pub(crate) fn symbol_localisation_preview(
    snapshot: &AnalysisSnapshot,
    kind: &str,
    symbol_name: &str,
    definition: &ResolutionDefinition,
    cancellation: &CancellationToken,
) -> Result<Vec<LocalisationPreviewRow>, Cancelled> {
    let full_range = definition.location.range;
    let selection_range = definition.selection_range;
    let input = definition
        .location
        .document
        .as_ref()
        .and_then(|document| input_for_document(snapshot, document))
        .or_else(|| {
            definition
                .location
                .file
                .and_then(|file| input_for_source_file(snapshot, file))
        });
    // Classifying explicit fields requires the source. Index-only definitions
    // can still use the type bindings and same-name lookup below.
    let mut references = match input.as_ref() {
        Some(input) => definition_field_references(snapshot, input, full_range, cancellation)?,
        None => Vec::new(),
    };
    references.sort_by_key(|(_, range, _)| {
        (
            if *range == selection_range { 0 } else { 1 },
            range.start(),
            range.end(),
        )
    });
    references.dedup();

    let mut rows = Vec::new();
    let mut seen = BTreeSet::new();
    for (name, range, field) in references {
        cancellation.checkpoint()?;
        let previews = localisation_previews_for_name(snapshot, &name, cancellation)?;
        let label = generated_key_field(snapshot, kind, symbol_name, &name)
            .map(str::to_owned)
            .or(field);
        for row in preview_rows(label, &name, &previews, range != selection_range) {
            if seen.insert((row.label.clone(), row.key.clone(), row.value.clone())) {
                rows.push(row);
            }
        }
    }
    collect_generated_preview_rows(
        snapshot,
        kind,
        symbol_name,
        &mut rows,
        &mut seen,
        cancellation,
    )?;
    if rows.iter().all(|row| row.location.is_none()) {
        let previews = localisation_previews_for_name(snapshot, symbol_name, cancellation)?;
        rows.extend(preview_rows(None, symbol_name, &previews, false));
    }
    Ok(rows)
}

/// Every binding declared by the resolved instance's type.
fn type_trait_impls(
    snapshot: &AnalysisSnapshot,
    type_id: rules::ir::TypeId,
) -> &[rules::ir::TraitImpl] {
    &snapshot.ir().type_info(type_id).trait_impls
}

/// Appends every generated-key row (self bindings and template bindings alike)
/// whose key resolves, labelling each group with the binding name.
fn collect_generated_preview_rows(
    snapshot: &AnalysisSnapshot,
    kind: &str,
    symbol_name: &str,
    rows: &mut Vec<LocalisationPreviewRow>,
    seen: &mut BTreeSet<(Option<String>, String, String)>,
    cancellation: &CancellationToken,
) -> Result<(), Cancelled> {
    let ir = snapshot.ir();
    if let Some(type_id) = ir.type_by_name(kind) {
        for implementation in type_trait_impls(snapshot, type_id) {
            for (label, argument) in &implementation.arguments {
                if let rules::ir::TraitArgument::Binding(binding) = argument
                    && let Some(template) = binding.loc
                {
                    cancellation.checkpoint()?;
                    let mut name = binding.expand(ir.strings(), template, symbol_name);
                    let mut previews =
                        localisation_previews_for_name(snapshot, &name, cancellation)?;
                    if previews.iter().all(|preview| preview.value.is_empty())
                        && let Some(template) = binding.preview_fallback
                    {
                        let fallback = binding.expand(ir.strings(), template, symbol_name);
                        let fallback_previews =
                            localisation_previews_for_name(snapshot, &fallback, cancellation)?;
                        if fallback_previews
                            .iter()
                            .any(|preview| !preview.value.is_empty())
                        {
                            name = fallback;
                            previews = fallback_previews;
                        }
                    }
                    for row in preview_rows(
                        Some(ir.strings.resolve(*label).to_owned()),
                        &name,
                        &previews,
                        binding.required,
                    ) {
                        if seen.insert((row.label.clone(), row.key.clone(), row.value.clone())) {
                            rows.push(row);
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

/// Builds a row for each resolved value, or for a missing required key.
pub(crate) fn preview_rows(
    label: Option<String>,
    key: &str,
    previews: &[LocalisationPreview],
    required: bool,
) -> Vec<LocalisationPreviewRow> {
    if previews.is_empty() && required {
        return vec![LocalisationPreviewRow {
            label,
            key: key.to_owned(),
            value: "Missing localisation".to_owned(),
            location: None,
        }];
    }
    previews
        .iter()
        .filter(|preview| !preview.value.is_empty())
        .map(|preview| LocalisationPreviewRow {
            label: label.clone(),
            key: key.to_owned(),
            value: preview.value.clone(),
            location: Some(preview.location.clone()),
        })
        .collect()
}

/// The binding name whose generated key expands to `key` for this
/// instance, when one exists — labels generated-key references hovering a
/// definition (`mission_one_title` → `name`).
fn generated_key_field<'a>(
    snapshot: &'a AnalysisSnapshot,
    kind: &str,
    symbol_name: &str,
    key: &str,
) -> Option<&'a str> {
    let ir = snapshot.ir();
    ir.type_by_name(kind).and_then(|id| {
        type_trait_impls(snapshot, id)
            .iter()
            .find_map(|implementation| {
                implementation
                    .arguments
                    .iter()
                    .find_map(|(label, argument)| {
                        let rules::ir::TraitArgument::Binding(binding) = argument else {
                            return None;
                        };
                        let template = binding.loc?;
                        binding
                            .expand(ir.strings(), template, symbol_name)
                            .eq_ignore_ascii_case(key)
                            .then(|| ir.strings.resolve(*label))
                    })
            })
    })
}

/// Formats the localisation-preview section over labelled row groups as a
/// markdown table, capped per field with a trailing count line for further
/// fields. The field label links to the entry supplying its text.
pub(crate) fn localisation_preview_section(
    rows: &[LocalisationPreviewRow],
    mut source_link: impl FnMut(&str, Location) -> String,
) -> String {
    let mut lines = vec!["| Field | Text |".to_owned(), "| --- | --- |".to_owned()];
    let mut shown = BTreeSet::new();
    let mut hidden = BTreeSet::new();
    for row in rows {
        let label = row.label.as_deref().unwrap_or("Localisation");
        let group = label.split_once('.').map_or(label, |(parent, _)| parent);
        if !shown.contains(group) && shown.len() == MAX_PREVIEW_FIELD_GROUPS {
            hidden.insert(group);
            continue;
        }
        shown.insert(group);
        let field = row.location.as_ref().map_or_else(
            || escape_table_cell(label),
            |location| source_link(label, location.clone()),
        );
        lines.push(format!("| {field} | {} |", escape_table_cell(&row.value)));
    }

    let mut section = format!("#### Localisation\n\n{}", lines.join("\n"));
    if !hidden.is_empty() {
        section.push_str(&format!("\n\n… and {} more fields", hidden.len()));
    }
    section
}

/// Escapes text for a markdown table cell: a pipe would end the cell early
/// and a newline would split the row.
fn escape_table_cell(text: &str) -> String {
    text.replace(['\n', '\r'], " ").replace('|', "\\|")
}

/// UI fields can live inside data blocks such as event options and conditional
/// descriptions. Their neighbouring effects and triggers are separate scripts.
fn definition_field_references(
    snapshot: &AnalysisSnapshot,
    input: &ParsedInput,
    definition: TextRange,
    cancellation: &CancellationToken,
) -> Result<Vec<(String, TextRange, Option<String>)>, Cancelled> {
    let Some(hir) = input.hir.as_deref() else {
        return Ok(Vec::new());
    };
    let ir = snapshot.ir();
    let script_blocks: Vec<_> = hir
        .schema_facts()
        .iter()
        .filter(|fact| {
            fact.range.start() < definition.end()
                && definition.start() < fact.range.end()
                && script_context(ir.strings().resolve(ir.schema(fact.schema).name))
        })
        .map(|fact| fact.range)
        .collect();
    let properties = hir.properties_in_range(definition);
    let child_definitions: BTreeSet<_> = hir
        .definitions()
        .iter()
        .map(|child| child.range)
        .filter(|range| *range != definition && text_range_within(*range, definition))
        .collect();
    let child_blocks: Vec<_> = properties
        .clone()
        .filter(|property| property.scalar.is_none() && child_definitions.contains(&property.range))
        .map(|property| property.range)
        .collect();

    let semantic = semantic_data_with_cancellation(snapshot, input, cancellation)?;
    let mut references = Vec::new();
    for (name, range) in semantic
        .references
        .into_iter()
        .filter(|reference| reference.kind.eq_ignore_ascii_case("localisation"))
        .map(|reference| (reference.name, reference.range))
        .chain(
            hir.binding_references_for_hover()
                .iter()
                .filter(|reference| reference.kind.eq_ignore_ascii_case("localisation"))
                .map(|reference| (reference.name.clone(), reference.range)),
        )
    {
        cancellation.checkpoint()?;
        if !text_range_within(range, definition)
            || script_blocks
                .iter()
                .chain(&child_blocks)
                .any(|block| text_range_within(range, *block))
        {
            continue;
        }
        let property = properties.clone().rev().find(|property| {
            text_range_within(range, property.key_range)
                || property
                    .value_range
                    .is_some_and(|value| text_range_within(range, value))
        });
        let mut label = None;
        if let Some(property) = property {
            let fact = hir.field_fact_at(property.key_range);
            // Hybrid data schemas can import executable fields, for example
            // custom_tooltip directly in an event option via keys__effect.
            if fact.is_some_and(|fact| {
                !fact.fields.is_empty()
                    && fact.fields.iter().all(|field| {
                        ir.provenance_of(*field)
                            .and_then(|source| {
                                ir.strings().resolve(source.pointer).split('/').nth(2)
                            })
                            .is_some_and(script_context)
                    })
            }) {
                continue;
            }
            // A reference inside a quoted script value is not a data field.
            if property.scalar.as_ref().is_some_and(|scalar| {
                text_range_within(range, scalar.range)
                    && range != scalar.range
                    && !scalar.value.eq_ignore_ascii_case(&name)
            }) {
                continue;
            }
            label = Some(
                if fact.is_some_and(|fact| {
                    let schema = ir.schema(fact.schema);
                    !fact.fields.is_empty()
                        && fact.fields.iter().all(|id| schema.patterns.contains(id))
                }) && let Some(parent) = property.path.iter().rev().nth(1)
                {
                    format!("{parent}.{}", property.key)
                } else {
                    property.key.clone()
                },
            );
        }
        references.push((name, range, label));
    }
    Ok(references)
}

fn script_context(schema: &str) -> bool {
    schema.split("__").any(|part| {
        matches!(
            part,
            "effect"
                | "trigger"
                | "modifier_rule"
                | "country_history_body"
                | "province_history_body"
        )
    })
}

pub(crate) fn find_cst_node(
    node: CstNode<'_>,
    kind: CstKind,
    range: TextRange,
) -> Option<CstNode<'_>> {
    find_cst_node_bounded(node, kind, range, MAX_CST_SEARCH_DEPTH)
}

/// Nesting bound for CST lookups; localisation files are flat, so this only guards against
/// pathological or corrupted trees, mirroring the bounded-scan rule for workspace scanning.
const MAX_CST_SEARCH_DEPTH: usize = 64;

fn find_cst_node_bounded(
    node: CstNode<'_>,
    kind: CstKind,
    range: TextRange,
    depth: usize,
) -> Option<CstNode<'_>> {
    if node.kind() == kind && node.range() == range {
        return Some(node);
    }
    if depth == 0 {
        return None;
    }
    node.children()
        .find_map(|child| find_cst_node_bounded(child, kind, range, depth - 1))
}
