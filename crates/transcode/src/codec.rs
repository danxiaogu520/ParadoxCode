//! The transcoder core: text-level and file-level encode/decode.
//!
//! Text level ([`encode_text`] / [`decode_text`]) operates on Unicode strings and is
//! the layer used by localisation yml buffers: escape bytes appear as characters via
//! the CP1252 inverse mapping. File level ([`encode_file`] / [`decode_file`]) adds the
//! byte-level script profile, where escapes are raw single bytes and non-escape bytes
//! decode through CP1252.

use crate::cp1252;
use crate::{EncodeError, EscapeSet, Profile, UnencodableCodePoint, UnencodableKind};

/// Successful decode: the readable text plus the positions of broken escape
/// sequences (an escape marker with fewer than two followers), which are passed
/// through untouched.
///
/// `broken_sequences` holds byte offsets into the *input*: character offsets for the
/// text and localisation paths, byte offsets for the script path.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Decoded {
    /// The decoded readable text.
    pub text: String,
    /// Input positions of orphan escape markers that were passed through as-is.
    pub broken_sequences: Vec<usize>,
}

/// Decoding a localisation yml failed because the file is not valid UTF-8. Escaped
/// yml files are always valid UTF-8, so this indicates damage, never escape content.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidUtf8;

impl std::fmt::Display for InvalidUtf8 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("localisation file is not valid UTF-8")
    }
}

impl std::error::Error for InvalidUtf8 {}

/// Encodes readable text into escaped form as a Unicode string (localisation yml
/// layer): every escapable code point becomes the three characters
/// `[marker, low, high]`, with `low`/`high` carried through the CP1252 inverse
/// mapping. The BOM (`U+FEFF`) and everything below `U+0100` passes through.
///
/// # Errors
/// Returns every code point in `U+0100..=U+0FFF` or `U+10000+` via [`EncodeError`];
/// nothing is produced for rejected input.
pub fn encode_text(input: &str, escape_set: EscapeSet) -> Result<String, EncodeError> {
    let mut output = String::with_capacity(input.len() * 3);
    let mut unencodable = Vec::new();
    for (byte_index, character) in input.char_indices() {
        let code_point = u32::from(character);
        if let Some(kind) = unencodable_kind(code_point) {
            unencodable.push(UnencodableCodePoint {
                byte_index,
                code_point,
                kind,
            });
            continue;
        }
        if is_passthrough(code_point) {
            output.push(character);
            continue;
        }
        let (marker, low, high) = escape_triple(code_point, escape_set);
        push_char(&mut output, u32::from(marker));
        push_char(&mut output, cp1252::byte_to_char(low));
        push_char(&mut output, cp1252::byte_to_char(high));
    }
    finish(output, unencodable)
}

/// Decodes escaped text into readable form (universal over every escape-set variant:
/// markers carry their own compensation). Non-marker characters pass through
/// unchanged, so readable input is a fixed point.
#[must_use]
pub fn decode_text(input: &str) -> Decoded {
    let positions: Vec<(usize, char)> = input.char_indices().collect();
    let mut text = String::with_capacity(input.len());
    let mut broken_sequences = Vec::new();
    let mut index = 0;
    while index < positions.len() {
        let (byte_index, character) = positions[index];
        let code_point = u32::from(character);
        if is_marker_code_point(code_point) && positions.len() - index >= 3 {
            let low = cp1252::char_to_byte(u32::from(positions[index + 1].1));
            let high = cp1252::char_to_byte(u32::from(positions[index + 2].1));
            push_char(&mut text, reconstruct(code_point, low, high));
            index += 3;
            continue;
        }
        if is_marker_code_point(code_point) {
            broken_sequences.push(byte_index);
        }
        text.push(character);
        index += 1;
    }
    Decoded {
        text,
        broken_sequences,
    }
}

