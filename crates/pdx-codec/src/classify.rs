//! The no-double-transcode gate: decides whether a buffer is readable text, escaped
//! text, a mix of both, or a transformation-free fixed point, before any encode or
//! decode is allowed to run.

use crate::Profile;
use crate::cp1252;

/// Content classification of a buffer. Every transformation entry point must check
/// this first: encode only accepts `Readable`/`Ascii`, decode only accepts `Escaped`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Classification {
    /// Raw CJK text that the game cannot display as-is. Encode (never decode).
    Readable,
    /// Escaped triples with no raw CJK and no broken markers. Decode (never encode).
    Escaped,
    /// Both forms present, or stray/broken markers below the escape threshold.
    /// Refuse both transformations and report a diagnostic.
    Mixed,
    /// No CJK and no escape sequences (pure ASCII or Latin-1 script bytes): a
    /// fixed point of the codec, no transformation needed or performed.
    Ascii,
}

/// Classifies an already-decoded Unicode buffer (editor view, LSP text sync).
/// Marker scanning mirrors the decoder's consumption so triple members never
/// miscount as content.
#[must_use]
pub fn classify_text(text: &str) -> Classification {
    let code_points: Vec<u32> = text.chars().map(u32::from).collect();
    classify_code_points(&code_points)
}

/// The scanner counts behind [`classify_text`], for callers that need finer
/// distinctions than the four-way category (for example reporting an
/// otherwise-clean escaped file with one damaged triple as a repairable
/// marker fault rather than a mixed-encoding error).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct ClassificationCounts {
    /// Complete escape triples found.
    pub escaped_triples: usize,
    /// Escape markers without two payload characters behind them.
    pub broken_markers: usize,
    /// Raw CJK-range characters.
    pub raw_cjk: usize,
}

/// Returns the [`ClassificationCounts`] for an already-decoded buffer.
#[must_use]
pub fn classify_text_counts(text: &str) -> ClassificationCounts {
    let code_points: Vec<u32> = text.chars().map(u32::from).collect();
    count_code_points(&code_points)
}

fn count_code_points(code_points: &[u32]) -> ClassificationCounts {
    let mut counts = ClassificationCounts::default();
    let mut index = 0;
    while index < code_points.len() {
        let code_point = code_points[index];
        if (0x10..=0x13).contains(&code_point) {
            if code_points.len() - index >= 3 {
                counts.escaped_triples += 1;
                index += 3;
                continue;
            }
            counts.broken_markers += 1;
        } else if is_raw_cjk(code_point) {
            counts.raw_cjk += 1;
        }
        index += 1;
    }
    counts
}

/// Classifies on-disk bytes for `profile`:
///
/// * `Localisation` must be valid UTF-8; anything else is `Mixed` (refuse to touch).
/// * `Script` accepts two shapes: valid UTF-8 (a readable or already-decoded buffer)
///   or a CP1252 byte stream whose escape markers are checked byte-wise. Escaped
///   script files are almost never valid UTF-8, so the two shapes separate cleanly.
#[must_use]
pub fn classify_file(bytes: &[u8], profile: Profile) -> Classification {
    match profile {
        Profile::Localisation => match std::str::from_utf8(bytes) {
            Ok(text) => classify_text(text),
            Err(_) => Classification::Mixed,
        },
        Profile::Script => {
            if let Ok(text) = std::str::from_utf8(bytes) {
                classify_text(text)
            } else {
                let code_points: Vec<u32> = bytes
                    .iter()
                    .map(|byte| cp1252::byte_to_char(*byte))
                    .collect();
                classify_code_points(&code_points)
            }
        }
    }
}

fn classify_code_points(code_points: &[u32]) -> Classification {
    let counts = count_code_points(code_points);
    let ClassificationCounts {
        escaped_triples: triples,
        broken_markers,
        raw_cjk,
    } = counts;
    if triples >= 3 && broken_markers == 0 && raw_cjk == 0 {
        Classification::Escaped
    } else if raw_cjk > 0 && triples == 0 && broken_markers == 0 {
        Classification::Readable
    } else if triples == 0 && broken_markers == 0 && raw_cjk == 0 {
        Classification::Ascii
    } else {
        Classification::Mixed
    }
}

/// CJK and adjacent ranges that only ever appear in readable form: escaped text
/// expresses them as triples instead. Superset of the design-doc ranges, extended
/// with Hangul and the supplementary ideograph planes.
pub fn is_raw_cjk(code_point: u32) -> bool {
    matches!(code_point,
        0x2E80..=0x9FFF          // CJK radicals, kana, unified ideographs, Yi
        | 0xAC00..=0xD7AF        // Hangul syllables
        | 0xF900..=0xFAFF        // CJK compatibility ideographs
        | 0xFF00..=0xFFEF        // fullwidth forms
        | 0x20000..=0x2FA1F      // supplementary planes B-F + compat supplement
        | 0x30000..=0x3134F      // plane G+
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EscapeSet;
    use crate::codec::{decode_text, encode_text};

    fn escaped(text: &str) -> String {
        encode_text(text, EscapeSet::Paratranz).unwrap()
    }

    #[test]
    fn ascii_is_a_fixed_point() {
        assert_eq!(
            classify_text("l_english:\r\n KEY:0 \"plain\""),
            Classification::Ascii
        );
        assert_eq!(
            classify_file(b"plain ascii", Profile::Script),
            Classification::Ascii
        );
        // Latin-1 script bytes (no UTF-8, no triples) are also a fixed point.
        assert_eq!(
            classify_file(b"# K\xf6nig \xe9", Profile::Script),
            Classification::Ascii
        );
    }

    #[test]
    fn readable_cjk() {
        assert_eq!(
            classify_text("\u{4E2D}\u{6587}abc"),
            Classification::Readable
        );
        // Readable CJK saved into a script path as UTF-8.
        assert_eq!(
            classify_file("\u{4E2D}\u{6587}".as_bytes(), Profile::Script),
            Classification::Readable
        );
    }

    #[test]
    fn escaped_text() {
        let encoded = escaped("\u{4E2D}\u{6587}\u{6D4B}\u{8BD5}");
        assert_eq!(classify_text(&encoded), Classification::Escaped);
        assert_eq!(
            classify_file(encoded.as_bytes(), Profile::Localisation),
            Classification::Escaped
        );
    }

    #[test]
    fn mixed_forms() {
        let encoded = escaped("\u{4E2D}\u{6587}\u{6D4B}\u{8BD5}");
        // Escaped triples plus raw CJK pasted in.
        assert_eq!(
            classify_text(&format!("{encoded}\u{6D4B}")),
            Classification::Mixed
        );
        // Readable text plus a stray control marker.
        assert_eq!(classify_text("\u{4E2D}\u{0010}"), Classification::Mixed);
        // Below the triple threshold: two triples alone are not evidence.
        assert_eq!(
            classify_text(&escaped("\u{4E2D}\u{6587}")),
            Classification::Mixed
        );
    }

    #[test]
    fn classification_matches_decode_consumption() {
        // A triple member that would look CJK if counted as content is skipped,
        // mirroring how the decoder consumes it.
        let encoded = escaped("\u{4E2D}\u{6587}\u{6D4B}\u{8BD5}");
        let round_tripped = decode_text(&encoded);
        assert_eq!(classify_text(&round_tripped.text), Classification::Readable);
    }

    #[test]
    fn invalid_utf8_localisation_is_mixed() {
        // 0xEF 0xBB is a truncated three-byte sequence: never valid UTF-8.
        assert_eq!(
            classify_file(&[0xEF, 0xBB], Profile::Localisation),
            Classification::Mixed
        );
    }
}
