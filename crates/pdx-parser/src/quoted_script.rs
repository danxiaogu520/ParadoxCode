//! Decoding and source mapping for Script embedded in a quoted scalar.

use pdx_text::{TextRange, TextSize};

use crate::{FileFormat, ParsedFile, parse};

/// A decoded quoted Script payload together with its lossless parser result and source map.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QuotedScript {
    parsed: ParsedFile,
    source_map: QuotedScriptSourceMap,
    closed: bool,
}

impl QuotedScript {
    /// Returns the decoded Script parse. Parser errors are retained for editor recovery.
    #[must_use]
    pub const fn parsed(&self) -> &ParsedFile {
        &self.parsed
    }

    /// Returns the mapping between decoded payload offsets and offsets in the quoted token.
    #[must_use]
    pub const fn source_map(&self) -> &QuotedScriptSourceMap {
        &self.source_map
    }

    /// Returns whether the source token had a closing quote.
    #[must_use]
    pub const fn is_closed(&self) -> bool {
        self.closed
    }
}

/// Monotonic UTF-8 byte-boundary mapping from decoded Script to the original quoted token.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QuotedScriptSourceMap {
    decoded_to_source: Vec<TextSize>,
    source_len: TextSize,
}

impl QuotedScriptSourceMap {
    /// Maps a decoded payload offset to an offset relative to the opening quote.
    #[must_use]
    pub fn decoded_offset(&self, offset: TextSize) -> Option<TextSize> {
        self.decoded_to_source
            .get(usize::try_from(offset).ok()?)
            .copied()
    }

    /// Maps a decoded payload range to a range relative to the opening quote.
    #[must_use]
    pub fn decoded_range(&self, range: TextRange) -> Option<TextRange> {
        TextRange::new(
            self.decoded_offset(range.start())?,
            self.decoded_offset(range.end())?,
        )
    }

    /// Maps an offset in the quoted token to the closest decoded boundary at or before it.
    #[must_use]
    pub fn source_offset(&self, offset: TextSize) -> Option<TextSize> {
        if offset > self.source_len {
            return None;
        }
        let index = self
            .decoded_to_source
            .partition_point(|candidate| *candidate <= offset)
            .saturating_sub(1);
        u32::try_from(index).ok()
    }
}

/// Decodes and parses a quoted scalar as Script while retaining parser recovery and source maps.
///
/// The source must start with `"`. An unterminated outer quote is accepted so completion can work
/// while the user is editing; in that case the remainder of `source` is treated as the payload.
#[must_use]
pub fn parse_quoted_script(source: &str) -> Option<QuotedScript> {
    let payload = source.strip_prefix('"')?;
    let closed = source.ends_with('"') && !closing_quote_is_escaped(source);
    let payload = if closed {
        payload.strip_suffix('"')?
    } else {
        payload
    };
    let (decoded, source_map) = decode_payload(payload, source.len())?;
    Some(QuotedScript {
        parsed: parse(FileFormat::Script, &decoded),
        source_map,
        closed,
    })
}

/// Encodes text for insertion into an existing quoted Script payload.
#[must_use]
pub fn encode_quoted_script_text(text: &str) -> String {
    let mut encoded = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '"' => encoded.push_str("\\\""),
            '\\' => encoded.push_str("\\\\"),
            _ => encoded.push(character),
        }
    }
    encoded
}

fn closing_quote_is_escaped(source: &str) -> bool {
    let slash_count = source[..source.len().saturating_sub(1)]
        .bytes()
        .rev()
        .take_while(|byte| *byte == b'\\')
        .count();
    slash_count % 2 == 1
}

