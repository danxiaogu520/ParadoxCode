//! Idempotent whole-file conversion, including comments, identifiers and strings.
//! Existing escape triples are preserved by encoding; readable runs are preserved
//! by decoding. Mixed legacy script bytes and readable UTF-8 are handled per run.

use crate::{EncodeError, EscapeSet, Profile, cp1252, encode_file};

/// A whole-file conversion cannot safely transform the input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileConversionError {
    /// A localisation file is not valid UTF-8.
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

/// Encodes every readable run, preserving valid existing triples byte-for-byte.
/// No syntax or quote boundary limits conversion.
///
/// # Errors
/// Refuses the complete file on invalid UTF-8, damaged triples or unencodable text.
pub fn encode_buffer(
    input: &[u8],
    profile: Profile,
    escape_set: EscapeSet,
) -> Result<Vec<u8>, FileConversionError> {
    convert(input, profile, Direction::Encode(escape_set))
}

/// Decodes every escape triple, including comments and unquoted text. Plain UTF-8
/// runs retain their bytes; legacy script bytes are converted through CP1252.
///
/// # Errors
/// Refuses the complete file on invalid localisation UTF-8 or damaged triples.
pub fn decode_buffer(input: &[u8], profile: Profile) -> Result<Vec<u8>, FileConversionError> {
    convert(input, profile, Direction::Decode)
}

fn convert(
    input: &[u8],
    profile: Profile,
    direction: Direction,
) -> Result<Vec<u8>, FileConversionError> {
    if profile == Profile::Localisation && std::str::from_utf8(input).is_err() {
        return Err(FileConversionError::InvalidUtf8);
    }
    let mut output = Vec::with_capacity(input.len());
    let mut index = 0;
    while index < input.len() {
        if (0x10..=0x13).contains(&input[index]) {
            let (next, character) = escape_at(input, index, input.len(), profile)?;
            match direction {
                Direction::Encode(_) => output.extend_from_slice(&input[index..next]),
                Direction::Decode => {
                    let mut encoded = [0; 4];
                    output.extend_from_slice(character.encode_utf8(&mut encoded).as_bytes());
                }
            }
            index = next;
        } else {
            let next = input[index..]
                .iter()
                .position(|byte| (0x10..=0x13).contains(byte))
                .map_or(input.len(), |offset| index + offset);
            let run = &input[index..next];
            match direction {
                Direction::Encode(escape_set) => {
                    encode_run(run, profile, escape_set, index, &mut output)?;
                }
                Direction::Decode => decode_run(run, &mut output),
            }
            index = next;
        }
    }
    Ok(output)
}

fn decode_run(mut input: &[u8], output: &mut Vec<u8>) {
    while !input.is_empty() {
        match std::str::from_utf8(input) {
            Ok(_) => {
                output.extend_from_slice(input);
                break;
            }
            Err(error) => {
                let prefix = error.valid_up_to();
                output.extend_from_slice(&input[..prefix]);
                let width = error.error_len().unwrap_or(input.len() - prefix);
                for byte in &input[prefix..prefix + width] {
                    let character = char::from_u32(cp1252::byte_to_char(*byte))
                        .expect("CP1252 maps each byte to a scalar");
                    let mut encoded = [0; 4];
                    output.extend_from_slice(character.encode_utf8(&mut encoded).as_bytes());
                }
                input = &input[prefix + width..];
            }
        }
    }
}

