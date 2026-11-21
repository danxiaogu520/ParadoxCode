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

/// Operation applied to a whole buffer or an independently selected byte span.
#[derive(Clone, Copy)]
pub enum Direction {
    /// Encode readable runs using the selected EU4 escape set.
    Encode(EscapeSet),
    /// Decode valid escapes to readable UTF-8.
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

/// Convert a large editor buffer in an isolated codec process. Ordinary analysis callers use
/// the serial functions so workspace workers do not each start another group of threads.
///
/// # Errors
/// Returns the exact same refusal and input offsets as the corresponding serial conversion.
pub fn convert_editor_buffer(
    input: &[u8],
    profile: Profile,
    direction: Direction,
) -> Result<Vec<u8>, FileConversionError> {
    const MIN_BYTES_PER_WORKER: usize = 256 * 1024;
    const MAX_WORKERS: usize = 4;
    let groups = (input.len() / MIN_BYTES_PER_WORKER).min(MAX_WORKERS);
    if groups < 2 {
        return convert(input, profile, direction);
    }
    let workers = std::thread::available_parallelism().map_or(1, |n| n.get().min(groups));
    if workers > 1
        && let Some(output) = convert_lines(input, profile, direction, workers)
    {
        return Ok(output);
    }
    // Failures are uncommon. Replaying once preserves global UTF-8 validation precedence
    // and the full list of unencodable characters in the original, possibly multi-line run.
    convert(input, profile, direction)
}

fn convert_lines(
    input: &[u8],
    profile: Profile,
    direction: Direction,
    workers: usize,
) -> Option<Vec<u8>> {
    // A literal LF is never legal inside an EU4 escape or a UTF-8 scalar. Include it in the
    // previous slice so a malformed trailing marker is rejected there as well.
    let mut chunks = Vec::with_capacity(workers);
    let mut start = 0;
    for index in 1..workers {
        let target = (input.len() / workers) * index;
        if target <= start {
            continue;
        }
        let Some(distance) = input[target..].iter().position(|&byte| byte == b'\n') else {
            break;
        };
        let end = target + distance + 1;
        if end == input.len() {
            break;
        }
        chunks.push(&input[start..end]);
        start = end;
    }
    if chunks.is_empty() {
        return None;
    }
    chunks.push(&input[start..]);
    std::thread::scope(|scope| {
        let mut handles = Vec::with_capacity(chunks.len() - 1);
        for &chunk in &chunks[1..] {
            handles.push(
                std::thread::Builder::new()
                    .spawn_scoped(scope, move || convert(chunk, profile, direction))
                    .ok()?,
            );
        }
        let mut outputs = Vec::with_capacity(chunks.len());
        outputs.push(convert(chunks[0], profile, direction).ok()?);
        for handle in handles {
            outputs.push(handle.join().ok()?.ok()?);
        }
        Some(outputs.concat())
    })
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
                Direction::Encode(_) if run.is_ascii() => output.extend_from_slice(run),
                Direction::Encode(escape_set) => {
                    encode_run(run, profile, escape_set, index, &mut output)?;
                }
                Direction::Decode if profile == Profile::Localisation => {
                    output.extend_from_slice(run)
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
    // The input escape set is not a blacklist of output bytes: compensating
    // low 0x2F produces '=' (e.g. 是), and the historical 0x21 variant produces
    // '/'. Accept those payloads only when the marker carries low compensation.
    // Other structural bytes still cannot swallow quotes or line boundaries.
    let compensated_delimiter = matches!(input[start], 0x11 | 0x13) && matches!(low, b'=' | b'/');
    if (EscapeSet::Paratranz.contains(low) && !compensated_delimiter)
        || EscapeSet::Paratranz.contains(high)
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
        0..=0x7F => return Some((start + 1, first)),
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
    fn worker_chunks_match_serial_bytes_in_source_order() {
        for profile in [Profile::Localisation, Profile::Script] {
            let mut line = encode_file("已有转义：是中国", profile, EscapeSet::Paratranz).unwrap();
            line.extend_from_slice(" # 新文字\r\n".as_bytes());
            if profile == Profile::Script {
                line.extend_from_slice(b"# K\xf6nig\n");
            }
            let mut input = line.repeat(73);
            input.extend_from_slice(b"\x11=f");
            for direction in [
                Direction::Encode(EscapeSet::Paratranz),
                Direction::Encode(EscapeSet::DllFull),
                Direction::Decode,
            ] {
                let expected = convert(&input, profile, direction).unwrap();
                for workers in [2, 4] {
                    assert_eq!(
                        convert_lines(&input, profile, direction, workers).unwrap(),
                        expected
                    );
                }
            }
        }
    }

    #[test]
    fn large_buffer_refusals_retain_serial_precedence_counts_and_offsets() {
        let lines = b"entry = value\n".repeat(32 * 1024);
        let mut input = lines.clone();
        input.extend_from_slice("bad = 😀\r\n".as_bytes());
        input.extend_from_slice(&lines);
        input.extend_from_slice("also_bad = 🐱".as_bytes());
        let direction = Direction::Encode(EscapeSet::Paratranz);
        let profile = Profile::Localisation;
        let expected = convert(&input, profile, direction).unwrap_err();
        let FileConversionError::Unencodable { error } = &expected else {
            panic!("expected unencodable characters")
        };
        assert_eq!(error.unencodable.len(), 2);
        assert_eq!(
            convert_editor_buffer(&input, profile, direction).unwrap_err(),
            expected
        );

        let mut broken = lines;
        let offset = broken.len();
        broken.extend_from_slice(b"\x10\n");
        broken.extend_from_slice(&input);
        assert_eq!(
            convert_editor_buffer(&broken, profile, Direction::Decode).unwrap_err(),
            FileConversionError::BrokenEscape { byte_index: offset }
        );
        broken.push(0xff);
        assert_eq!(
            convert_editor_buffer(&broken, profile, Direction::Decode).unwrap_err(),
            FileConversionError::InvalidUtf8
        );
    }

    #[test]
    fn unbroken_lines_stay_serial() {
        let input = "长".repeat(200_000);
        assert!(
            convert_lines(
                input.as_bytes(),
                Profile::Localisation,
                Direction::Decode,
                4
            )
            .is_none()
        );
        assert_eq!(
            convert_editor_buffer(input.as_bytes(), Profile::Localisation, Direction::Decode)
                .unwrap(),
            input.as_bytes()
        );
    }

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
    fn compensated_delimiters_are_valid_payloads() {
        // Canonical low 0x2F becomes '='; the historical 0x21 variant becomes
        // '/'. Markers 0x11 and 0x13 both carry low-byte compensation.
        for (triple, readable) in [
            (b"\x11=f", "是"),
            (b"\x13=R", "\u{5B2F}"),
            (b"\x11/N", "両"),
            (b"\x13/R", "\u{5B21}"),
        ] {
            let input = [b"name = \"".as_slice(), triple, b"\"\n"].concat();
            let expected = format!("name = \"{readable}\"\n");
            for profile in [Profile::Script, Profile::Localisation] {
                assert_eq!(decode_buffer(&input, profile).unwrap(), expected.as_bytes());
                for set in [EscapeSet::Paratranz, EscapeSet::DllFull] {
                    assert_eq!(encode_buffer(&input, profile, set).unwrap(), input);
                }
            }
        }
    }

    #[test]
    fn complete_file_conversion_accepts_every_encodable_bmp_scalar() {
        for code_point in 0x1000..=0xFFFF {
            let Some(character) = char::from_u32(code_point) else {
                continue;
            };
            let readable = character.to_string();
            for profile in [Profile::Script, Profile::Localisation] {
                for set in [EscapeSet::Paratranz, EscapeSet::DllFull] {
                    let encoded = encode_file(&readable, profile, set).unwrap();
                    assert_eq!(
                        decode_buffer(&encoded, profile).as_deref(),
                        Ok(readable.as_bytes()),
                        "decode U+{code_point:04X}, {profile:?}, {set:?}"
                    );
                    assert_eq!(
                        encode_buffer(&encoded, profile, set).as_deref(),
                        Ok(encoded.as_slice()),
                        "preserve U+{code_point:04X}, {profile:?}, {set:?}"
                    );
                }
            }
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
            b"name = \"\x11\"\n".as_slice(),
            b"name = \"\x13\"\n".as_slice(),
            b"# \x11\nf".as_slice(),
            b"# \x13\rf".as_slice(),
            b"\x10".as_slice(),
        ] {
            let offset = bytes
                .iter()
                .position(|byte| (0x10..=0x13).contains(byte))
                .unwrap();
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
