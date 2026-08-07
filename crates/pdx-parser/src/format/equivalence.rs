use super::common::TextEdit;
use super::script::{decode_payload, quoted_payload, quoted_script};
use crate::{CstNode, FileFormat, ParsedFile, TokenKind, parse};
use pdx_text::TextRange;
pub(super) fn equivalent(original: &ParsedFile, formatted: &ParsedFile, depth: usize) -> bool {
    if original.format() != formatted.format()
        || !formatted.errors().is_empty()
        || !same_tree_shape(original.root(), formatted.root())
        || original.tokens().len() != formatted.tokens().len()
    {
        return false;
    }
    original
        .tokens()
        .iter()
        .zip(formatted.tokens())
        .all(|(before, after)| {
            if before.kind() != after.kind() {
                return false;
            }
            let Some(before_text) = original.text(before.range()) else {
                return false;
            };
            let Some(after_text) = formatted.text(after.range()) else {
                return false;
            };
            if before.kind() == TokenKind::Quoted
                && let Some(before_script) = quoted_script(before_text, depth)
            {
                let Some(after_payload) = quoted_payload(after_text).and_then(decode_payload)
                else {
                    return false;
                };
                let after_script = parse(FileFormat::Script, &after_payload);
                return equivalent(
                    &before_script.parsed,
                    &after_script,
                    depth.saturating_add(1),
                );
            }
            before_text == after_text
        })
}

fn same_tree_shape(left: &CstNode, right: &CstNode) -> bool {
    left.kind() == right.kind()
        && left.children().len() == right.children().len()
        && left
            .children()
            .iter()
            .zip(right.children())
            .all(|(left, right)| same_tree_shape(left, right))
}

pub(super) fn minimal_edits(
    original: &ParsedFile,
    formatted: &ParsedFile,
) -> Option<Vec<TextEdit>> {
    if original.tokens().len() != formatted.tokens().len() {
        return None;
    }
    let mut edits = Vec::new();
    let mut before_end = 0_usize;
    let mut after_end = 0_usize;
    for (before, after) in original.tokens().iter().zip(formatted.tokens()) {
        if before.kind() != after.kind() {
            return None;
        }
        let before_start = usize::try_from(before.range().start()).ok()?;
        let after_start = usize::try_from(after.range().start()).ok()?;
        push_changed_range(
            &mut edits,
            original.source(),
            before_end,
            before_start,
            formatted.source().get(after_end..after_start)?,
        )?;

        let before_text = original.text(before.range())?;
        let after_text = formatted.text(after.range())?;
        if before_text != after_text {
            if before.kind() != TokenKind::Quoted || quoted_script(before_text, 0).is_none() {
                return None;
            }
            push_minimal_token_edit(&mut edits, before_text, after_text, before_start)?;
        }
        before_end = usize::try_from(before.range().end()).ok()?;
        after_end = usize::try_from(after.range().end()).ok()?;
    }
    push_changed_range(
        &mut edits,
        original.source(),
        before_end,
        original.source().len(),
        formatted.source().get(after_end..)?,
    )?;
    Some(edits)
}

fn push_changed_range(
    edits: &mut Vec<TextEdit>,
    source: &str,
    start: usize,
    end: usize,
    replacement: &str,
) -> Option<()> {
    if source.get(start..end)? == replacement {
        return Some(());
    }
    edits.push(TextEdit {
        range: text_range(start, end)?,
        replacement: replacement.to_owned(),
    });
    Some(())
}

fn push_minimal_token_edit(
    edits: &mut Vec<TextEdit>,
    before: &str,
    after: &str,
    absolute_start: usize,
) -> Option<()> {
    let mut prefix = before
        .bytes()
        .zip(after.bytes())
        .take_while(|(left, right)| left == right)
        .count();
    while prefix > 0 && (!before.is_char_boundary(prefix) || !after.is_char_boundary(prefix)) {
        prefix -= 1;
    }

    let max_suffix = before.len().min(after.len()).saturating_sub(prefix);
    let mut suffix = before
        .bytes()
        .rev()
        .zip(after.bytes().rev())
        .take(max_suffix)
        .take_while(|(left, right)| left == right)
        .count();
    while suffix > 0
        && (!before.is_char_boundary(before.len() - suffix)
            || !after.is_char_boundary(after.len() - suffix))
    {
        suffix -= 1;
    }

    let start = absolute_start.checked_add(prefix)?;
    let end = absolute_start.checked_add(before.len().saturating_sub(suffix))?;
    let replacement_end = after.len().saturating_sub(suffix);
    edits.push(TextEdit {
        range: text_range(start, end)?,
        replacement: after.get(prefix..replacement_end)?.to_owned(),
    });
    Some(())
}

fn text_range(start: usize, end: usize) -> Option<TextRange> {
    TextRange::new(u32::try_from(start).ok()?, u32::try_from(end).ok()?)
}