fn escape_at(
    input: &[u8],
    start: usize,
    end: usize,
    profile: Profile,
) -> Result<(usize, char), FileConversionError> {
    let broken = || FileConversionError::BrokenEscape { byte_index: start };
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
    // A valid triple never consumes an escape-set structural byte, including
    // quote, comment, whitespace and line boundaries.
    if [low, high]
        .iter()
        .any(|byte| crate::EscapeSet::Paratranz.contains(*byte))
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
) -> Result<(), FileConversionError> {
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
            Err(_) => return Err(FileConversionError::InvalidUtf8),
        };
        let encoded = encode_file(readable, profile, escape_set).map_err(|mut error| {
            for point in &mut error.unencodable {
                point.byte_index += base;
            }
            FileConversionError::Unencodable { error }
        })?;
        output.extend_from_slice(&encoded);
        let consumed = readable.len() + legacy;
        output.extend_from_slice(&input[readable.len()..consumed]);
        base += consumed;
        input = &input[consumed..];
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complete_files_match_the_canonical_codec_and_round_trip() {
        let readable = "\u{FEFF}# 中文注释\r\n中文标识 = 中文值\r\nname = \"中文内容\"\r\n";
        for profile in [Profile::Script, Profile::Localisation] {
            let encoded = encode_buffer(readable.as_bytes(), profile, EscapeSet::Paratranz)
                .expect("whole-file encoding");
            assert_eq!(
                encoded,
                encode_file(readable, profile, EscapeSet::Paratranz).unwrap()
            );
            assert_eq!(
                decode_buffer(&encoded, profile).unwrap(),
                readable.as_bytes()
            );
            assert_eq!(
                encode_buffer(&encoded, profile, EscapeSet::Paratranz).unwrap(),
                encoded
            );
            assert_eq!(
                decode_buffer(readable.as_bytes(), profile).unwrap(),
                readable.as_bytes()
            );
        }
    }

    #[test]
    fn mixed_files_converge_across_comments_identifiers_and_strings() {
        for profile in [Profile::Script, Profile::Localisation] {
            let escaped = encode_file("中", profile, EscapeSet::DllFull).unwrap();
            let mut mixed = b"# ".to_vec();
            mixed.extend_from_slice(&escaped);
            mixed.extend_from_slice("文\n中文 = \"".as_bytes());
            mixed.extend_from_slice(&escaped);
            mixed.extend_from_slice("文\"\n".as_bytes());
            let decoded = "# 中文\n中文 = \"中文\"\n";
            assert_eq!(decode_buffer(&mixed, profile).unwrap(), decoded.as_bytes());
            let encoded = encode_buffer(&mixed, profile, EscapeSet::Paratranz).unwrap();
            assert_eq!(
                decode_buffer(&encoded, profile).unwrap(),
                decoded.as_bytes()
            );
            assert_eq!(
                encode_buffer(&encoded, profile, EscapeSet::Paratranz).unwrap(),
                encoded
            );
        }
    }

    #[test]
    fn even_one_unquoted_escape_is_decoded_without_a_threshold() {
        for profile in [Profile::Script, Profile::Localisation] {
            let encoded = encode_file("# 中\n", profile, EscapeSet::Paratranz).unwrap();
            assert_eq!(
                decode_buffer(&encoded, profile).unwrap(),
                "# 中\n".as_bytes()
            );
        }
    }

    #[test]
    fn comments_and_bare_text_have_the_same_refusals_as_strings() {
        for text in ["# 😀\n", "value = 😀\n", "name = \"😀\"\n"] {
            assert!(matches!(
                encode_buffer(text.as_bytes(), Profile::Script, EscapeSet::Paratranz),
                Err(FileConversionError::Unencodable { .. })
            ));
        }
        for bytes in [
            b"# \x10\n".as_slice(),
            b"name = \"\x10\"\n".as_slice(),
            b"\x10".as_slice(),
        ] {
            let offset = bytes.iter().position(|byte| *byte == 0x10).unwrap();
            for profile in [Profile::Script, Profile::Localisation] {
                assert_eq!(
                    decode_buffer(bytes, profile),
                    Err(FileConversionError::BrokenEscape { byte_index: offset })
                );
                assert_eq!(
                    encode_buffer(bytes, profile, EscapeSet::Paratranz),
                    Err(FileConversionError::BrokenEscape { byte_index: offset })
                );
            }
        }
    }

    #[test]
    fn localisation_utf8_is_checked_outside_strings_too() {
        for bytes in [
            b"# \xff\nname = \"x\"\n".as_slice(),
            b"\xff = x\n".as_slice(),
        ] {
            assert_eq!(
                decode_buffer(bytes, Profile::Localisation),
                Err(FileConversionError::InvalidUtf8)
            );
            assert_eq!(
                encode_buffer(bytes, Profile::Localisation, EscapeSet::Paratranz),
                Err(FileConversionError::InvalidUtf8)
            );
        }
    }

    #[test]
    fn script_legacy_bytes_and_readable_utf8_share_a_complete_file() {
        let bytes = [b"# K\xf6nig ".as_slice(), "中文\nname = \"é\"\n".as_bytes()].concat();
        let readable = "# König 中文\nname = \"é\"\n";
        assert_eq!(
            decode_buffer(&bytes, Profile::Script).unwrap(),
            readable.as_bytes()
        );
        let encoded = encode_buffer(&bytes, Profile::Script, EscapeSet::Paratranz).unwrap();
        assert_eq!(
            decode_buffer(&encoded, Profile::Script).unwrap(),
            readable.as_bytes()
        );
    }
}
