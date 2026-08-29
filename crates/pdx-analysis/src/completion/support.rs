use std::cmp::Ordering;
use std::collections::BTreeMap;

use crate::resolution::*;
use crate::semantic::{
    completion_overlay_allowed, completion_source_file_allowed, localisation_key_index,
};
use crate::support::*;
use crate::types::*;
use pdx_engine::{AnalysisSnapshot, DocumentSource};
use pdx_parser::FileFormat;
use pdx_text::TextSize;

pub(crate) fn completion_definitions_for_kinds(
    snapshot: &AnalysisSnapshot,
    prefix: &str,
    kinds: &[&str],
    cancellation: &CancellationToken,
) -> Result<Vec<(String, String)>, Cancelled> {
    // Localisation is the one definition family that can grow to hundreds of thousands of
    // entries once Vanilla is installed.  Its snapshot-owned compact index answers the same
    // prefix/substring query without walking every definition bucket on each keystroke.
    if kinds.len() == 1 && kinds[0].eq_ignore_ascii_case("localisation") {
        return localisation_key_index(snapshot)
            .select_with_cancellation(prefix, cancellation)
            .map(|names| {
                names
                    .into_iter()
                    .map(|name| ("localisation".to_owned(), name))
                    .collect()
            });
    }
    let mut definitions = Vec::new();
    for kind in kinds {
        for definition in snapshot.index().definitions_for_kind(kind) {
            cancellation.checkpoint()?;
            if completion_source_file_allowed(snapshot, definition.file_id)
                && completion_matches(&definition.name, prefix)
            {
                definitions.push((definition.kind.clone(), definition.name.clone()));
            }
        }
    }
    for document in snapshot.documents().values() {
        cancellation.checkpoint()?;
        if document.source() != DocumentSource::Overlay || !completion_overlay_allowed(snapshot) {
            continue;
        }
        if let Some(input) = input_for_document(snapshot, document.id()) {
            definitions.extend(
                semantic_data(snapshot, &input)
                    .definitions
                    .into_iter()
                    .filter(|definition| {
                        kinds
                            .iter()
                            .any(|kind| definition.kind.eq_ignore_ascii_case(kind))
                            && completion_matches(&definition.name, prefix)
                    })
                    .map(|definition| (definition.kind, definition.name)),
            );
        }
    }
    definitions.sort_by(|left, right| {
        (left.0.to_ascii_lowercase(), left.1.to_ascii_lowercase())
            .cmp(&(right.0.to_ascii_lowercase(), right.1.to_ascii_lowercase()))
    });
    definitions.dedup_by(|left, right| {
        left.0.eq_ignore_ascii_case(&right.0) && left.1.eq_ignore_ascii_case(&right.1)
    });
    Ok(definitions)
}
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
    if input.format == FileFormat::Localisation {
        return line.contains(':') && !line.trim_start().starts_with('#');
    }
    let equals = line.rfind('=');
    let open = line.rfind('{');
    equals.is_some_and(|equals| open.is_none_or(|open| equals > open))
}

pub(crate) fn localisation_language_header(input: &ParsedInput, position: TextSize) -> bool {
    if input.format != FileFormat::Localisation {
        return false;
    }
    let offset = usize::try_from(position)
        .unwrap_or(input.source.len())
        .min(input.source.len());
    let line_start = input.source[..offset]
        .rfind('\n')
        .map_or(0, |index| index + 1);
    let line = input.source[line_start..offset].trim();
    let Some(language) = line.strip_prefix("l_") else {
        return false;
    };
    let Some(language) = language.strip_suffix(':') else {
        return false;
    };
    !language.is_empty()
        && language
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
}

/// Match quality is one component of the completion rank.  Schema provenance is intentionally
/// compared before it so explicit members of a parent block remain above broad child-context
/// candidates such as `effect` commands.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum CompletionMatchQuality {
    Exact,
    ExactIgnoreCase,
    CaseSensitivePrefix,
    CaseInsensitivePrefix,
    SegmentPrefix,
    Substring,
}

/// Describes where a candidate came from.  Explicit members of the enclosing block outrank a
/// broad child context such as `effect`; this is what keeps `event.option` members above effect
/// commands without hard-coding that EU4 construct in the ranking layer.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum CompletionSchemaTier {
    /// A value inferred from every active scripted-macro use site.
    MacroInferred,
    /// A rule attached to the explicit parent path of the current block.
    ExplicitParentMember,
    /// A rule from the current semantic context, such as `effect` or `trigger`.
    CurrentContext,
    /// A rule reachable only through an ambiguous alternative context.
    Alternative,
}

/// Semantic specificity is the rank component after the schema source.  It is intentionally
/// independent of the LSP completion kind so callers can distinguish, for example, an exact
/// literal from an arbitrary dynamic value while retaining the same protocol icon.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum CompletionSpecificity {
    Exact,
    Enum,
    ScriptedMacro,
    Type,
    Value,
    Localisation,
    Dynamic,
    Scope,
    Fallback,
}

/// Semantic rank inputs shared by key, value, and macro candidate builders.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CompletionRankContext {
    pub(crate) schema_tier: CompletionSchemaTier,
    pub(crate) specificity: CompletionSpecificity,
    /// Whether this candidate represents a required rule that is still missing in the current
    /// block.  Required candidates sort before optional candidates within the same schema tier.
    pub(crate) required: bool,
    pub(crate) deprecated: bool,
    /// Number of scope-link hops, when the candidate is a scope expression.
    pub(crate) scope_distance: u8,
}

