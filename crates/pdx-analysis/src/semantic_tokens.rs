//! Editor-neutral semantic token queries.
//!
//! Highlighting is a deterministic single pass over the loss-aware CST plus a flat set of
//! rule-known keys and active dynamic definition names. Classification never depends on diagnostics
//! validity: a document with syntax errors still produces tokens from every recoverable node. The
//! stable legend contract lives in [`crate::SemanticTokenType`]; protocol adapters only convert
//! ranges and legend indices.

use std::collections::BTreeSet;
use std::sync::Arc;

use pdx_engine::{AnalysisSnapshot, DocumentId};
use pdx_parser::{CstKind, CstNode, FileFormat, ParsedFile};
use pdx_rules::KeyMatcher;
use pdx_text::TextRange;

use crate::semantic::effective_workspace_member_names;
use crate::support::{ParsedContent, input_for_document};
use crate::types::{CancellationToken, Cancelled, SemanticToken, SemanticTokenType, uncancelled};

/// Returns semantic tokens for an open script document.
#[must_use]
pub fn semantic_tokens(snapshot: &AnalysisSnapshot, document: &DocumentId) -> Vec<SemanticToken> {
    uncancelled(semantic_tokens_with_cancellation(
        snapshot,
        document,
        &CancellationToken::new(),
    ))
}

/// Returns semantic tokens with cooperative cancellation checkpoints.
///
/// Script files are tokenized; localisation and unsupported documents produce no tokens.
pub fn semantic_tokens_with_cancellation(
    snapshot: &AnalysisSnapshot,
    document: &DocumentId,
    cancellation: &CancellationToken,
) -> Result<Vec<SemanticToken>, Cancelled> {
    semantic_tokens_in_range_with_cancellation(snapshot, document, None, cancellation)
}

/// Returns semantic tokens intersecting `range` with cooperative cancellation.
///
/// The range is a UTF-8 byte span in the document.  CST nodes whose complete source range is
/// outside the viewport are skipped before descending, so a large generated file does not pay
/// the classification cost for entities the editor did not request.  Passing `None` is the full
/// document query used by [`semantic_tokens_with_cancellation`].
pub fn semantic_tokens_in_range_with_cancellation(
    snapshot: &AnalysisSnapshot,
    document: &DocumentId,
    range: Option<TextRange>,
    cancellation: &CancellationToken,
) -> Result<Vec<SemanticToken>, Cancelled> {
    cancellation.checkpoint()?;
    let Some(input) = input_for_document(snapshot, document) else {
        return Ok(Vec::new());
    };
    if input.format != FileFormat::Script {
        return Ok(Vec::new());
    }
    let keys = semantic_keys(snapshot);
    let profile = snapshot.game_profile();
    let ParsedContent::Text(parsed) = &input.parsed;
    let mut tokens = Vec::new();
    collect_tokens(
        parsed,
        parsed.root(),
        &keys,
        profile,
        &mut tokens,
        cancellation,
        range,
    )?;
    Ok(tokens)
}

/// Builds the flat set of rule-known script keys: profile fallback keys, exact semantic-rule
/// keys, symbol descriptor kinds, and active workspace-defined dynamic definition names. Record table
/// column names are deliberately excluded so CWT metadata never colors script text.
/// Rule- and profile-derived key set, rebuilt only when the rules change.
///
/// Tens of thousands of exact rule keys are lowercased into this set; doing
/// that per semantic-tokens request dominated the request cost.
fn static_semantic_keys(snapshot: &AnalysisSnapshot) -> Arc<BTreeSet<String>> {
    let revision = snapshot.revision();
    const KEY: &str = "semantic-keys:static";
    if let Some(cached) = snapshot
        .query_cache()
        .get::<BTreeSet<String>>(revision, KEY)
    {
        return cached;
    }
    let mut keys = snapshot
        .game_profile()
        .fallback_keys
        .iter()
        .map(|key| key.to_ascii_lowercase())
        .collect::<BTreeSet<_>>();
    for rule in &snapshot.rules().model().semantic.rules {
        if let KeyMatcher::Exact(key) = &rule.key {
            keys.insert(key.to_ascii_lowercase());
        }
    }
    keys.extend(
        snapshot
            .rules()
            .model()
            .symbol_descriptors
            .iter()
            .map(|descriptor| descriptor.kind_id.to_ascii_lowercase()),
    );
    let keys = Arc::new(keys);
    snapshot.query_cache().insert(
        revision,
        pdx_engine::CacheDomain::Index,
        KEY.to_owned(),
        Arc::clone(&keys),
    );
    keys
}

fn semantic_keys(snapshot: &AnalysisSnapshot) -> BTreeSet<String> {
    let mut keys = (*static_semantic_keys(snapshot)).clone();
    // Completion classifies workspace-defined dynamic definitions as callable functions. Reuse the
    // same effective (overlay-aware and source-priority-aware) member view for source coloring so
    // a definition does not switch back to the generic property color after insertion.
    let dynamic_types = snapshot
        .rules()
        .model()
        .semantic
        .type_descriptors
        .iter()
        .filter_map(|(type_name, descriptor)| {
            descriptor
                .dynamic_definition
                .as_ref()
                .filter(|dynamic_descriptor| dynamic_descriptor.enabled)
                .map(|_| type_name.clone())
        })
        .collect::<Vec<_>>();
    for type_name in dynamic_types {
        keys.extend(
            effective_workspace_member_names(snapshot, &type_name)
                .into_iter()
                .map(|name| name.to_ascii_lowercase()),
        );
    }
    keys
}

