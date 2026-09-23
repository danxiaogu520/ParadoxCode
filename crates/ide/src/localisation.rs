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
use crate::types::{CancellationToken, Cancelled};

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
    if let Some(file) = definition.location.file
        && let Some(preview) = snapshot.localisation_preview(file, definition.location.range)
    {
        return Some((preview.language.clone(), preview.value.clone()));
    }
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
    let value = truncate_localisation_preview(&transcode::decode_value(value));
    if value.is_empty() {
        return None;
    }
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
pub(crate) fn localisation_previews_for_name(
    snapshot: &AnalysisSnapshot,
    name: &str,
    cancellation: &CancellationToken,
) -> Result<Vec<(Option<String>, String)>, Cancelled> {
    let mut previews: Vec<(Option<String>, String)> = Vec::new();
    let mut priorities: Vec<u64> = Vec::new();
    for candidate in symbol_candidates_for_hover(snapshot, "localisation", name, cancellation)? {
        let Some(preview) = localisation_preview(snapshot, &candidate) else {
            continue;
        };
        if preview.1.is_empty() {
            continue;
        }
        let claimed = previews.iter().position(|(language, _)| {
            language.as_deref().is_some_and(|known| {
                preview
                    .0
                    .as_deref()
                    .is_some_and(|current| known.eq_ignore_ascii_case(current))
            })
        });
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
    previews.retain(|(language, _)| preview_language_is_target(language.as_deref(), target));
    Ok(previews)
}

/// Whether a preview language in YAML-header form (`l_english`) names the
/// workspace target language; previews with an unknown (`None`) language
/// pass through.
pub(crate) fn preview_language_is_target(language: Option<&str>, target: &str) -> bool {
    language.is_none_or(|language| {
        language
            .strip_prefix("l_")
            .unwrap_or(language)
            .eq_ignore_ascii_case(target)
    })
}

/// One rendered localisation preview line: the resolved text for one key
/// candidate in the target language.  The label names the binding field (or
/// the explicit source field) that produced the key, when known.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LocalisationPreviewRow {
    pub label: Option<String>,
    pub value: String,
}

/// Maximum labelled fields rendered; further fields collapse into a count
/// line.  Binding rows per kind are few (≤5), so this mostly bounds
/// definition bodies carrying many explicit localisation references.
const MAX_PREVIEW_FIELDS: usize = 6;

/// Finds the localisation previews for a non-localisation symbol definition.
///
/// Type-instance localisation mappings are indexed as ordinary localisation references at the
/// instance's source range.  Looking those references up from the resolved definition lets a
/// hover over `event = foo.1` (or another typed symbol use) show the same preview as hovering its
/// generated localisation key. Type descriptors may also use the implicit same-name convention
/// without a localisation-binding row. Finally, the per-family generated templates
/// (`$_title`, `$.t`, `building_$`, …) are tried for every definition — the
/// coverage matrix — and only shown when the generated key actually resolves.
///
/// Every strategy contributes its resolvable rows instead of the first match winning: a
/// definition may carry an explicit `title` while its `desc` template also resolves, and the
/// hover shows both, one labelled field per row group. Rows repeating a `(language, value)`
/// pair already shown are dropped.
pub(crate) fn symbol_localisation_preview(
    snapshot: &AnalysisSnapshot,
    kind: &str,
    symbol_name: &str,
    definition: &ResolutionDefinition,
    cancellation: &CancellationToken,
) -> Result<Vec<LocalisationPreviewRow>, Cancelled> {
    if kind.eq_ignore_ascii_case("localisation") {
        let previews = localisation_previews_for_name(snapshot, symbol_name, cancellation)?;
        return Ok(unlabelled_preview_rows(&previews));
    }
    if !kind_is_localisation_displayable(snapshot, kind) {
        return Ok(Vec::new());
    }

    let full_range = definition.location.range;
    let selection_range = definition.selection_range;
    let mut references = Vec::<(String, TextRange)>::new();
    let mut label_input = None;
    if let Some(document) = definition.location.document.as_ref() {
        if let Some(input) = input_for_document(snapshot, document) {
            references.extend(localisation_references_for_hover(
                snapshot,
                &input,
                cancellation,
            )?);
            label_input = Some(input);
        }
    } else if let Some(file) = definition.location.file {
        if let Some(input) = input_for_source_file(snapshot, file) {
            references.extend(localisation_references_for_hover(
                snapshot,
                &input,
                cancellation,
            )?);
            label_input = Some(input);
        } else {
            references.extend(
                snapshot
                    .index()
                    .references(file)
                    .iter()
                    .filter(|reference| reference.kind.eq_ignore_ascii_case("localisation"))
                    .map(|reference| (reference.name.to_string(), reference.range)),
            );
        }
    }
    references.retain(|(_, range)| text_range_within(*range, full_range));
    references.sort_by_key(|(_, range)| {
        (
            if *range == selection_range { 0 } else { 1 },
            range.start(),
            range.end(),
        )
    });
    references.dedup();

    let mut rows = Vec::new();
    let mut seen = BTreeSet::<String>::new();
    for (name, range) in references {
        cancellation.checkpoint()?;
        let previews = localisation_previews_for_name(snapshot, &name, cancellation)?;
        if previews.is_empty() {
            continue;
        }
        let label = generated_key_field(snapshot, kind, symbol_name, &name)
            .map(str::to_owned)
            .or_else(|| {
                label_input
                    .as_ref()
                    .and_then(|input| enclosing_field_label(input, range))
            });
        push_preview_rows(&mut rows, &mut seen, label, &previews);
    }
    collect_generated_preview_rows(
        snapshot,
        kind,
        symbol_name,
        is_type_definition(snapshot, kind),
        &mut rows,
        &mut seen,
        cancellation,
    )?;
    Ok(rows)
}

