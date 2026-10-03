use std::cmp::Ordering;

use crate::support::*;
use parser::FileFormat;
use text::TextSize;

pub(crate) fn completion_value_context(input: &ParsedInput, position: TextSize) -> bool {
    if input.format == FileFormat::Script
        && let Some(hir) = input.hir.as_deref()
    {
        if hir.properties().iter().any(|property| {
            position >= property.key_range.start() && position <= property.key_range.end()
        }) {
            return false;
        }
        if hir.properties().iter().any(|property| {
            property.scalar.as_ref().is_some_and(|scalar| {
                position >= scalar.range.start() && position <= scalar.range.end()
            })
        }) {
            return true;
        }
    }
    let offset = usize::try_from(position)
        .unwrap_or(input.source.len())
        .min(input.source.len());
    let line_start = input.source[..offset]
        .rfind('\n')
        .map_or(0, |index| index + 1);
    let line = &input.source[line_start..offset];
    let equals = line.rfind('=');
    let open = line.rfind('{');
    equals.is_some_and(|equals| open.is_none_or(|open| equals > open))
}

/// Compares labels using case-insensitive lexical order, preferring the lowercase spelling when
/// two labels differ only by ASCII case (`a < A < b < B`).
pub(crate) fn completion_label_cmp(left: &str, right: &str) -> Ordering {
    left.to_ascii_lowercase()
        .cmp(&right.to_ascii_lowercase())
        .then_with(|| {
            left.as_bytes()
                .iter()
                .zip(right.as_bytes())
                .find_map(|(&left_byte, &right_byte)| {
                    if left_byte == right_byte {
                        return None;
                    }
                    if left_byte.is_ascii_lowercase()
                        && left_byte.to_ascii_uppercase() == right_byte
                    {
                        Some(Ordering::Less)
                    } else if left_byte.is_ascii_uppercase()
                        && left_byte.to_ascii_lowercase() == right_byte
                    {
                        Some(Ordering::Greater)
                    } else {
                        Some(left_byte.cmp(&right_byte))
                    }
                })
                .unwrap_or_else(|| left.len().cmp(&right.len()))
        })
}

#[cfg(test)]
mod tests {
    use super::completion_label_cmp;

    #[test]
    fn completion_labels_prefer_lowercase_when_case_insensitive_names_tie() {
        let mut labels = vec!["B", "a", "A", "b"];
        labels.sort_by(|left, right| completion_label_cmp(left, right));
        assert_eq!(labels, ["a", "A", "b", "B"]);
    }
}
