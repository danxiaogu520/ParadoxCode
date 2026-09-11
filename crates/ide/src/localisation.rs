//! Scripted-localisation indexing and bounded localisation-command queries.
//!
//! CWTools treats scripted localisation as a path-driven namespace: definitions are read from
//! `name = ...` fields below a scripted-localisation directory, even when the active ruleset does
//! not describe that directory as a typed definition.  The engine lowers those facts into the
//! ordinary `defined_text` symbol family; this module adds the snapshot query and the editor
//! behaviour that consumes it.

use std::collections::HashSet;
use std::sync::Arc;

use engine::{AnalysisSnapshot, DocumentSource};
use parser::{CstKind, CstNode, FileFormat};
use rules::KeyMatcher;
use text::{TextRange, TextSize};

use crate::resolution::{
    ResolutionDefinition, semantic_data_with_cancellation, symbol_candidates_for_hover,
    text_range_within,
};
use crate::support::{
    ParsedContent, ParsedInput, input_for_document, input_for_source_file, truncate_hover_text,
    word_range,
};
use crate::types::{
    CancellationToken, Cancelled, CompletionItem, CompletionKind, CompletionResult, Diagnostic,
    DiagnosticCertainty, DiagnosticCode, Severity,
};

const SCRIPTED_LOCALISATION_NAMES_CACHE_KEY: &str = "scripted-localisation-names";
const LOCALISATION_COMMANDS_CACHE_KEY: &str = "localisation-command-names";
const MAX_LOCALISATION_COMMAND_DIAGNOSTICS: usize = 256;

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

/// A merged command registry used by diagnostics and completion.
struct LocalisationCommandRegistry {
    names: Vec<String>,
    lookup: HashSet<String>,
    has_scripted_localisations: bool,
}

fn localisation_command_registry(
    snapshot: &AnalysisSnapshot,
    cancellation: &CancellationToken,
) -> Result<LocalisationCommandRegistry, Cancelled> {
    let scripted = scripted_localisation_names_cached_with_cancellation(snapshot, cancellation)?;
    let static_names = static_localisation_command_names(snapshot, cancellation)?;
    let mut names = static_names.as_ref().clone();
    names.extend(scripted.iter().cloned());
    names.sort_by(|left, right| {
        left.to_ascii_lowercase()
            .cmp(&right.to_ascii_lowercase())
            .then_with(|| left.cmp(right))
    });
    names.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
    let lookup = names.iter().map(|name| name_key(name)).collect();
    Ok(LocalisationCommandRegistry {
        names,
        lookup,
        has_scripted_localisations: !scripted.is_empty(),
    })
}

impl LocalisationCommandRegistry {
    fn contains(&self, name: &str) -> bool {
        self.lookup.contains(&name_key(name))
    }
}