/// Same-name and generated-template localisation previews for a typed token
/// whose definition site is not at hand — the rule-layer hovers over
/// `Type`-matched scope links (`tripolitania_area = { … }`) and typed scalar
/// values. Localisation keys resolve on their own, so no definition lookup
/// participates.
pub(crate) fn typed_name_localisation_previews(
    snapshot: &AnalysisSnapshot,
    kind: &str,
    name: &str,
    cancellation: &CancellationToken,
) -> Result<Vec<LocalisationPreviewRow>, Cancelled> {
    if !kind_is_localisation_displayable(snapshot, kind) {
        return Ok(Vec::new());
    }
    let mut rows = Vec::new();
    let mut seen = BTreeSet::<String>::new();
    collect_generated_preview_rows(
        snapshot,
        kind,
        name,
        is_type_definition(snapshot, kind),
        &mut rows,
        &mut seen,
        cancellation,
    )?;
    Ok(rows)
}

/// Whether hover localisation previews apply to a kind: it carries
/// localisation bindings or is a type descriptor (the implicit same-name
/// convention).
pub(crate) fn kind_is_localisation_displayable(snapshot: &AnalysisSnapshot, kind: &str) -> bool {
    has_localisation_binding(snapshot, kind) || is_type_definition(snapshot, kind)
}

fn has_localisation_binding(snapshot: &AnalysisSnapshot, kind: &str) -> bool {
    snapshot
        .rules()
        .model()
        .semantic
        .localisation_bindings
        .iter()
        .any(|binding| binding.type_name.eq_ignore_ascii_case(kind))
}

fn is_type_definition(snapshot: &AnalysisSnapshot, kind: &str) -> bool {
    snapshot
        .rules()
        .model()
        .semantic
        .type_descriptors
        .keys()
        .any(|type_name| type_name.eq_ignore_ascii_case(kind))
}

/// Appends the implicit same-name row (for type descriptors) and every
/// generated-template row whose key resolves, labelling each group with the
/// binding field.
fn collect_generated_preview_rows(
    snapshot: &AnalysisSnapshot,
    kind: &str,
    symbol_name: &str,
    same_name: bool,
    rows: &mut Vec<LocalisationPreviewRow>,
    seen: &mut BTreeSet<String>,
    cancellation: &CancellationToken,
) -> Result<(), Cancelled> {
    if same_name {
        cancellation.checkpoint()?;
        let previews = localisation_previews_for_name(snapshot, symbol_name, cancellation)?;
        push_preview_rows(rows, seen, None, &previews);
    }
    for binding in snapshot
        .rules()
        .model()
        .semantic
        .localisation_bindings
        .iter()
        .filter(|binding| binding.type_name.eq_ignore_ascii_case(kind))
    {
        let Some(template) = binding.template.as_deref() else {
            continue;
        };
        let name = template.replace('$', symbol_name);
        cancellation.checkpoint()?;
        let previews = localisation_previews_for_name(snapshot, &name, cancellation)?;
        push_preview_rows(rows, seen, Some(binding.field.clone()), &previews);
    }
    Ok(())
}

/// Appends one row per value not already shown; empty values and repeats of
/// an identical value are dropped.
fn push_preview_rows(
    rows: &mut Vec<LocalisationPreviewRow>,
    seen: &mut BTreeSet<String>,
    label: Option<String>,
    previews: &[(Option<String>, String)],
) {
    for (_, value) in previews {
        if value.is_empty() || !seen.insert(value.clone()) {
            continue;
        }
        rows.push(LocalisationPreviewRow {
            label: label.clone(),
            value: value.clone(),
        });
    }
}

