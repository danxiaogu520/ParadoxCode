//! Scripted-localisation indexing and localisation lookup queries.
//!
//! CWTools treats scripted localisation as a path-driven namespace: definitions are read from
//! `name = ...` fields below a scripted-localisation directory, even when the active ruleset does
//! not describe that directory as a typed definition.  The engine lowers those facts into the
//! ordinary `defined_text` symbol family; this module adds the snapshot query and the editor
//! behaviour that consumes it.

use std::sync::Arc;

use engine::{AnalysisSnapshot, DocumentSource};
use parser::{CstKind, CstNode, FileFormat};
use text::{TextRange, TextSize};

use crate::resolution::{
    ResolutionDefinition, semantic_data_with_cancellation, symbol_candidates_for_hover,
    text_range_within,
};
use crate::support::{
    ParsedContent, ParsedInput, input_for_document, input_for_source_file, truncate_hover_text,
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
    let value = truncate_hover_text(&transcode::decode_value(value));
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

/// Finds every language a localisation key resolves to, one preview per
/// language. Candidates arrive sorted by priority descending then read order
/// ascending, so per language the effective value is the *latest* definition
/// of the highest layer — the game's later-load override semantics. The hover
/// renders the languages in parallel instead of a single first-found value.
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
    Ok(previews)
}

/// Finds the localisation previews for a non-localisation symbol definition.
///
/// Type-instance localisation mappings are indexed as ordinary localisation references at the
/// instance's source range.  Looking those references up from the resolved definition lets a
/// hover over `event = foo.1` (or another typed symbol use) show the same preview as hovering its
/// generated localisation key. Type descriptors may also use the implicit same-name convention
/// without a localisation-binding row. Finally, the per-family generated templates
/// (`$_title`, `$.t`, `building_$`, …) are tried for every definition — the
/// coverage matrix — and only shown when the generated key actually resolves;
/// this closes, among others, events without an explicit `title` (the engine
/// falls back to `<id>.t`, verified against vanilla) and mod-authored
/// definitions whose family convention needs no in-file reference at all.
pub(crate) fn symbol_localisation_preview(
    snapshot: &AnalysisSnapshot,
    kind: &str,
    symbol_name: &str,
    definition: &ResolutionDefinition,
    cancellation: &CancellationToken,
) -> Result<Vec<(Option<String>, String)>, Cancelled> {
    if kind.eq_ignore_ascii_case("localisation") {
        return localisation_previews_for_name(snapshot, symbol_name, cancellation);
    }
    let semantic = &snapshot.rules().model().semantic;
    let has_binding = semantic
        .localisation_bindings
        .iter()
        .any(|binding| binding.type_name.eq_ignore_ascii_case(kind));
    let is_type_definition = semantic
        .type_descriptors
        .keys()
        .any(|type_name| type_name.eq_ignore_ascii_case(kind));
    if !has_binding && !is_type_definition {
        return Ok(Vec::new());
    }

    let full_range = definition.location.range;
    let selection_range = definition.selection_range;
    let mut references = Vec::<(String, TextRange)>::new();
    if let Some(document) = definition.location.document.as_ref() {
        if let Some(input) = input_for_document(snapshot, document) {
            references.extend(localisation_references_for_hover(
                snapshot,
                &input,
                cancellation,
            )?);
        }
    } else if let Some(file) = definition.location.file {
        if let Some(input) = input_for_source_file(snapshot, file) {
            references.extend(localisation_references_for_hover(
                snapshot,
                &input,
                cancellation,
            )?);
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
    for (name, _) in references {
        cancellation.checkpoint()?;
        let previews = localisation_previews_for_name(snapshot, &name, cancellation)?;
        if !previews.is_empty() {
            return Ok(previews);
        }
    }
    if is_type_definition {
        cancellation.checkpoint()?;
        let previews = localisation_previews_for_name(snapshot, symbol_name, cancellation)?;
        if !previews.is_empty() {
            return Ok(previews);
        }
    }
    for binding in semantic
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
        if !previews.is_empty() {
            return Ok(previews);
        }
    }
    Ok(Vec::new())
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