/// Decodes a single localisation *value* for display: if it contains any escape
/// marker it is escaped content, so decode it; otherwise return it unchanged.
///
/// This is the value-level gate used by the localisation database (previews,
/// mission rendering). Unlike [`crate::classify_text`] it applies no triple
/// threshold, because a marker inside a value never legitimately appears in
/// readable text and even a one-triple value must decode.
#[must_use]
pub fn decode_value(value: &str) -> String {
    if !value
        .chars()
        .any(|character| is_marker_code_point(u32::from(character)))
    {
        return value.to_owned();
    }
    decode_text(value).text
}

/// Encodes readable text into the on-disk escaped bytes for `profile`:
/// localisation files get the CP1252-remapped UTF-8 form, script files get raw
/// single bytes with the 27 mapped CP1252 characters kept as single bytes.
///
/// # Errors
/// Same rejection rules as [`encode_text`].
pub fn encode_file(
    input: &str,
    profile: Profile,
    escape_set: EscapeSet,
) -> Result<Vec<u8>, EncodeError> {
    match profile {
        Profile::Localisation => encode_text(input, escape_set).map(String::into_bytes),
        Profile::Script => {
            let mut bytes = Vec::with_capacity(input.len() * 3);
            let mut unencodable = Vec::new();
            for (byte_index, character) in input.char_indices() {
                let code_point = u32::from(character);
                // The 27 CP1252 characters stay single bytes before any rejection
                // check: eight of them (Š, œ, …) sit inside U+0100..=U+0FFF and
                // would otherwise be refused, yet the single-byte form round-trips
                // exactly — required by the real script corpus.
                if code_point < 0x100 {
                    bytes.push(code_point as u8);
                } else if code_point == 0xFEFF {
                    bytes.extend_from_slice("\u{FEFF}".as_bytes());
                } else if let Some(single) = cp1252::mapped_byte(code_point) {
                    bytes.push(single);
                } else if let Some(kind) = unencodable_kind(code_point) {
                    unencodable.push(UnencodableCodePoint {
                        byte_index,
                        code_point,
                        kind,
                    });
                    continue;
                } else {
                    let (marker, low, high) = escape_triple(code_point, escape_set);
                    bytes.extend_from_slice(&[marker, low, high]);
                }
            }
            finish(bytes, unencodable)
        }
    }
}

/// Decodes on-disk escaped bytes for `profile` into readable text. Localisation
/// input must be valid UTF-8; script bytes decode one byte at a time through
/// CP1252, so any byte stream decodes.
///
/// # Errors
/// Returns [`InvalidUtf8`] for non-UTF-8 localisation input only; the script
/// profile never fails.
pub fn decode_file(bytes: &[u8], profile: Profile) -> Result<Decoded, InvalidUtf8> {
    match profile {
        Profile::Localisation => {
            let input = std::str::from_utf8(bytes).map_err(|_| InvalidUtf8)?;
            Ok(decode_text(input))
        }
        Profile::Script => {
            let mut text = String::with_capacity(bytes.len());
            let mut broken_sequences = Vec::new();
            let mut index = 0;
            while index < bytes.len() {
                let byte = bytes[index];
                if is_marker_byte(byte) && bytes.len() - index >= 3 {
                    let sp = reconstruct(u32::from(byte), bytes[index + 1], bytes[index + 2]);
                    push_char(&mut text, sp);
                    index += 3;
                    continue;
                }
                if is_marker_byte(byte) {
                    broken_sequences.push(index);
                }
                push_char(&mut text, cp1252::byte_to_char(byte));
                index += 1;
            }
            Ok(Decoded {
                text,
                broken_sequences,
            })
        }
    }
}

fn finish<T>(output: T, unencodable: Vec<UnencodableCodePoint>) -> Result<T, EncodeError> {
    if unencodable.is_empty() {
        Ok(output)
    } else {
        Err(EncodeError { unencodable })
    }
}

/// Whether `code_point` never takes the escape-triple path: Latin-1 range plus the
/// BOM, which is a structural character rather than content.
fn is_passthrough(code_point: u32) -> bool {
    code_point < 0x100 || code_point == 0xFEFF
}