fn decode_payload(payload: &str, source_len: usize) -> Option<(String, QuotedScriptSourceMap)> {
    let mut decoded = String::with_capacity(payload.len());
    let mut decoded_to_source = Vec::with_capacity(payload.len().saturating_add(1));
    decoded_to_source.push(1);
    let mut offset = 0_usize;
    while offset < payload.len() {
        let character = payload[offset..].chars().next()?;
        let character_len = character.len_utf8();
        if character != '\\' {
            decoded.push(character);
            for byte in 1..=character_len {
                decoded_to_source.push(u32::try_from(1 + offset + byte).ok()?);
            }
            offset += character_len;
            continue;
        }

        let escaped_offset = offset.checked_add(1)?;
        let escaped = payload.get(escaped_offset..)?.chars().next()?;
        let escaped_len = escaped.len_utf8();
        if matches!(escaped, '"' | '\\') {
            decoded.push(escaped);
            for byte in 1..=escaped_len {
                decoded_to_source.push(u32::try_from(1 + escaped_offset + byte).ok()?);
            }
        } else {
            decoded.push('\\');
            decoded_to_source.push(u32::try_from(1 + escaped_offset).ok()?);
            decoded.push(escaped);
            for byte in 1..=escaped_len {
                decoded_to_source.push(u32::try_from(1 + escaped_offset + byte).ok()?);
            }
        }
        offset = escaped_offset.checked_add(escaped_len)?;
    }
    Some((
        decoded,
        QuotedScriptSourceMap {
            decoded_to_source,
            source_len: u32::try_from(source_len).ok()?,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoted_script_maps_escaped_quotes_and_utf8_back_to_source() {
        let source = "\"name = \\\"界\\\"\nvalue = yes\"";
        let script = parse_quoted_script(source).expect("quoted script");
        assert_eq!(script.parsed().source(), "name = \"界\"\nvalue = yes");
        let quoted = script
            .parsed()
            .tokens()
            .iter()
            .find(|token| token.kind() == crate::TokenKind::Quoted)
            .expect("inner quote");
        let mapped = script
            .source_map()
            .decoded_range(quoted.range())
            .expect("mapped quote");
        assert_eq!(
            &source
                [usize::try_from(mapped.start()).unwrap()..usize::try_from(mapped.end()).unwrap()],
            "\\\"界\\\""
        );
    }

    #[test]
    fn quoted_script_keeps_recovery_for_incomplete_payload_and_outer_quote() {
        let script = parse_quoted_script("\"if = { value = yes").expect("quoted script");
        assert!(!script.is_closed());
        assert!(!script.parsed().errors().is_empty());
        assert_eq!(
            script.source_map().source_offset(5),
            Some(4),
            "source offsets include the opening quote"
        );
    }

    #[test]
    fn quoted_script_insertions_escape_only_quote_payload_syntax() {
        assert_eq!(
            encode_quoted_script_text("name = \"x\"\\path\nnext = yes"),
            "name = \\\"x\\\"\\\\path\nnext = yes"
        );
    }

    #[test]
    fn nested_quoted_script_tokens_decode_exactly_one_layer_at_a_time() {
        let inner_payload = r#"value = "slash\\\"quote"
next = yes"#;
        let inner_token = format!("\"{}\"", encode_quoted_script_text(inner_payload));
        let outer_payload = format!("nested = {inner_token}");
        let outer_token = format!("\"{}\"", encode_quoted_script_text(&outer_payload));

        let outer = parse_quoted_script(&outer_token).expect("outer quoted Script");
        assert_eq!(outer.parsed().source(), outer_payload);
        let nested_source = outer
            .parsed()
            .tokens()
            .iter()
            .find(|token| token.kind() == crate::TokenKind::Quoted)
            .and_then(|token| outer.parsed().text(token.range()))
            .expect("nested quoted token");
        assert_eq!(nested_source, inner_token);

        let inner = parse_quoted_script(nested_source).expect("inner quoted Script");
        assert_eq!(inner.parsed().source(), inner_payload);
    }

    #[test]
    fn quoted_script_source_map_is_monotonic_and_round_trips_boundaries() {
        for payload in [
            "first = yes\r\nsecond = no",
            "emoji = 😀\nname = \"界\"",
            "unknown = \\q\nslashes = \\\\",
            "nested = \\\"value = \\\\\\\"x\\\\\\\"\\\"",
        ] {
            let source = format!("\"{}\"", encode_quoted_script_text(payload));
            let script = parse_quoted_script(&source).expect("quoted script");
            assert_eq!(script.parsed().source(), payload);
            let mut previous = 0;
            for decoded in 0..=payload.len() {
                let decoded = u32::try_from(decoded).expect("bounded fixture");
                let source_offset = script
                    .source_map()
                    .decoded_offset(decoded)
                    .expect("decoded boundary");
                assert!(source_offset >= previous, "source map must be monotonic");
                assert_eq!(
                    script.source_map().source_offset(source_offset),
                    Some(decoded),
                    "mapped decoded boundaries must invert exactly"
                );
                previous = source_offset;
            }
        }
    }
}
