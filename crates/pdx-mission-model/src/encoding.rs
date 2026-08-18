//! Lossless file-encoding handling for legacy mission files.
//!
//! Most EU4 files are UTF-8, but legacy files (and some mods) use the
//! single-byte Windows-1252 / Latin-1 family — real-world example:
//! `DOM_French_Missions.txt` (v1.37.5) is pure ASCII except one `0xF4` byte
//! (`ô` in `"Basse-Côte"`).
//!
//! Loading such a file must not lose bytes (`String::from_utf8_lossy` would
//! replace them with U+FFFD), and saving must not change the encoding of
//! untouched content (transcoding the whole file would break the editor's
//! byte-fidelity promise). We therefore:
//!
//! - decode strictly as UTF-8 when the file is valid UTF-8 (current behavior);
//! - otherwise decode every byte through the Windows-1252 table (identity for
//!   its five undefined bytes). The mapping is bijective, so an untouched
//!   save re-encodes to the exact original bytes;
//! - remember the encoding per session and re-encode on save. Characters with
//!   no single-byte representation (e.g. CJK typed into a legacy file) are
//!   rejected with an error instead of silently corrupting the file.

use std::fmt;

/// The encoding of a loaded mission file.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FileEncoding {
    /// Valid UTF-8 (current behavior; files written by modern tools).
    Utf8,
    /// Single-byte legacy: Windows-1252, with identity fallback for its
    /// undefined bytes so every byte round-trips.
    Cp1252,
}

/// Windows-1252 mappings for `0x80..=0x9F`. `None` entries are undefined in
/// Windows-1252 and fall back to identity, which keeps the byte mapping
/// bijective.
const CP1252_HIGH: [Option<char>; 32] = [
    Some('\u{20ac}'), // 0x80 €
    None,             // 0x81
    Some('\u{201a}'), // 0x82 ‚
    Some('\u{0192}'), // 0x83 ƒ
    Some('\u{201e}'), // 0x84 „
    Some('\u{2026}'), // 0x85 …
    Some('\u{2020}'), // 0x86 †
    Some('\u{2021}'), // 0x87 ‡
    Some('\u{02c6}'), // 0x88 ˆ
    Some('\u{2030}'), // 0x89 ‰
    Some('\u{0160}'), // 0x8a Š
    Some('\u{2039}'), // 0x8b ‹
    Some('\u{0152}'), // 0x8c Œ
    None,             // 0x8d
    Some('\u{017d}'), // 0x8e Ž
    None,             // 0x8f
    None,             // 0x90
    Some('\u{2018}'), // 0x91 '
    Some('\u{2019}'), // 0x92 '
    Some('\u{201c}'), // 0x93 "
    Some('\u{201d}'), // 0x94 "
    Some('\u{2022}'), // 0x95 •
    Some('\u{2013}'), // 0x96 –
    Some('\u{2014}'), // 0x97 —
    Some('\u{02dc}'), // 0x98 ˜
    Some('\u{2122}'), // 0x99 ™
    Some('\u{0161}'), // 0x9a š
    Some('\u{203a}'), // 0x9b ›
    Some('\u{0153}'), // 0x9c œ
    None,             // 0x9d
    Some('\u{017e}'), // 0x9e ž
    Some('\u{0178}'), // 0x9f Ÿ
];

fn char_from_cp1252(byte: u8) -> char {
    match byte {
        0x00..=0x7f => char::from(byte),
        0x80..=0x9f => CP1252_HIGH[usize::from(byte - 0x80)].unwrap_or_else(|| char::from(byte)),
        _ => char::from(byte),
    }
}

fn byte_from_cp1252(ch: char) -> Option<u8> {
    if let Some(pos) = CP1252_HIGH.iter().position(|c| *c == Some(ch)) {
        return Some(0x80 + pos as u8);
    }
    let code = u32::from(ch);
    if code < 0x100 { Some(code as u8) } else { None }
}

/// Decodes mission-file bytes losslessly: valid UTF-8 stays UTF-8, anything
/// else is decoded through the Windows-1252 table (bijective).
#[must_use]
pub fn decode_bytes(bytes: &[u8]) -> (String, FileEncoding) {
    match std::str::from_utf8(bytes) {
        Ok(text) => (text.to_owned(), FileEncoding::Utf8),
        Err(_) => {
            let text: String = bytes.iter().map(|&b| char_from_cp1252(b)).collect();
            (text, FileEncoding::Cp1252)
        }
    }
}

/// A character that cannot be represented in the file's single-byte encoding.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EncodingError {
    pub character: char,
}

impl fmt::Display for EncodingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "character `{}` (U+{:04X}) cannot be encoded in this file's legacy \
             single-byte encoding; remove it or save as a UTF-8 file",
            self.character, self.character as u32
        )
    }
}

impl std::error::Error for EncodingError {}

/// Re-encodes edited content to the file's encoding. `Utf8` is a pass-through;
/// `Cp1252` fails on characters with no single-byte representation.
pub fn encode_text(text: &str, encoding: FileEncoding) -> Result<Vec<u8>, EncodingError> {
    match encoding {
        FileEncoding::Utf8 => Ok(text.as_bytes().to_vec()),
        FileEncoding::Cp1252 => text
            .chars()
            .map(byte_from_cp1252)
            .collect::<Option<Vec<u8>>>()
            .ok_or_else(|| EncodingError {
                character: text
                    .chars()
                    .find(|c| byte_from_cp1252(*c).is_none())
                    .expect("an unmappable character exists"),
            }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_files_pass_through_unchanged() {
        let bytes = "mission = { icon = mission_x }".as_bytes();
        let (text, encoding) = decode_bytes(bytes);
        assert_eq!(encoding, FileEncoding::Utf8);
        assert_eq!(text, "mission = { icon = mission_x }");
        assert_eq!(encode_text(&text, encoding).unwrap(), bytes);
    }

    #[test]
    fn every_byte_round_trips_through_cp1252() {
        // The mapping must be bijective over all 256 bytes so untouched saves
        // are byte-identical, including Windows-1252's undefined bytes.
        let bytes: Vec<u8> = (0..=255).collect();
        let (text, encoding) = decode_bytes(&bytes);
        assert_eq!(encoding, FileEncoding::Cp1252);
        assert_eq!(encode_text(&text, encoding).unwrap(), bytes);
    }

    #[test]
    fn legacy_french_file_decodes_and_reencodes() {
        // DOM_French_Missions.txt style: ASCII plus one 0xF4 byte (ô).
        let bytes = b"legacy_tree = {\n\tslot = 1\n\tname = \"Basse-C\xf4te\"\n}\n".to_vec();
        let (text, encoding) = decode_bytes(&bytes);
        assert_eq!(encoding, FileEncoding::Cp1252);
        assert!(text.contains("Basse-Côte"), "{text}");
        assert_eq!(encode_text(&text, encoding).unwrap(), bytes);
    }

    #[test]
    fn unmappable_characters_are_rejected_not_corrupted() {
        let err = encode_text("中文任务", FileEncoding::Cp1252).unwrap_err();
        assert_eq!(err.character, '中');
        // U+0081 (an undefined CP1252 byte) still round-trips via identity.
        let (text, encoding) = decode_bytes(&[0x81]);
        assert_eq!(text, "\u{81}");
        assert_eq!(encode_text(&text, encoding).unwrap(), vec![0x81]);
    }
}