impl CompletionRankContext {
    pub(crate) const fn new(
        schema_tier: CompletionSchemaTier,
        specificity: CompletionSpecificity,
        required: bool,
        deprecated: bool,
    ) -> Self {
        Self {
            schema_tier,
            specificity,
            required,
            deprecated,
            scope_distance: 0,
        }
    }

    pub(crate) const fn with_scope_distance(mut self, scope_distance: u8) -> Self {
        self.scope_distance = scope_distance;
        self
    }
}

/// Complete rank used before the editor-neutral `CompletionItem` is returned.  It stays
/// analysis-internal so the public DTO remains compatible with existing adapters.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct CompletionRank {
    pub(crate) schema_tier: CompletionSchemaTier,
    pub(crate) match_quality: CompletionMatchQuality,
    pub(crate) required_penalty: u8,
    pub(crate) specificity: CompletionSpecificity,
    pub(crate) scope_distance: u8,
    pub(crate) deprecated: bool,
}

impl CompletionRank {
    /// Encodes the lexicographic rank into the legacy `sort_score` field.  Each component has a
    /// fixed-width range, so the score is safe for LSP `sortText` and future dimensions cannot
    /// accidentally cross the match-quality boundary.
    pub(crate) fn sort_score(self) -> u32 {
        const SCHEMA_WEIGHT: u32 = 10_000_000;
        const MATCH_WEIGHT: u32 = 1_000_000;
        const REQUIRED_WEIGHT: u32 = 10_000;
        const SPECIFICITY_WEIGHT: u32 = 1_000;
        const SCOPE_WEIGHT: u32 = 10;
        u32::from(self.schema_tier as u8) * SCHEMA_WEIGHT
            + u32::from(self.match_quality as u8) * MATCH_WEIGHT
            + u32::from(self.required_penalty) * REQUIRED_WEIGHT
            + u32::from(self.specificity as u8) * SPECIFICITY_WEIGHT
            + u32::from(self.scope_distance.min(99)) * SCOPE_WEIGHT
            + u32::from(self.deprecated)
    }
}

/// A completion item paired with its analysis-only rank.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RankedCompletionItem {
    pub(crate) item: CompletionItem,
    pub(crate) rank: CompletionRank,
}

fn segment_prefix_match(label: &str, prefix: &str) -> bool {
    label
        .split(['_', '-', '.'])
        .skip(1)
        .any(|segment| starts_with_ignore_ascii_case(segment, prefix))
}

fn completion_match_quality(label: &str, prefix: &str) -> Option<CompletionMatchQuality> {
    if prefix.is_empty() {
        return Some(CompletionMatchQuality::CaseSensitivePrefix);
    }
    if label == prefix {
        return Some(CompletionMatchQuality::Exact);
    }
    if label.eq_ignore_ascii_case(prefix) {
        return Some(CompletionMatchQuality::ExactIgnoreCase);
    }
    if label.starts_with(prefix) {
        return Some(CompletionMatchQuality::CaseSensitivePrefix);
    }
    if starts_with_ignore_ascii_case(label, prefix) {
        return Some(CompletionMatchQuality::CaseInsensitivePrefix);
    }
    if segment_prefix_match(label, prefix) {
        return Some(CompletionMatchQuality::SegmentPrefix);
    }
    completion_matches(label, prefix).then_some(CompletionMatchQuality::Substring)
}

pub(crate) fn push_completion(
    items: &mut Vec<RankedCompletionItem>,
    mut item: CompletionItem,
    prefix: &str,
    context: CompletionRankContext,
) {
    let Some(match_quality) = completion_match_quality(&item.label, prefix) else {
        return;
    };
    let rank = CompletionRank {
        match_quality,
        schema_tier: context.schema_tier,
        required_penalty: u8::from(!context.required),
        specificity: context.specificity,
        scope_distance: context.scope_distance,
        deprecated: context.deprecated,
    };
    item.sort_score = rank.sort_score();
    items.push(RankedCompletionItem { item, rank });
}

fn merge_completion_item(best: &mut CompletionItem, other: &CompletionItem) {
    if best.documentation.is_none() {
        best.documentation = other.documentation.clone();
    }
    if best.resolve_data.is_none() {
        best.resolve_data = other.resolve_data.clone();
    }
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

/// Selects the best evidence for duplicate labels, sorts deterministically, and materializes the
/// editor-neutral DTOs.  Deduplication happens before sorting because the same label can be
/// emitted by multiple semantic rules with different ranks.
pub(crate) fn finalize_completion_items(
    candidates: Vec<RankedCompletionItem>,
) -> Vec<CompletionItem> {
    let mut unique = BTreeMap::<(String, CompletionKind), RankedCompletionItem>::new();
    for candidate in candidates {
        let key = (
            candidate.item.label.to_ascii_lowercase(),
            candidate.item.kind,
        );
        match unique.entry(key) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(candidate);
            }
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                if candidate.rank < entry.get().rank {
                    let mut replacement = candidate;
                    merge_completion_item(&mut replacement.item, &entry.get().item);
                    entry.insert(replacement);
                } else {
                    merge_completion_item(&mut entry.get_mut().item, &candidate.item);
                }
            }
        }
    }
    let mut candidates = unique.into_values().collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        left.rank
            .cmp(&right.rank)
            .then_with(|| completion_label_cmp(&left.item.label, &right.item.label))
            .then_with(|| left.item.kind.cmp(&right.item.kind))
            .then_with(|| left.item.detail.cmp(&right.item.detail))
            .then_with(|| left.item.insert_text.cmp(&right.item.insert_text))
    });
    candidates
        .into_iter()
        .map(|mut candidate| {
            candidate.item.sort_score = candidate.rank.sort_score();
            candidate.item
        })
        .collect()
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
