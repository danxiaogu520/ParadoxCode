//! Detection and lossless encoding of on-disk source text.

use std::borrow::Cow;

use encoding_rs::{Encoding, UTF_16BE, UTF_16LE, WINDOWS_1252};
use rules::SourceEncoding;

/// The encoding detected from a source file's bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiskSourceEncoding {
    /// UTF-8, whose optional BOM is retained in the decoded text.
    Utf8,
    /// Little-endian UTF-16 with a BOM.
    Utf16Le,
    /// Big-endian UTF-16 with a BOM.
    Utf16Be,
    /// Windows-1252 without a BOM.
    Windows1252,
}

impl DiskSourceEncoding {
    /// Decodes source bytes using their BOM or UTF-8, then the profile's fallback.
    /// UTF-16 BOMs are consumed so decoded positions match the editor buffer.
    pub fn decode(bytes: &[u8], fallback: SourceEncoding) -> Option<(Cow<'_, str>, Self)> {
        if let Some((decoder, encoding)) = utf16_encoding(bytes) {
            // The UTF-32LE BOM also starts with FF FE.
            if bytes.starts_with(b"\xff\xfe\0\0") {
                return None;
            }
            let text = decoder.decode_without_bom_handling_and_without_replacement(&bytes[2..])?;
            return Some((text, encoding));
        }
        if let Ok(text) = std::str::from_utf8(bytes) {
            return Some((Cow::Borrowed(text), Self::Utf8));
        }
        if fallback == SourceEncoding::Windows1252
            && !bytes.starts_with(b"\xef\xbb\xbf")
            && looks_like_legacy_text(bytes)
        {
            let text = WINDOWS_1252.decode_without_bom_handling_and_without_replacement(bytes)?;
            return Some((text, Self::Windows1252));
        }
        None
    }

    /// Encodes text in the original source encoding, including the UTF-16 BOM.
    /// Returns `None` if Windows-1252 cannot represent the text losslessly.
    pub fn encode(self, text: &str) -> Option<Cow<'_, [u8]>> {
        match self {
            Self::Utf8 => Some(Cow::Borrowed(text.as_bytes())),
            Self::Utf16Le | Self::Utf16Be => {
                let little_endian = self == Self::Utf16Le;
                let mut bytes = Vec::from(if little_endian {
                    [0xff, 0xfe]
                } else {
                    [0xfe, 0xff]
                });
                for unit in text.encode_utf16() {
                    bytes.extend_from_slice(&if little_endian {
                        unit.to_le_bytes()
                    } else {
                        unit.to_be_bytes()
                    });
                }
                Some(Cow::Owned(bytes))
            }
            Self::Windows1252 => {
                let (bytes, _, had_errors) = WINDOWS_1252.encode(text);
                (!had_errors).then_some(bytes)
            }
        }
    }
}

fn utf16_encoding(bytes: &[u8]) -> Option<(&'static Encoding, DiskSourceEncoding)> {
    match bytes {
        [0xff, 0xfe, ..] => Some((UTF_16LE, DiskSourceEncoding::Utf16Le)),
        [0xfe, 0xff, ..] => Some((UTF_16BE, DiskSourceEncoding::Utf16Be)),
        _ => None,
    }
}

pub(crate) fn looks_like_legacy_text(bytes: &[u8]) -> bool {
    utf16_encoding(bytes).is_none()
        && !bytes.contains(&0)
        && bytes
            .iter()
            .any(|byte| matches!(*byte, b'=' | b'{' | b'}' | b'#' | b'\n' | b':'))
}
