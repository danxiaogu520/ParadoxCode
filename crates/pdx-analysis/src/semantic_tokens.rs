//! Editor-neutral semantic token queries.
//!
//! Highlighting is a deterministic single pass over the loss-aware CST plus a flat set of
//! rule-known keys. Classification never depends on diagnostics validity: a document with syntax
//! errors still produces tokens from every recoverable node. The stable legend contract lives in
//! [`crate::SemanticTokenType`]; protocol adapters only convert ranges and legend indices.

use std::collections::BTreeSet;

use pdx_engine::{AnalysisSnapshot, DocumentId};
use pdx_parser::{CstKind, CstNode, FileFormat, ParsedFile};
use pdx_rules::KeyMatcher;
use pdx_text::TextRange;

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
    cancellation.checkpoint()?;
    let Some(input) = input_for_document(snapshot, document) else {
        return Ok(Vec::new());
    };
    if input.format != FileFormat::Script {
        return Ok(Vec::new());
    }
    let keys = semantic_keys(snapshot);
    let ParsedContent::Text(parsed) = &input.parsed;
    let mut tokens = Vec::new();
    collect_tokens(parsed, parsed.root(), &keys, &mut tokens, cancellation)?;
    Ok(tokens)
}

/// Builds the flat set of rule-known script keys: profile fallback keys, exact semantic-rule
/// keys, and symbol descriptor kinds. Record table column names are deliberately excluded so CWT
/// metadata never colors script text.
fn semantic_keys(snapshot: &AnalysisSnapshot) -> BTreeSet<String> {
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
    keys
}

/// Recursively classifies one CST node in source order.
fn collect_tokens(
    parsed: &ParsedFile,
    node: &CstNode,
    keys: &BTreeSet<String>,
    tokens: &mut Vec<SemanticToken>,
    cancellation: &CancellationToken,
) -> Result<(), Cancelled> {
    cancellation.checkpoint()?;
    match node.kind() {
        CstKind::Comment => tokens.push(token(node.range(), SemanticTokenType::Comment, false)),
        CstKind::Key => {
            if let Some(text) = parsed.text(node.range()) {
                let (token_type, definition) = classify_identifier(text, true, keys);
                tokens.push(token(node.range(), token_type, definition));
            }
        }
        CstKind::Operator => tokens.push(token(node.range(), SemanticTokenType::Operator, false)),
        CstKind::HeaderBlock => {
            // The header is the leading scalar child; its block content follows.
            if let Some(header) = node.children().first() {
                if header.kind() == CstKind::BareValue {
                    tokens.push(token(header.range(), SemanticTokenType::Type, false));
                } else {
                    collect_tokens(parsed, header, keys, tokens, cancellation)?;
                }
            }
            for child in node.children().iter().skip(1) {
                collect_tokens(parsed, child, keys, tokens, cancellation)?;
            }
        }
        CstKind::ParameterCondition => {
            // The condition name is a parameter selector such as `country` in `[[!country] … ]`.
            for child in node.children() {
                if child.kind() == CstKind::BareValue {
                    tokens.push(token(child.range(), SemanticTokenType::Parameter, false));
                } else {
                    collect_tokens(parsed, child, keys, tokens, cancellation)?;
                }
            }
        }
        CstKind::BareValue | CstKind::QuotedString => {
            if let Some(text) = parsed.text(node.range()) {
                let (token_type, definition) = classify_identifier(text, false, keys);
                tokens.push(token(node.range(), token_type, definition));
            }
        }
        CstKind::Property
        | CstKind::Value
        | CstKind::Block
        | CstKind::ParameterBlock
        | CstKind::Document
        | CstKind::Error => {
            for child in node.children() {
                collect_tokens(parsed, child, keys, tokens, cancellation)?;
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
/// data properties, `@name` keys are variable definitions, and rule-known keys are functions.
fn classify_identifier(
    text: &str,
    is_key: bool,
    keys: &BTreeSet<String>,
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
        return if keys.contains(text.to_ascii_lowercase().as_str()) {
            (SemanticTokenType::Function, false)
        } else {
            (SemanticTokenType::Property, false)
        };
    }
    if is_number_spelling(text) {
        return (SemanticTokenType::Number, false);
    }
    if text.eq_ignore_ascii_case("yes") || text.eq_ignore_ascii_case("no") {
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