/// Recursively classifies one CST node in source order.
fn collect_tokens(
    parsed: &ParsedFile,
    node: CstNode<'_>,
    keys: &BTreeSet<String>,
    profile: &pdx_rules::GameProfile,
    tokens: &mut Vec<SemanticToken>,
    cancellation: &CancellationToken,
    range: Option<TextRange>,
) -> Result<(), Cancelled> {
    cancellation.checkpoint()?;
    if let Some(viewport) = range
        && (node.range().end() <= viewport.start() || node.range().start() >= viewport.end())
    {
        return Ok(());
    }
    match node.kind() {
        CstKind::Comment => push_token_if_visible(
            tokens,
            node.range(),
            SemanticTokenType::Comment,
            false,
            range,
        ),
        CstKind::Key => {
            if let Some(text) = parsed.text(node.range()) {
                let (token_type, definition) = classify_identifier(text, true, keys, profile);
                push_token_if_visible(tokens, node.range(), token_type, definition, range);
            }
        }
        CstKind::Operator => push_token_if_visible(
            tokens,
            node.range(),
            SemanticTokenType::Operator,
            false,
            range,
        ),
        CstKind::HeaderBlock => {
            // The header is the leading scalar child; its block content follows.
            if let Some(header) = node.children().next() {
                if header.kind() == CstKind::BareValue {
                    push_token_if_visible(
                        tokens,
                        header.range(),
                        SemanticTokenType::Type,
                        false,
                        range,
                    );
                } else {
                    collect_tokens(parsed, header, keys, profile, tokens, cancellation, range)?;
                }
            }
            for child in node.children().skip(1) {
                collect_tokens(parsed, child, keys, profile, tokens, cancellation, range)?;
            }
        }
        CstKind::ParameterCondition => {
            // The condition name is a parameter selector such as `country` in `[[!country] … ]`.
            for child in node.children() {
                if child.kind() == CstKind::BareValue {
                    push_token_if_visible(
                        tokens,
                        child.range(),
                        SemanticTokenType::Parameter,
                        false,
                        range,
                    );
                } else {
                    collect_tokens(parsed, child, keys, profile, tokens, cancellation, range)?;
                }
            }
        }
        CstKind::BareValue | CstKind::QuotedString => {
            if let Some(text) = parsed.text(node.range()) {
                let (token_type, definition) = classify_identifier(text, false, keys, profile);
                push_token_if_visible(tokens, node.range(), token_type, definition, range);
            }
        }
        CstKind::Property
        | CstKind::Value
        | CstKind::Block
        | CstKind::ParameterBlock
        | CstKind::Document
        | CstKind::Error => {
            for child in node.children() {
                collect_tokens(parsed, child, keys, profile, tokens, cancellation, range)?;
            }
        }
        // The BOM and localisation node kinds never reach a Script document walk.
        CstKind::Bom
        | CstKind::LocalisationDocument
        | CstKind::LanguageHeader
        | CstKind::LocalisationEntry
        | CstKind::LocalisationKey
        | CstKind::Version
        | CstKind::LocalisationString
        | CstKind::UnquotedValue => {}
    }
    Ok(())
}

/// Classifies a scalar by spelling. `is_key` selects the key-position rules: quoted keys stay
/// data properties, `@name` keys are variable definitions, control-flow keys are keywords, and
/// rule-known keys are functions.
fn classify_identifier(
    text: &str,
    is_key: bool,
    keys: &BTreeSet<String>,
    profile: &pdx_rules::GameProfile,
) -> (SemanticTokenType, bool) {
    if text.starts_with('"') {
        return if is_key {
            (SemanticTokenType::Property, false)
        } else {
            (SemanticTokenType::String, false)
        };
    }
    if text.starts_with('@') {
        // `@name` in key position binds a scripted variable; value positions use it.
        return (SemanticTokenType::Variable, is_key);
    }
    if is_parameter_spelling(text) {
        return (SemanticTokenType::Parameter, false);
    }
    if is_key {
        if profile.is_control_flow_key(text) {
            return (SemanticTokenType::Keyword, false);
        }
        return if keys.contains(text.to_ascii_lowercase().as_str()) {
            (SemanticTokenType::Function, false)
        } else {
            (SemanticTokenType::Property, false)
        };
    }
    if is_number_spelling(text) {
        return (SemanticTokenType::Number, false);
    }
    if text.eq_ignore_ascii_case("yes")
        || text.eq_ignore_ascii_case("no")
        || text.eq_ignore_ascii_case("true")
        || text.eq_ignore_ascii_case("false")
    {
        return (SemanticTokenType::Boolean, false);
    }
    (SemanticTokenType::String, false)
}

/// Matches the `$name$` parameter spelling.
fn is_parameter_spelling(text: &str) -> bool {
    text.len() >= 3
        && text.starts_with('$')
        && text.ends_with('$')
        && !text[1..text.len() - 1].contains('$')
}

/// Matches the numeric scalar spelling `-?[0-9]+(\.[0-9]+)?`.
fn is_number_spelling(text: &str) -> bool {
    let digits = text.strip_prefix('-').unwrap_or(text);
    let Some((integer, fraction)) = digits.split_once('.') else {
        return !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit());
    };
    !integer.is_empty()
        && integer.bytes().all(|byte| byte.is_ascii_digit())
        && !fraction.is_empty()
        && fraction.bytes().all(|byte| byte.is_ascii_digit())
}

fn token(range: TextRange, token_type: SemanticTokenType, definition: bool) -> SemanticToken {
    SemanticToken {
        range,
        token_type,
        definition,
    }
}

fn push_token_if_visible(
    tokens: &mut Vec<SemanticToken>,
    token_range: TextRange,
    token_type: SemanticTokenType,
    definition: bool,
    viewport: Option<TextRange>,
) {
    if viewport
        .is_none_or(|range| token_range.end() > range.start() && token_range.start() < range.end())
    {
        tokens.push(token(token_range, token_type, definition));
    }
}
