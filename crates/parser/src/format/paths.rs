//! Asset-path spelling canonicalization shared by the script formatter, its
//! safety gate, and the fuzz invariants.

use std::borrow::Cow;

/// File extensions whose scalar values the formatter treats as asset paths.
/// Vanilla ships the same texture with `/`, `//`, `\`, and `\\` separators, and
/// the engine accepts all four spellings, so they canonicalize to single
/// forward slashes. Matching is case-insensitive.
const ASSET_EXTENSIONS: &[&str] = &[
    "dds", "tga", "png", "jpg", "jpeg", "bmp", "mesh", "anim", "ttf", "wav", "ogg",
];

/// Whether a scalar token's spelling (surrounding quotes included) denotes an
/// asset path the formatter may rewrite. The inner text must be non-empty,
/// free of whitespace and quote characters (which also rules out escaped
/// quotes such as `\"`), and end with a known asset extension.
#[must_use]
pub fn is_asset_path_spelling(token_text: &str) -> bool {
    let inner = unquote(token_text);
    if inner.is_empty()
        || inner
            .chars()
            .any(|character| character.is_whitespace() || character == '"' || character == '\'')
    {
        return false;
    }
    match inner.rsplit_once('.') {
        Some((_, extension)) => ASSET_EXTENSIONS
            .iter()
            .any(|known| known.eq_ignore_ascii_case(extension)),
        None => false,
    }
}

/// Rewrites every maximal run of `/` and `\` separators to a single `/`.
/// Already-canonical spellings are returned borrowed, which makes the
/// normalization idempotent.
#[must_use]
pub fn normalize_asset_path_separators(token_text: &str) -> Cow<'_, str> {
    if !token_text.contains(['/', '\\']) {
        return Cow::Borrowed(token_text);
    }
    let mut output = String::with_capacity(token_text.len());
    let mut in_separator_run = false;
    for character in token_text.chars() {
        if character == '/' || character == '\\' {
            if !in_separator_run {
                output.push('/');
                in_separator_run = true;
            }
        } else {
            output.push(character);
            in_separator_run = false;
        }
    }
    if output == token_text {
        Cow::Borrowed(token_text)
    } else {
        Cow::Owned(output)
    }
}

/// Whether rewriting `before` to `after` is exactly the asset-path separator
/// canonicalization; the safety gate and the fuzz invariants accept no other
/// scalar text change beyond keyword casing.
#[must_use]
pub fn is_asset_path_normalization(before: &str, after: &str) -> bool {
    before != after
        && is_asset_path_spelling(before)
        && normalize_asset_path_separators(before).as_ref() == after
}

fn unquote(token_text: &str) -> &str {
    token_text
        .strip_prefix('"')
        .and_then(|inner| inner.strip_suffix('"'))
        .unwrap_or(token_text)
}