/// Whether `code_point` must be rejected by encoding, and why. `U+FEFF` (the BOM)
/// is a structural character and passes through below this check.
pub fn unencodable_kind(code_point: u32) -> Option<UnencodableKind> {
    if code_point < 0x100 || code_point == 0xFEFF {
        None
    } else if code_point <= 0xFFF {
        Some(UnencodableKind::MangledLowPlane)
    } else if code_point >= 0x10000 {
        Some(UnencodableKind::BeyondBmp)
    } else {
        None
    }
}

/// The profile-aware form of [`unencodable_kind`]: the script profile accepts
/// the 27 CP1252-mapped characters (`Š`, `œ`, `€`, …) as single bytes even when
/// they fall in `U+0100..=U+0FFF`, so they are only refused for localisation
/// files. This mirrors the order of checks in [`encode_file`].
#[must_use]
pub fn file_unencodable_kind(code_point: u32, profile: Profile) -> Option<UnencodableKind> {
    if profile == Profile::Script && cp1252::mapped_byte(code_point).is_some() {
        return None;
    }
    unencodable_kind(code_point)
}

/// Whether re-encoding `text` (a script-profile document already read through
/// the CP1252 char layer) with the canonical escape set reproduces it exactly.
///
/// Decoding is universal over escape-set variants, so a file written by a
/// historical encoder still decodes correctly — but re-saving it normalizes the
/// triples to the canonical set. This predicate detects that divergence:
/// decode, re-encode as raw script bytes, map the bytes back to characters, and
/// compare with the original text. `false` also covers inputs whose decoded
/// form cannot be re-encoded at all (damaged triples), which is fine for the
/// caller's Hint-level "re-saving will normalize this file" report.
#[must_use]
pub fn script_roundtrip_is_canonical(text: &str) -> bool {
    let decoded = decode_text(text);
    let Ok(bytes) = encode_file(&decoded.text, Profile::Script, EscapeSet::Paratranz) else {
        return false;
    };
    let mut roundtrip = String::with_capacity(bytes.len());
    for byte in bytes {
        let code_point = cp1252::byte_to_char(byte);
        roundtrip.push(char::from_u32(code_point).unwrap_or('\u{FFFD}'));
    }
    roundtrip == text
}

/// Splits a 4-digit code point into its escape triple: marker selection from escape
/// set membership, then the `low += 0x0E` / `high -= 0x09` compensation so that set
/// members never appear raw inside the triple.
fn escape_triple(code_point: u32, escape_set: EscapeSet) -> (u8, u8, u8) {
    let low = (code_point & 0xFF) as u8;
    let high = (code_point >> 8) as u8;
    let mut marker = 0x10u8;
    if escape_set.contains(high) {
        marker += 2;
    }
    if escape_set.contains(low) {
        marker += 1;
    }
    let low = match marker {
        0x11 | 0x13 => low.wrapping_add(0x0E),
        _ => low,
    };
    let high = match marker {
        0x12 | 0x13 => high.wrapping_sub(0x09),
        _ => high,
    };
    (marker, low, high)
}

/// Rebuilds the code point from a triple: `sp = (high << 8) | low`, then undo the
/// compensation encoded in the marker. Markers are self-describing, which is what
/// makes decoding independent of any escape set.
fn reconstruct(marker: u32, low: u8, high: u8) -> u32 {
    let mut sp = (u32::from(high) << 8) | u32::from(low);
    match marker {
        0x11 => sp = sp.wrapping_sub(0x0E),
        0x12 => sp = sp.wrapping_add(0x900),
        0x13 => sp = sp.wrapping_add(0x8F2),
        _ => {}
    }
    sp
}

fn is_marker_code_point(code_point: u32) -> bool {
    (0x10..=0x13).contains(&code_point)
}

fn is_marker_byte(byte: u8) -> bool {
    (0x10..=0x13).contains(&byte)
}

