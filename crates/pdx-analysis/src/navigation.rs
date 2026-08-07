use crate::resolution::*;
use crate::support::*;
use crate::types::*;
use pdx_engine::{AnalysisSnapshot, DocumentId, DocumentSource};
use pdx_rules::SymbolResolutionPolicy;
use pdx_text::TextSize;

/// Resolves the symbol at a position. Ambiguous and unresolved references deliberately return no
/// location so a client can never be sent to an arbitrary candidate.
#[must_use]
pub fn definition(
    snapshot: &AnalysisSnapshot,
    document: &DocumentId,
    position: TextSize,
) -> Vec<Location> {
    uncancelled(definition_with_cancellation(
        snapshot,
        document,
        position,
        &CancellationToken::new(),
    ))
}

/// Resolves a definition with cooperative cancellation checkpoints.
pub fn definition_with_cancellation(
    snapshot: &AnalysisSnapshot,
    document: &DocumentId,
    position: TextSize,
    cancellation: &CancellationToken,
) -> Result<Vec<Location>, Cancelled> {
    cancellation.checkpoint()?;
    let Some(input) = input_for_document(snapshot, document) else {
        return Ok(Vec::new());
    };
    if let Some((definition, _)) = local_parameter_target(&input, position) {
        return Ok(vec![local_location(&input, definition.name_range)]);
    }
    let all = all_semantics(snapshot, cancellation)?;
    let Some((kind, name)) = symbol_at(&all, document, position) else {
        return Ok(Vec::new());
    };
    Ok(match resolve_symbol(snapshot, &all, &kind, &name) {
        Resolution::Unique(definition) => vec![definition_selection_location(&definition)],
        Resolution::Ambiguous | Resolution::Missing => Vec::new(),
    })
}

/// Returns resolved references for the symbol at a position.
#[must_use]
pub fn references(
    snapshot: &AnalysisSnapshot,
    document: &DocumentId,
    position: TextSize,
    include_declaration: bool,
) -> Vec<Location> {
    uncancelled(references_with_cancellation(
        snapshot,
        document,
        position,
        include_declaration,
        &CancellationToken::new(),
    ))
}

/// Resolves references with cooperative cancellation checkpoints.
pub fn references_with_cancellation(
    snapshot: &AnalysisSnapshot,
    document: &DocumentId,
    position: TextSize,
    include_declaration: bool,
    cancellation: &CancellationToken,
) -> Result<Vec<Location>, Cancelled> {
    cancellation.checkpoint()?;
    let Some(input) = input_for_document(snapshot, document) else {
        return Ok(Vec::new());
    };
    if let Some((definition, _)) = local_parameter_target(&input, position) {
        let Some(hir) = input.hir.as_deref() else {
            return Ok(Vec::new());
        };
        let mut result = Vec::new();
        if include_declaration {
            result.push(local_location(&input, definition.name_range));
        }
        result.extend(
            hir.parameter_references_for_owner(definition.owner_range)
                .filter(|reference| {
                    reference.name.eq_ignore_ascii_case(&definition.name)
                        && reference.name_range != definition.name_range
                })
                .map(|reference| local_location(&input, reference.name_range)),
        );
        return Ok(result);
    }
    let all = all_semantics(snapshot, cancellation)?;
    let Some((kind, name)) = symbol_at(&all, document, position) else {
        return Ok(Vec::new());
    };
    let Resolution::Unique(target) = resolve_symbol(snapshot, &all, &kind, &name) else {
        return Ok(Vec::new());
    };
    let mut result = Vec::new();
    if include_declaration {
        result.push(definition_selection_location(&target));
    }
    for reference in &all.references {
        cancellation.checkpoint()?;
        if reference.kind != kind || !same_name(&reference.name, &name) {
            continue;
        }
        if let Resolution::Unique(candidate) =
            resolve_symbol(snapshot, &all, &kind, &reference.name)
            && same_location(&candidate.location, &target.location)
        {
            result.push(reference.location());
        }
    }
    result.sort_by_key(|location| {
        (
            location
                .path
                .as_ref()
                .map_or(String::new(), |path| path.as_str().to_owned()),
            location.range.start(),
        )
    });
    result.dedup();
    cancellation.checkpoint()?;
    Ok(result)
}

/// Returns the identifier range when the cursor is on a uniquely resolved, writable symbol.
pub fn prepare_rename(
    snapshot: &AnalysisSnapshot,
    document: &DocumentId,
    position: TextSize,
) -> Result<PrepareRenameResult, RenameError> {
    match prepare_rename_with_cancellation(snapshot, document, position, &CancellationToken::new())
    {
        Ok(result) => Ok(result),
        Err(RenameFailure::Rejected(error)) => Err(error),
        Err(RenameFailure::Cancelled) => {
            unreachable!("a fresh cancellation token cannot be cancelled")
        }
    }
}

