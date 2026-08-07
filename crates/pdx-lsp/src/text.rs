use std::fs;
use std::path::PathBuf;

use lsp_types::Range as LspRange;
use pdx_text::{LineIndex, Position, TextRange};

use crate::protocol::RpcError;
use crate::{INVALID_PARAMS, MAX_DOCUMENT_BYTES};

pub(crate) fn normalize_workspace_path(path: PathBuf) -> PathBuf {
    if let Ok(canonical) = fs::canonicalize(&path) {
        return canonical;
    }

    let mut ancestor = path.as_path();
    let mut missing = Vec::new();
    while let Some(name) = ancestor.file_name() {
        missing.push(name.to_owned());
        let Some(parent) = ancestor.parent() else {
            break;
        };
        ancestor = parent;
        if let Ok(mut canonical) = fs::canonicalize(ancestor) {
            for component in missing.iter().rev() {
                canonical.push(component);
            }
            return canonical;
        }
    }
    path
}

pub(crate) fn lsp_range_to_text_range(
    range: &LspRange,
    index: &LineIndex,
    text: &str,
) -> Result<TextRange, RpcError> {
    let start = Position::new(range.start.line, range.start.character);
    let end = Position::new(range.end.line, range.end.character);
    let start = index.offset(text, start).ok_or_else(|| {
        RpcError::new(INVALID_PARAMS, "range start is not a valid UTF-16 position")
    })?;
    let end = index
        .offset(text, end)
        .ok_or_else(|| RpcError::new(INVALID_PARAMS, "range end is not a valid UTF-16 position"))?;
    TextRange::new(start, end)
        .ok_or_else(|| RpcError::new(INVALID_PARAMS, "range end precedes start"))
}

pub(crate) fn apply_text_change(
    text: &mut String,
    range: Option<TextRange>,
    replacement: &str,
) -> Result<(), RpcError> {
    if let Some(range) = range {
        let start = usize::try_from(range.start())
            .map_err(|_| RpcError::new(INVALID_PARAMS, "range is too large"))?;
        let end = usize::try_from(range.end())
            .map_err(|_| RpcError::new(INVALID_PARAMS, "range is too large"))?;
        if text.get(start..end).is_none() {
            return Err(RpcError::new(
                INVALID_PARAMS,
                "range is outside the document",
            ));
        }
        text.replace_range(start..end, replacement);
    } else {
        text.clear();
        text.push_str(replacement);
    }
    Ok(())
}

pub(crate) fn changed_document_len(
    current_len: usize,
    range: Option<TextRange>,
    replacement_len: usize,
) -> Result<usize, RpcError> {
    let removed = range.map_or(current_len, |range| {
        usize::try_from(range.len()).unwrap_or(usize::MAX)
    });
    let next = current_len
        .checked_sub(removed)
        .and_then(|length| length.checked_add(replacement_len))
        .ok_or_else(|| RpcError::new(INVALID_PARAMS, "document change has an invalid size"))?;
    if next > MAX_DOCUMENT_BYTES {
        return Err(RpcError::new(
            INVALID_PARAMS,
            format!("document exceeds the {MAX_DOCUMENT_BYTES}-byte safety limit"),
        ));
    }
    Ok(next)
}