/// Pushes `code_point` as a character, substituting `U+FFFD` for the surrogate
/// range, which escape bytes can express but UTF-8 cannot.
fn push_char(text: &mut String, code_point: u32) {
    text.push(char::from_u32(code_point).unwrap_or('\u{FFFD}'));
}

#[cfg(test)]
mod tests {
    use super::*;

    const P: EscapeSet = EscapeSet::Paratranz;

    fn decoded_text(input: &str) -> String {
        decode_text(input).text
    }

    #[test]
    fn plain_passthrough() {
        assert_eq!(
            encode_text("hello world\r\n", P).unwrap(),
            "hello world\r\n"
        );
        assert_eq!(decoded_text("hello world\r\n"), "hello world\r\n");
        assert_eq!(encode_file("abc", Profile::Script, P).unwrap(), b"abc");
    }

    #[test]
    fn bom_passthrough() {
        let input = "\u{FEFF}中";
        let encoded = encode_text(input, P).unwrap();
        assert!(encoded.starts_with('\u{FEFF}'));
        assert_eq!(decode_text(&encoded).text, input);
    }

    #[test]
    fn script_roundtrip_detects_legacy_escape_variants() {
        // A canonical script file re-encodes to itself through the char layer.
        let readable = "dynasty = \"大明王朝\"\r\n";
        let canonical_bytes = encode_file(readable, Profile::Script, EscapeSet::Paratranz).unwrap();
        let canonical_text: String = canonical_bytes
            .iter()
            .map(|byte| char::from_u32(cp1252::byte_to_char(*byte)).expect("scalar"))
            .collect();
        assert!(script_roundtrip_is_canonical(&canonical_text));
        // A file produced with the 29-value superset decodes fine but no longer
        // re-encodes to itself under the canonical set — the Hint case. 尺
        // (U+5C3A) has low byte 0x3A, escaped only by the legacy superset.
        let legacy_bytes = encode_file("尺", Profile::Script, EscapeSet::DllFull).unwrap();
        let legacy_text: String = legacy_bytes
            .iter()
            .map(|byte| char::from_u32(cp1252::byte_to_char(*byte)).expect("scalar"))
            .collect();
        let canonical_bytes = encode_file("尺", Profile::Script, EscapeSet::Paratranz).unwrap();
        let canonical_text: String = canonical_bytes
            .iter()
            .map(|byte| char::from_u32(cp1252::byte_to_char(*byte)).expect("scalar"))
            .collect();
        assert_ne!(
            legacy_text, canonical_text,
            "fixture must pick a diverging byte"
        );
        assert!(!script_roundtrip_is_canonical(&legacy_text));
        // Orphan markers pass through decode unchanged, so they stay canonical.
        assert!(script_roundtrip_is_canonical("a\u{0010}\u{0010}"));
        // Plain ASCII is a fixed point of both directions.
        assert!(script_roundtrip_is_canonical("plain ascii\r\n"));
    }

    #[test]
    fn known_triple_from_release_file() {
        // U+5E8A (low 0x8A not in set, high 0x5E not in set) -> [0x10, U+0160, '^']
        // observed verbatim in the EDG-KTP release file.
        let encoded = encode_text("\u{5E8A}", P).unwrap();
        assert_eq!(encoded, "\u{0010}\u{0160}^");
        assert_eq!(decoded_text(&encoded), "\u{5E8A}");
    }

    #[test]
    fn high_byte_compensation() {
        // U+5F02: high 0x5F in set -> marker 0x12, high 0x5F-9 = 0x56.
        let encoded = encode_text("\u{5F02}", P).unwrap();
        let cps: Vec<u32> = encoded.chars().map(u32::from).collect();
        assert_eq!(cps, [0x12, 0x02, 0x56]);
        assert_eq!(decoded_text(&encoded), "\u{5F02}");
    }