/// Prepares a rename while allowing the caller to cancel semantic resolution.
pub fn prepare_rename_with_cancellation(
    snapshot: &AnalysisSnapshot,
    document: &DocumentId,
    position: TextSize,
    cancellation: &CancellationToken,
) -> Result<PrepareRenameResult, RenameFailure> {
    cancellation
        .checkpoint()
        .map_err(|Cancelled| RenameFailure::Cancelled)?;
    let input = input_for_document(snapshot, document).ok_or(RenameError::NoSymbol)?;
    if let Some((_, reference)) = local_parameter_target(&input, position) {
        if !writable_location(snapshot, &local_location(&input, reference.name_range)) {
            return Err(RenameError::ReadOnly.into());
        }
        return Ok(PrepareRenameResult {
            range: reference.name_range,
            placeholder: reference.name.clone(),
        });
    }
    let target = rename_target(snapshot, document, position, cancellation)?;
    let placeholder = input
        .source_text(target.cursor_range)
        .ok_or(RenameError::NoSymbol)?
        .to_owned();
    Ok(PrepareRenameResult {
        range: target.cursor_range,
        placeholder,
    })
}

/// Builds a safe, editor-neutral WorkspaceEdit for a semantic rename.
pub fn rename(
    snapshot: &AnalysisSnapshot,
    document: &DocumentId,
    position: TextSize,
    new_name: &str,
) -> Result<WorkspaceEditPlan, RenameError> {
    match rename_with_cancellation(
        snapshot,
        document,
        position,
        new_name,
        &CancellationToken::new(),
    ) {
        Ok(result) => Ok(result),
        Err(RenameFailure::Rejected(error)) => Err(error),
        Err(RenameFailure::Cancelled) => {
            unreachable!("a fresh cancellation token cannot be cancelled")
        }
    }
}

/// Builds a rename plan with cooperative cancellation checkpoints.
pub fn rename_with_cancellation(
    snapshot: &AnalysisSnapshot,
    document: &DocumentId,
    position: TextSize,
    new_name: &str,
    cancellation: &CancellationToken,
) -> Result<WorkspaceEditPlan, RenameFailure> {
    cancellation
        .checkpoint()
        .map_err(|Cancelled| RenameFailure::Cancelled)?;
    if !valid_rename_name(new_name) {
        return Err(RenameError::InvalidName.into());
    }
    let input = input_for_document(snapshot, document).ok_or(RenameError::NoSymbol)?;
    if let Some((definition, _)) = local_parameter_target(&input, position) {
        if !valid_parameter_name(new_name) {
            return Err(RenameError::InvalidName.into());
        }
        if !writable_location(snapshot, &local_location(&input, definition.name_range)) {
            return Err(RenameError::ReadOnly.into());
        }
        let Some(hir) = input.hir.as_deref() else {
            return Err(RenameError::NoSymbol.into());
        };
        if hir
            .parameter_definitions_for_owner(definition.owner_range)
            .any(|candidate| {
                candidate.name_range != definition.name_range
                    && candidate.name.eq_ignore_ascii_case(new_name)
            })
        {
            return Err(RenameError::Conflict.into());
        }
        let mut edits = Vec::new();
        for reference in hir
            .parameter_references_for_owner(definition.owner_range)
            .filter(|reference| reference.name.eq_ignore_ascii_case(&definition.name))
        {
            cancellation
                .checkpoint()
                .map_err(|Cancelled| RenameFailure::Cancelled)?;
            edits.push(WorkspaceTextEdit {
                location: local_location(&input, reference.name_range),
                new_text: new_name.to_owned(),
            });
        }
        edits.sort_by(|left, right| {
            right
                .location
                .range
                .start()
                .cmp(&left.location.range.start())
                .then_with(|| right.location.range.end().cmp(&left.location.range.end()))
        });
        edits.dedup_by(|left, right| left.location == right.location);
        return Ok(WorkspaceEditPlan {
            revision: snapshot.revision(),
            edits,
        });
    }
    let target = rename_target(snapshot, document, position, cancellation)?;
    let all =
        all_semantics(snapshot, cancellation).map_err(|Cancelled| RenameFailure::Cancelled)?;
    check_rename_conflict(snapshot, &all, &target, new_name, cancellation)?;

    let mut edits = vec![WorkspaceTextEdit {
        location: Location {
            range: target.definition.selection_range,
            ..target.definition.location.clone()
        },
        new_text: new_name.to_owned(),
    }];
    let overlay_files = overlay_file_ids(snapshot);
    for reference in &all.references {
        cancellation
            .checkpoint()
            .map_err(|Cancelled| RenameFailure::Cancelled)?;
        if reference.kind != target.kind || !same_name(&reference.name, &target.name) {
            continue;
        }
        // A document overlay replaces its disk candidate.  Do not return edits for the hidden
        // disk text as that would overwrite user changes when the client applies the WorkspaceEdit.
        if reference.document.is_none()
            && reference
                .file
                .is_some_and(|file| overlay_files.contains(&file))
        {
            continue;
        }
        let Resolution::Unique(candidate) =
            resolve_symbol(snapshot, &all, &target.kind, &reference.name)
        else {
            continue;
        };
        if !same_location(&candidate.location, &target.definition.location)
            || !writable_location(snapshot, &reference.location())
        {
            continue;
        }
        edits.push(WorkspaceTextEdit {
            location: reference.location(),
            new_text: new_name.to_owned(),
        });
    }
    edits.sort_by(|left, right| {
        edit_target_key(&left.location)
            .cmp(&edit_target_key(&right.location))
            .then_with(|| {
                right
                    .location
                    .range
                    .start()
                    .cmp(&left.location.range.start())
            })
            .then_with(|| right.location.range.end().cmp(&left.location.range.end()))
    });
    edits
        .dedup_by(|left, right| left.location == right.location && left.new_text == right.new_text);
    cancellation
        .checkpoint()
        .map_err(|Cancelled| RenameFailure::Cancelled)?;
    Ok(WorkspaceEditPlan {
        revision: snapshot.revision(),
        edits,
    })
}