fn static_localisation_command_names(
    snapshot: &AnalysisSnapshot,
    cancellation: &CancellationToken,
) -> Result<Arc<Vec<String>>, Cancelled> {
    let revision = snapshot.revision();
    if let Some(cached) = snapshot
        .query_cache()
        .get::<Vec<String>>(revision, LOCALISATION_COMMANDS_CACHE_KEY)
    {
        return Ok(cached);
    }
    let mut names = Vec::new();
    for (index, rule) in snapshot
        .rules()
        .semantic_rules_for_context("root:localisation_commands")
        .enumerate()
    {
        if index & 255 == 0 {
            cancellation.checkpoint()?;
        }
        match &rule.key {
            KeyMatcher::Exact(name) => names.push(name.clone()),
            KeyMatcher::Enum(enum_name) => {
                if let Some((_, values)) = snapshot
                    .rules()
                    .model()
                    .semantic
                    .enum_values
                    .iter()
                    .find(|(name, _)| name.eq_ignore_ascii_case(enum_name))
                {
                    names.extend(values.iter().cloned());
                }
            }
            _ => {}
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
        engine::CacheDomain::Index,
        LOCALISATION_COMMANDS_CACHE_KEY.to_owned(),
        Arc::clone(&names),
    );
    Ok(names)
}

/// Produces bounded diagnostics for unknown final segments of localisation command chains.
///
/// The command language is intentionally treated conservatively.  Until at least one scripted
/// localisation is visible, unknown tails remain lenient because a static ruleset cannot
/// distinguish a typo from a runtime-defined command.  Once the registry is populated, known
/// first-party commands, `Get*` getters, and runtime/dynamic forms remain accepted while an
/// unknown final segment receives a warning.  Scope transitions are left to the script semantic
/// engine; this check only establishes the CWTools-style name registry boundary.
pub(crate) fn localisation_command_diagnostics(
    snapshot: &AnalysisSnapshot,
    input: &ParsedInput,
    cancellation: &CancellationToken,
) -> Result<Vec<Diagnostic>, Cancelled> {
    cancellation.checkpoint()?;
    if input.format != FileFormat::Localisation {
        return Ok(Vec::new());
    }
    let registry = localisation_command_registry(snapshot, cancellation)?;
    if !registry.has_scripted_localisations {
        return Ok(Vec::new());
    }
    let ParsedContent::Text(parsed) = &input.parsed;
    let mut diagnostics = Vec::new();
    for (index, entry) in parsed.root().children().enumerate() {
        if index & 31 == 0 {
            cancellation.checkpoint()?;
        }
        if entry.kind() != CstKind::LocalisationEntry {
            continue;
        }
        let Some(value) = entry.children().find(|child| {
            matches!(
                child.kind(),
                CstKind::LocalisationString | CstKind::UnquotedValue
            )
        }) else {
            continue;
        };
        for (command, range) in
            localisation_commands_in_range(input.source.as_ref(), value.range(), cancellation)?
        {
            if diagnostics.len() >= MAX_LOCALISATION_COMMAND_DIAGNOSTICS {
                return Ok(diagnostics);
            }
            if localisation_command_is_bypassed(&command)
                || command
                    .get(..3)
                    .is_some_and(|prefix| prefix.eq_ignore_ascii_case("get"))
                || registry.contains(&command)
            {
                continue;
            }
            diagnostics.push(
                Diagnostic::new(
                    DiagnosticCode::InvalidValue,
                    Severity::Warning,
                    range,
                    format!(
                        "unknown localisation command `{command}`{}",
                        crate::messages::did_you_mean(crate::suggest::best_suggestion(
                            &command,
                            registry.names.iter().map(String::as_str),
                        ))
                    ),
                )
                .with_certainty(DiagnosticCertainty::Inferred),
            );
        }
    }
    Ok(diagnostics)
}

/// Returns the current command fragment for localisation completion, if the cursor is inside an
/// unfinished `[...]` expression.  The range begins after the last `.` so a chain such as
/// `[ROOT.Get` replaces only `Get` rather than the whole expression.
pub(crate) fn localisation_command_fragment(
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
    let open = input.source[..offset].rfind('[')?;
    if input.source[open + 1..offset].contains(']') || escaped_at(&input.source, open) {
        return None;
    }
    let content = &input.source[open + 1..offset];
    if content.contains('|') {
        return None;
    }
    let segment_start = content
        .rfind('.')
        .map_or(open + 1, |relative| open + 1 + relative + 1);
    let word = word_range(&input.source, position);
    let start = segment_start.max(usize::try_from(word.start()).ok()?);
    let end = usize::try_from(word.end()).ok()?.max(offset);
    let prefix = input.source.get(start..offset)?.to_owned();
    let range = TextRange::new(u32::try_from(start).ok()?, u32::try_from(end).ok()?)?;
    Some((range, prefix))
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

/// Completes static and workspace-defined localisation commands.
pub(crate) fn localisation_command_completion(
    snapshot: &AnalysisSnapshot,
    replacement_range: TextRange,
    prefix: &str,
    cancellation: &CancellationToken,
) -> Result<CompletionResult, Cancelled> {
    let registry = localisation_command_registry(snapshot, cancellation)?;
    let mut items = registry
        .names
        .into_iter()
        .filter(|name| crate::support::completion_matches(name, prefix))
        .map(|name| CompletionItem {
            label: name.clone(),
            kind: CompletionKind::Command,
            detail: "localisation command".to_owned(),
            documentation: None,
            replacement_range,
            insert_text: name,
            sort_score: 0,
            deprecated: false,
            resolve_data: None,
        })
        .collect::<Vec<_>>();
    items.sort_by(|left, right| left.label.cmp(&right.label));
    items.dedup_by(|left, right| left.label.eq_ignore_ascii_case(&right.label));
    cancellation.checkpoint()?;
    Ok(CompletionResult {
        revision: snapshot.revision(),
        items,
    })
}

fn localisation_commands_in_range(
    source: &str,
    range: TextRange,
    cancellation: &CancellationToken,
) -> Result<Vec<(String, TextRange)>, Cancelled> {
    let start = usize::try_from(range.start()).ok();
    let end = usize::try_from(range.end()).ok();
    let Some((start, end)) = start.zip(end).filter(|(start, end)| *start <= *end) else {
        return Ok(Vec::new());
    };
    let bytes = source.as_bytes();
    let mut commands = Vec::new();
    let mut open = None;
    let mut depth = 0usize;
    let mut escaped = false;
    for (index, byte) in bytes
        .iter()
        .enumerate()
        .take(end.min(bytes.len()))
        .skip(start)
    {
        if index & 255 == 0 {
            cancellation.checkpoint()?;
        }
        let byte = *byte;
        if escaped {
            escaped = false;
            continue;
        }
        if byte == b'\\' {
            escaped = true;
            continue;
        }
        match byte {
            b'[' => {
                if open.is_none() {
                    open = Some(index);
                    depth = 1;
                } else {
                    depth = depth.saturating_add(1);
                }
            }
            b']' if open.is_some() => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    if let Some(command) = command_tail(source, open.unwrap_or(index), index) {
                        commands.push(command);
                    }
                    open = None;
                }
            }
            _ => {}
        }
    }
    Ok(commands)
}

