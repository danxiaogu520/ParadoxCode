//! Idempotent manual conversion of quoted content in partially transcoded files.
//! Existing escape triples are copied verbatim by encoding; decoding only replaces
//! those triples. Every byte outside quoted strings, including comments, is preserved.

use crate::{EncodeError, EscapeSet, Profile, cp1252, encode_file, scan_string_spans};

/// A manual conversion cannot safely transform one quoted fragment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QuotedConversionError {
    /// A localisation string is not valid UTF-8.
    InvalidUtf8,
    /// A marker has a truncated, structural, or invalid-code-point payload.
    BrokenEscape { byte_index: usize },
    /// Readable content contains characters the encoder cannot round-trip.
    Unencodable { error: EncodeError },
}

#[derive(Clone, Copy)]
enum Direction {
    Encode(EscapeSet),
    Decode,
}

/// Encodes only readable quoted fragments, preserving existing escape triples
/// byte-for-byte, including triples from other supported escape-set variants.
/// Comments and all other unquoted bytes are copied without interpretation.
///
/// # Errors
/// Returns [`QuotedConversionError`] for invalid or unencodable quoted content.
pub fn encode_quoted_file(
    input: &[u8],
    profile: Profile,
    escape_set: EscapeSet,
) -> Result<Vec<u8>, QuotedConversionError> {
    convert(input, profile, Direction::Encode(escape_set))
}

/// Decodes only escape triples inside quoted strings. Readable fragments and
/// comments retain their original bytes; no whole-file classification is used.
///
/// # Errors
/// Returns [`QuotedConversionError`] for invalid quoted UTF-8 or escape triples.
pub fn decode_quoted_file(
    input: &[u8],
    profile: Profile,
) -> Result<Vec<u8>, QuotedConversionError> {
    convert(input, profile, Direction::Decode)
}

fn convert(
    input: &[u8],
    profile: Profile,
    direction: Direction,
) -> Result<Vec<u8>, QuotedConversionError> {
    let mut output = Vec::with_capacity(input.len());
    let mut cursor = 0;
    for (start, end) in scan_string_spans(input) {
        output.extend_from_slice(&input[cursor..start]);
        let span = &input[start..end];
        if profile == Profile::Localisation && std::str::from_utf8(span).is_err() {
            return Err(QuotedConversionError::InvalidUtf8);
        }
        let mut index = start;
        while index < end {
            if (0x10..=0x13).contains(&input[index]) {
                let (triple_end, character) = escape_at(input, index, end, profile)?;
                match direction {
                    Direction::Encode(_) => output.extend_from_slice(&input[index..triple_end]),
                    Direction::Decode => {
                        let mut encoded = [0; 4];
                        output.extend_from_slice(character.encode_utf8(&mut encoded).as_bytes());
                    }
                }
                index = triple_end;
            } else {
                let run_end = input[index..end]
                    .iter()
                    .position(|byte| (0x10..=0x13).contains(byte))
                    .map_or(end, |offset| index + offset);
                let run = &input[index..run_end];
                match direction {
                    Direction::Encode(escape_set) => {
                        encode_run(run, profile, escape_set, index, &mut output)?;
                    }
                    Direction::Decode => output.extend_from_slice(run),
                }
                index = run_end;
            }
        }
        cursor = end;
    }
    output.extend_from_slice(&input[cursor..]);
    Ok(output)
}

fn escape_at(
    input: &[u8],
    start: usize,
    end: usize,
    profile: Profile,
) -> Result<(usize, char), QuotedConversionError> {
    let broken = || QuotedConversionError::BrokenEscape { byte_index: start };
    let (triple_end, low, high) = match profile {
        Profile::Script => {
            if end - start < 3 {
                return Err(broken());
            }
            (start + 3, input[start + 1], input[start + 2])
        }
        Profile::Localisation => {
            let (low_end, low) = payload_character(input, start + 1, end).ok_or_else(broken)?;
            let (high_end, high) = payload_character(input, low_end, end).ok_or_else(broken)?;
            (high_end, low, high)
        }
    };
    // A valid triple never consumes a quote, escape, or line boundary. In
    // particular, a marker before the closing quote must not eat that quote.
    if [low, high]
        .iter()
        .any(|byte| matches!(byte, b'"' | b'\\' | b'\r' | b'\n'))
    {
        return Err(broken());
    }
    let code_point = crate::escape::reconstruct(u32::from(input[start]), low, high);
    // Escape triples represent four-digit code points; ASCII/Latin-1 and
    // lower-plane characters cannot legitimately be emitted as these triples.
    if !(0x1000..=0xFFFF).contains(&code_point) {
        return Err(broken());
    }
    let character = char::from_u32(code_point).ok_or_else(broken)?;
    Ok((triple_end, character))
}

fn payload_character(input: &[u8], start: usize, end: usize) -> Option<(usize, u8)> {
    let first = *input.get(start).filter(|_| start < end)?;
    let width = match first {
        0..=0x7F => 1,
        0xC2..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF4 => 4,
        _ => return None,
    };
    let next = start + width;
    if next > end {
        return None;
    }
    let character = std::str::from_utf8(&input[start..next])
        .ok()?
        .chars()
        .next()?;
    let code_point = u32::from(character);
    let byte = if code_point < 0x100 {
        code_point as u8
    } else {
        cp1252::mapped_byte(code_point)?
    };
    Some((next, byte))
}

fn encode_run(
    mut input: &[u8],
    profile: Profile,
    escape_set: EscapeSet,
    mut base: usize,
    output: &mut Vec<u8>,
) -> Result<(), QuotedConversionError> {
    while !input.is_empty() {
        let (readable, legacy) = match std::str::from_utf8(input) {
            Ok(text) => (text, 0),
            Err(error) if profile == Profile::Script => {
                // Readable UTF-8 and legacy CP1252 bytes may share a run.
                // Convert valid UTF-8 portions and preserve already-single-byte
                // characters instead of interpreting the entire run as CP1252.
                let prefix = error.valid_up_to();
                let text = std::str::from_utf8(&input[..prefix]).expect("validated UTF-8 prefix");
                let legacy = error.error_len().unwrap_or(input.len() - prefix);
                (text, legacy)
            }
            Err(_) => return Err(QuotedConversionError::InvalidUtf8),
        };
        let encoded = encode_file(readable, profile, escape_set).map_err(|mut error| {
            for point in &mut error.unencodable {
                point.byte_index += base;
            }
            QuotedConversionError::Unencodable { error }
        })?;
        output.extend_from_slice(&encoded);
        let consumed = readable.len() + legacy;
        output.extend_from_slice(&input[readable.len()..consumed]);
        base += consumed;
        input = &input[consumed..];
    }
    Ok(())
}