/// Returns symbols declared by one document.
#[must_use]
pub fn document_symbols(snapshot: &AnalysisSnapshot, document: &DocumentId) -> Vec<Symbol> {
    uncancelled(document_symbols_with_cancellation(
        snapshot,
        document,
        &CancellationToken::new(),
    ))
}

/// Returns document symbols with cooperative cancellation checkpoints.
pub fn document_symbols_with_cancellation(
    snapshot: &AnalysisSnapshot,
    document: &DocumentId,
    cancellation: &CancellationToken,
) -> Result<Vec<Symbol>, Cancelled> {
    cancellation.checkpoint()?;
    let Some(input) = input_for_document(snapshot, document) else {
        return Ok(Vec::new());
    };
    let data = semantic_data(snapshot, &input);
    let parameter_count = input
        .hir
        .as_deref()
        .map_or(0, |hir| hir.parameter_definitions().len());
    let mut result = Vec::with_capacity(data.definitions.len() + parameter_count);
    for definition in data.definitions {
        cancellation.checkpoint()?;
        result.push(definition.symbol);
    }
    if let Some(hir) = input.hir.as_deref() {
        for definition in hir.parameter_definitions() {
            cancellation.checkpoint()?;
            result.push(Symbol {
                name: definition.name.clone(),
                kind: "parameter".to_owned(),
                range: definition.range,
                selection_range: definition.name_range,
                location: local_location(&input, definition.range),
            });
        }
    }
    result.sort_by(|left, right| {
        left.range
            .start()
            .cmp(&right.range.start())
            .then_with(|| left.range.end().cmp(&right.range.end()))
            .then_with(|| left.kind.cmp(&right.kind))
            .then_with(|| left.name.cmp(&right.name))
    });
    Ok(result)
}

/// Returns active workspace symbols using deterministic prefix/fuzzy ranking.
#[must_use]
pub fn workspace_symbols(snapshot: &AnalysisSnapshot, query: &str) -> Vec<WorkspaceSymbol> {
    uncancelled(workspace_symbols_with_cancellation(
        snapshot,
        query,
        &CancellationToken::new(),
    ))
}