/// Wraps the previews of one key as unlabelled rows.
pub(crate) fn unlabelled_preview_rows(
    previews: &[(Option<String>, String)],
) -> Vec<LocalisationPreviewRow> {
    previews
        .iter()
        .map(|(_, value)| LocalisationPreviewRow {
            label: None,
            value: value.clone(),
        })
        .collect()
}

/// The binding field whose generated template expands to `key` for this
/// instance, when one exists — labels derived-template references hovering a
/// definition (`event_one.1.t` → `title_default`).
fn generated_key_field<'a>(
    snapshot: &'a AnalysisSnapshot,
    kind: &str,
    symbol_name: &str,
    key: &str,
) -> Option<&'a str> {
    snapshot
        .rules()
        .model()
        .semantic
        .localisation_bindings
        .iter()
        .filter(|binding| binding.type_name.eq_ignore_ascii_case(kind))
        .find_map(|binding| {
            let template = binding.template.as_deref()?;
            template
                .replace('$', symbol_name)
                .eq_ignore_ascii_case(key)
                .then_some(binding.field.as_str())
        })
}

/// The key of the property whose scalar value sits at `range` — labels
/// explicit localisation references with their source field (`title = …`).
fn enclosing_field_label(input: &ParsedInput, range: TextRange) -> Option<String> {
    let hir = input.hir.as_deref()?;
    hir.properties()
        .iter()
        .find(|property| {
            property
                .scalar
                .as_ref()
                .is_some_and(|scalar| text_range_within(range, scalar.range))
        })
        .map(|property| property.key.to_string())
}

/// Formats the localisation-preview section over labelled row groups as a
/// markdown table, capped per field with a trailing count line for further
/// fields.  Rows carry no language — the preview language is the
/// workspace-configured target, so the table needs only the Field column
/// (when any shown row carries a label) and the text.
pub(crate) fn localisation_preview_section(rows: &[LocalisationPreviewRow]) -> String {
    let mut shown_rows = Vec::<&LocalisationPreviewRow>::new();
    let mut fields_shown = 0;
    let mut fields_hidden = 0usize;
    let mut start = 0;
    while start < rows.len() {
        let label = rows[start].label.clone();
        let end = start
            + rows[start..]
                .iter()
                .position(|row| row.label != label)
                .unwrap_or(rows.len() - start);
        let group = &rows[start..end];
        start = end;
        if fields_shown == MAX_PREVIEW_FIELDS {
            fields_hidden += 1;
            continue;
        }
        fields_shown += 1;
        shown_rows.extend(group.iter());
    }

    let show_label = shown_rows.iter().any(|row| row.label.is_some());
    let mut headers = Vec::new();
    if show_label {
        headers.push("Field");
    }
    headers.push("Text");
    let mut lines = vec![
        format!("| {} |", headers.join(" | ")),
        format!("| {} |", vec!["---"; headers.len()].join(" | ")),
    ];
    lines.extend(shown_rows.iter().map(|row| {
        let mut cells = Vec::new();
        if show_label {
            cells.push(escape_table_cell(
                row.label.as_deref().unwrap_or("Localisation"),
            ));
        }
        cells.push(escape_table_cell(&row.value));
        format!("| {} |", cells.join(" | "))
    }));

    let mut section = format!("#### Localisation preview\n\n{}", lines.join("\n"));
    if fields_hidden > 0 {
        section.push_str(&format!("\n\n… and {fields_hidden} more fields"));
    }
    section
}

/// Escapes text for a markdown table cell: a pipe would end the cell early
/// and a newline would split the row.
fn escape_table_cell(text: &str) -> String {
    text.replace(['\n', '\r'], " ").replace('|', "\\|")
}

fn localisation_references_for_hover(
    snapshot: &AnalysisSnapshot,
    input: &ParsedInput,
    cancellation: &CancellationToken,
) -> Result<Vec<(String, TextRange)>, Cancelled> {
    let derived = input
        .hir
        .as_deref()
        .zip(input.path.as_ref())
        .map(|(hir, path)| {
            hir::derived_localisation_references_for_hover(hir, path, snapshot.rules())
        })
        .unwrap_or_default();
    let semantic = semantic_data_with_cancellation(snapshot, input, cancellation)?;
    let mut references = semantic
        .references
        .into_iter()
        .filter(|reference| reference.kind.eq_ignore_ascii_case("localisation"))
        .map(|reference| (reference.name, reference.range))
        .collect::<Vec<_>>();
    references.extend(
        derived
            .into_iter()
            .map(|reference| (reference.name, reference.range)),
    );
    Ok(references)
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