    #[test]
    fn low_byte_compensation() {
        // U+4E3B (主): low 0x3B in set -> marker 0x11, low 0x3B+0x0E = 0x49.
        let encoded = encode_text("\u{4E3B}", P).unwrap();
        let cps: Vec<u32> = encoded.chars().map(u32::from).collect();
        assert_eq!(cps, [0x11, 0x49, 0x4E]);
        assert_eq!(decoded_text(&encoded), "\u{4E3B}");
    }

    #[test]
    fn both_byte_compensation() {
        // U+3B22: low 0x22 and high 0x3B in set -> marker 0x13.
        let encoded = encode_text("\u{3B22}", P).unwrap();
        let cps: Vec<u32> = encoded.chars().map(u32::from).collect();
        assert_eq!(cps, [0x13, 0x30, 0x32]);
        assert_eq!(decoded_text(&encoded), "\u{3B22}");
    }

    #[test]
    fn marker_valued_triple_members_round_trip() {
        // Triple members can themselves be marker bytes; decoding consumes
        // positionally, so the stream stays unambiguous.
        for code_point in [0x1061, 0x1010, 0x6110, 0x6113, 0x1361] {
            let character = char::from_u32(code_point).unwrap().to_string();
            let encoded = encode_text(&character, P).unwrap();
            assert_eq!(decoded_text(&encoded), character, "U+{code_point:04X}");
        }
    }

    #[test]
    fn escape_sets_diverge_but_decode_agrees() {
        // 为 U+4E3A (low 0x3A) and 个 U+4E2A (low 0x2A): paratranz leaves them raw,
        // dll-full escapes them — the calibration lesson from EDG-KTP.
        for code_point in [0x4E3A, 0x4E2A] {
            let character = char::from_u32(code_point).unwrap().to_string();
            let canonical = encode_file(&character, Profile::Script, P).unwrap();
            let dll = encode_file(&character, Profile::Script, EscapeSet::DllFull).unwrap();
            assert_ne!(canonical, dll, "U+{code_point:04X}");
            assert_eq!(
                decode_file(&canonical, Profile::Script).unwrap().text,
                character
            );
            assert_eq!(decode_file(&dll, Profile::Script).unwrap().text, character);
        }
    }

    #[test]
    fn script_profile_keeps_cp1252_single_bytes() {
        let bytes = encode_file("\u{2018}x\u{20AC}", Profile::Script, P).unwrap();
        assert_eq!(bytes, vec![0x91, b'x', 0x80]);
    }

    #[test]
    fn rejects_unencodable_code_points() {
        let error = encode_text("前\u{0160}后\u{1F600}", P).unwrap_err();
        assert_eq!(
            error.unencodable,
            vec![
                UnencodableCodePoint {
                    byte_index: "前".len(),
                    code_point: 0x0160,
                    kind: UnencodableKind::MangledLowPlane,
                },
                UnencodableCodePoint {
                    byte_index: "前".len() + "\u{0160}".len() + "后".len(),
                    code_point: 0x1F600,
                    kind: UnencodableKind::BeyondBmp,
                },
            ]
        );
        // Script profile: the low-plane CP1252 letters stay single bytes, so only
        // unmapped low-plane characters are rejected.
        assert!(encode_file("\u{0100}", Profile::Script, P).is_err());
        assert_eq!(
            encode_file("\u{0160}", Profile::Script, P).unwrap(),
            vec![0x8A]
        );
    }

    #[test]
    fn broken_marker_passes_through() {
        let decoded = decode_text("a\u{0010}");
        assert_eq!(decoded.text, "a\u{0010}");
        assert_eq!(decoded.broken_sequences, vec![1]);

        let decoded = decode_file(&[b'a', 0x10], Profile::Script).unwrap();
        assert_eq!(decoded.text, "a\u{0010}");
        assert_eq!(decoded.broken_sequences, vec![1]);
    }

    #[test]
    fn localisation_decode_requires_utf8() {
        assert!(decode_file(&[0xEF, 0xBB], Profile::Localisation).is_err());
    }
}