fn command_tail(source: &str, open: usize, close: usize) -> Option<(String, TextRange)> {
    let content_start = open.checked_add(1)?;
    let mut content_end = close;
    if let Some(format_offset) = source.get(content_start..content_end)?.find('|') {
        content_end = content_start.checked_add(format_offset)?;
    }
    while content_end > content_start && source.as_bytes()[content_end - 1].is_ascii_whitespace() {
        content_end -= 1;
    }
    let segment_start = source
        .get(content_start..content_end)?
        .rfind('.')
        .map_or(content_start, |relative| content_start + relative + 1);
    let mut segment_start = segment_start;
    while segment_start < content_end && source.as_bytes()[segment_start].is_ascii_whitespace() {
        segment_start += 1;
    }
    while content_end > segment_start && source.as_bytes()[content_end - 1].is_ascii_whitespace() {
        content_end -= 1;
    }
    if segment_start >= content_end {
        return None;
    }
    let command = source.get(segment_start..content_end)?.to_owned();
    Some((
        command,
        TextRange::new(
            u32::try_from(segment_start).ok()?,
            u32::try_from(content_end).ok()?,
        )?,
    ))
}

fn localisation_command_is_bypassed(command: &str) -> bool {
    command.is_empty()
        || command.starts_with('?')
        || command.starts_with('$')
        || command.contains('$')
        || command.contains(':')
        || command.parse::<f64>().is_ok()
}

#[inline]
fn name_key(name: &str) -> String {
    name.to_ascii_lowercase()
}

fn escaped_at(source: &str, offset: usize) -> bool {
    let mut slashes = 0usize;
    let bytes = source.as_bytes();
    let mut index = offset;
    while index > 0 && bytes[index - 1] == b'\\' {
        slashes += 1;
        index -= 1;
    }
    slashes % 2 == 1
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
/// of the highest layer — the game's later-load override semantics
/// (`docs/eu4-cjk-localisation-design.md` §7.2). The hover renders the
/// languages in parallel instead of a single first-found value.
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

#[cfg(test)]
mod tests {
    use super::{LocalisationCommandRegistry, name_key};

    #[test]
    fn command_registry_uses_case_insensitive_lookup() {
        let names = vec!["GetName".to_owned(), "Scripted.One".to_owned()];
        let lookup = names.iter().map(|name| name_key(name)).collect();
        let registry = LocalisationCommandRegistry {
            names,
            lookup,
            has_scripted_localisations: true,
        };
        assert!(registry.contains("getname"));
        assert!(registry.contains("SCRIPTED.ONE"));
        assert!(!registry.contains("Scripted.Two"));
    }
}
