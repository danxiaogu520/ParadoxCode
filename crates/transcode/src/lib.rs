//! Transcoder between readable CJK text and the escaped byte form consumed by the
//! EU4 double-byte patch (EU4dll), matching the paratranz converter byte for byte.
//!
//! The patch ecosystem encodes every non-Latin-1 code point as a three-byte escape
//! `[marker, low, high]` (marker `0x10`–`0x13`, low byte first) inside files that the
//! unpatched game reads as CP1252/UTF-8 text. Two file profiles share one codec core:
//!
//! * [`Profile::Localisation`] — `localisation/*.yml` (`paratranz` "utf8eu4"): UTF-8 with
//!   BOM; every escaped byte is re-encoded through the CP1252 inverse mapping before it
//!   is written as UTF-8.
//! * [`Profile::Script`] — game script txt (`paratranz` "latin1eu4"): raw single-byte
//!   stream without BOM; the 27 CP1252-mapped characters (ä, é, €, …) stay single bytes.
//!
//! Guarantees:
//!
//! * Decoding is **universal**: escape markers are self-describing, so streams produced
//!   by the canonical 23-value set, the EU4dll 29-value superset, and historical
//!   variants all decode correctly regardless of the configured [`EscapeSet`].
//! * Encoding rejects code points the ecosystem cannot round-trip (`U+0100..=U+0FFF`
//!   and `U+10000+`) with a structured [`EncodeError`] instead of silently mangling.
//! * [`classify_file`] gates every transformation so escaped text is never encoded twice
//!   and readable text is never decoded.
//!
//! Algorithm provenance: `matanki-saito/EU4SpecialEscape` (MIT) via the paratranz
//! official gist `special-escape.js`; calibrated byte-for-byte against the deployed
//! paratranz converter and 310 real transcoded script files (2026-09-11).

mod classify;
mod codec;
mod cp1252;

pub use classify::{
    Classification, ClassificationCounts, classify_file, classify_text, classify_text_counts,
    is_raw_cjk,
};
pub use codec::{
    Decoded, InvalidUtf8, decode_file, decode_text, decode_value, encode_file, encode_text,
    file_unencodable_kind, script_roundtrip_is_canonical, unencodable_kind,
};
pub use cp1252::CP1252_MAP;

/// Bumped whenever encoding output for identical input changes (escape set adjustments,
/// compensation fixes). Consumers that persist decoded text (index caches, previews)
/// must invalidate their entries when this value changes.
pub const CODEC_VERSION: u32 = 1;

/// Selects the byte-level shape of escaped content: BOM/CRLF yml files versus raw
/// single-byte script files. See the crate docs for the exact differences.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Profile {
    /// `localisation/**/*.yml` — UTF-8 with BOM, escaped bytes re-encoded via the
    /// CP1252 inverse mapping (paratranz `utf8eu4`).
    Localisation,
    /// Game script txt — raw single bytes, no BOM, CP1252 single-byte passthrough for
    /// the 27 mapped characters (paratranz `latin1eu4`).
    Script,
}

/// The set of byte values that must not appear raw inside an escape triple, because
/// the game parser treats them as delimiters. Membership only affects which triples
/// encoding emits; decoding is universal over all sets.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum EscapeSet {
    /// Canonical paratranz / EU4SpecialEscape set (23 values, includes `0x2F`).
    /// Verified byte-exact against EDG-KTP release files and 310 workshop scripts.
    #[default]
    Paratranz,
    /// EU4dll in-game encoder superset (29 values). Rarely needed for encoding —
    /// kept for parity with the game-side transcoder.
    DllFull,
}

impl EscapeSet {
    /// The raw byte members of this set.
    #[must_use]
    pub const fn bytes(self) -> &'static [u8] {
        match self {
            Self::Paratranz => &PARATRANZ_ESCAPE_BYTES,
            Self::DllFull => &DLL_FULL_ESCAPE_BYTES,
        }
    }

    /// Whether `byte` is a member of this set.
    #[must_use]
    pub const fn contains(self, byte: u8) -> bool {
        match self {
            Self::Paratranz => contains_const(&PARATRANZ_ESCAPE_BYTES, byte),
            Self::DllFull => contains_const(&DLL_FULL_ESCAPE_BYTES, byte),
        }
    }
}

/// Canonical 23-value escape set (paratranz / EU4SpecialEscape, EU4 profile with
/// `0x2F`). Empirically confirmed by the EDG-KTP file pair and the 310-file
/// workshop script corpus.
pub const PARATRANZ_ESCAPE_BYTES: [u8; 23] = [
    0x00, 0x0A, 0x0D, 0x20, 0x22, 0x23, 0x24, 0x2F, 0x3B, 0x3D, 0x40, 0x5B, 0x5C, 0x5D, 0x5F, 0x7B,
    0x7D, 0x7E, 0x80, 0xA3, 0xA4, 0xA7, 0xBD,
];

/// EU4dll 29-value superset: the canonical set plus `0x3A 0x3C 0x3E 0x3F 0x7C 0x2A`.
pub const DLL_FULL_ESCAPE_BYTES: [u8; 29] = [
    0x00, 0x0A, 0x0D, 0x20, 0x22, 0x23, 0x24, 0x2F, 0x3B, 0x3D, 0x40, 0x5B, 0x5C, 0x5D, 0x5F, 0x7B,
    0x7D, 0x7E, 0x80, 0xA3, 0xA4, 0xA7, 0xBD, 0x3A, 0x3C, 0x3E, 0x3F, 0x7C, 0x2A,
];

const fn contains_const(set: &[u8], byte: u8) -> bool {
    let mut i = 0;
    while i < set.len() {
        if set[i] == byte {
            return true;
        }
        i += 1;
    }
    false
}

/// A code point that encoding refuses to process, with the position where it occurs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnencodableCodePoint {
    /// Byte offset of the character inside the readable input text.
    pub byte_index: usize,
    /// The offending code point.
    pub code_point: u32,
    /// Why it cannot be encoded.
    pub kind: UnencodableKind,
}

/// Why a code point is rejected by encoding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnencodableKind {
    /// `U+0100..=U+0FFF`: the whole ecosystem slices a 3-digit hex string, so the
    /// character silently changes when round-tripped (e.g. `U+0160` becomes `U+1660`).
    MangledLowPlane,
    /// `U+10000+`: 5-digit hex slicing destroys the character and the game has no
    /// glyph for it anyway.
    BeyondBmp,
}

/// Structured encoding failure: the input contains code points that the ecosystem
/// cannot round-trip. Nothing is written; fix or remove the listed characters first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncodeError {
    /// Every rejected code point, in input order.
    pub unencodable: Vec<UnencodableCodePoint>,
}

impl std::fmt::Display for EncodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "text contains {} code point(s) the EU4 transcoder cannot round-trip",
            self.unencodable.len()
        )?;
        for point in &self.unencodable {
            let reason = match point.kind {
                UnencodableKind::MangledLowPlane => "U+0100..U+0FFF is mangled by the ecosystem",
                UnencodableKind::BeyondBmp => "beyond-BMP characters are destroyed",
            };
            write!(
                f,
                "; U+{:04X} at byte {}: {}",
                point.code_point, point.byte_index, reason
            )?;
        }
        Ok(())
    }
}

impl std::error::Error for EncodeError {}