/// Returns workspace symbols with cooperative cancellation checkpoints.
pub fn workspace_symbols_with_cancellation(
    snapshot: &AnalysisSnapshot,
    query: &str,
    cancellation: &CancellationToken,
) -> Result<Vec<WorkspaceSymbol>, Cancelled> {
    let all = all_semantics(snapshot, cancellation)?;
    let query = query.trim().to_ascii_lowercase();
    let mut result = Vec::new();
    for definition in &all.definitions {
        cancellation.checkpoint()?;
        let name = definition.name.to_ascii_lowercase();
        let score = if query.is_empty() {
            Some(20)
        } else if name.starts_with(&query) {
            Some(0)
        } else if name.contains(&query) {
            Some(10)
        } else if fuzzy_match(&name, &query) {
            Some(30)
        } else {
            None
        };
        if score.is_none() {
            continue;
        }
        if let Resolution::Unique(active) =
            resolve_symbol(snapshot, &all, &definition.kind, &definition.name)
            && same_location(&active.location, &definition.symbol.location)
        {
            result.push((score.unwrap_or(99), definition.symbol.clone()));
        }
    }
    result.sort_by_key(|(score, symbol)| {
        (
            *score,
            symbol.name.to_ascii_lowercase(),
            symbol.kind.clone(),
        )
    });
    cancellation.checkpoint()?;
    Ok(result.into_iter().map(|(_, symbol)| symbol).collect())
}
pub(crate) fn rename_target(
    snapshot: &AnalysisSnapshot,
    document: &DocumentId,
    position: TextSize,
    cancellation: &CancellationToken,
) -> Result<RenameTarget, RenameFailure> {
    cancellation
        .checkpoint()
        .map_err(|Cancelled| RenameFailure::Cancelled)?;
    let input = input_for_document(snapshot, document).ok_or(RenameError::NoSymbol)?;
    let all =
        all_semantics(snapshot, cancellation).map_err(|Cancelled| RenameFailure::Cancelled)?;
    let Some((kind, name)) = symbol_at(&all, document, position) else {
        return Err(RenameError::NoSymbol.into());
    };
    let definition = match resolve_symbol(snapshot, &all, &kind, &name) {
        Resolution::Unique(definition) => definition,
        Resolution::Ambiguous => return Err(RenameError::Ambiguous.into()),
        Resolution::Missing => return Err(RenameError::Unresolved.into()),
    };
    if !writable_location(snapshot, &definition.location) {
        return Err(RenameError::ReadOnly.into());
    }
    Ok(RenameTarget {
        kind,
        name,
        cursor_range: word_range(&input.source, position),
        definition,
    })
}

pub(crate) fn check_rename_conflict(
    snapshot: &AnalysisSnapshot,
    all: &SemanticWorkspace,
    target: &RenameTarget,
    new_name: &str,
    cancellation: &CancellationToken,
) -> Result<(), RenameFailure> {
    let policy = snapshot
        .rules()
        .model()
        .symbol_descriptors
        .iter()
        .find(|descriptor| descriptor.kind_id.eq_ignore_ascii_case(&target.kind))
        .map_or(SymbolResolutionPolicy::ReplaceBySymbol, |descriptor| {
            descriptor.resolution
        });
    for definition in &all.definitions {
        cancellation
            .checkpoint()
            .map_err(|Cancelled| RenameFailure::Cancelled)?;
        if definition.kind != target.kind || !same_name(&definition.name, new_name) {
            continue;
        }
        if same_location(&definition.symbol.location, &target.definition.location) {
            continue;
        }
        let priority = definition_priority(snapshot, definition);
        let conflict = match policy {
            SymbolResolutionPolicy::Merge | SymbolResolutionPolicy::Unique => true,
            SymbolResolutionPolicy::ReplaceBySymbol => priority >= target.definition.priority,
        };
        if conflict {
            return Err(RenameError::Conflict.into());
        }
    }
    Ok(())
}

pub(crate) fn valid_rename_name(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(is_word_byte)
}

pub(crate) fn valid_parameter_name(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|byte| byte != b'$' && is_word_byte(byte))
}

pub(crate) fn writable_location(snapshot: &AnalysisSnapshot, location: &Location) -> bool {
    if let Some(file) = location.file
        && let Some(source_file) = snapshot.source_files().get(&file)
    {
        return snapshot
            .source_roots()
            .iter()
            .find(|root| root.id == source_file.root_id)
            .is_some_and(|root| matches!(root.kind, pdx_engine::SourceRootKind::CurrentMod));
    }
    if let Some(document_id) = location.document.as_ref()
        && let Some(document) = snapshot.document(document_id)
    {
        if document.source() != DocumentSource::Overlay {
            return false;
        }
        return document.path().is_none_or(|path| {
            root_for_path(snapshot, path)
                .is_some_and(|root| matches!(root.kind, pdx_engine::SourceRootKind::CurrentMod))
        });
    }
    false
}
pub(crate) fn edit_target_key(location: &Location) -> (u8, String) {
    if let Some(document) = location.document.as_ref() {
        return (0, document.as_str().to_owned());
    }
    if let Some(file) = location.file {
        return (1, file.get().to_string());
    }
    (
        2,
        location
            .path
            .as_ref()
            .map_or_else(String::new, |path| path.as_str().to_owned()),
    )
}

pub(crate) fn fuzzy_match(value: &str, query: &str) -> bool {
    let mut chars = value.chars();
    query
        .chars()
        .all(|wanted| chars.by_ref().any(|actual| actual == wanted))
}
