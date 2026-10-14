//! Transcode diagnostics for the EU4dll double-byte patch ecosystem.
//!
//! The same whole-file rules apply to comments, identifiers and strings. Valid
//! escaped/readable mixtures are supported. Damaged triples report their input
//! position, and unencodable characters are checked across the complete file.

use crate::support::ParsedInput;
use crate::types::{CancellationToken, Cancelled, Diagnostic, DiagnosticCode, Severity};
use parser::FileFormat;
use text::{TextRange, TextSize};

/// Bound per file, mirroring the other bounded diagnostics passes.
const MAX_TRANSCODE_DIAGNOSTICS: usize = 32;

pub(crate) fn transcode_diagnostics(
    input: &ParsedInput,
    cancellation: &CancellationToken,
) -> Result<Vec<Diagnostic>, Cancelled> {
    cancellation.checkpoint()?;
    if input.format != FileFormat::Script {
        return Ok(Vec::new());
    }
    let source: &str = input.source.as_ref();
    let mut diagnostics = Vec::new();
    // LSP text carries script bytes through the CP1252 Unicode layer. The
    // localisation byte profile validates that layer without mistaking UTF-8
    // payload characters for raw single-byte triple members.
    if let Err(transcode::FileConversionError::BrokenEscape { byte_index }) =
        transcode::decode_buffer(source.as_bytes(), transcode::Profile::Localisation)
        && let Some(range) = char_range(source, byte_index)
    {
        diagnostics.push(Diagnostic::new(
            DiagnosticCode::LocalisationBrokenEscapeSequence,
            Severity::Warning,
            range,
            "damaged EU4dll escape sequence: whole-file conversion is refused until this is repaired"
                .to_owned(),
        ));
    }
    if transcode::classify_text(source) == transcode::Classification::Escaped
        && !transcode::script_roundtrip_is_canonical(source)
    {
        diagnostics.push(Diagnostic::new(
            DiagnosticCode::ScriptLegacyEscapeVariant,
            Severity::Hint,
            TextRange::empty(TextSize::from(u32::try_from(source.len()).unwrap_or(u32::MAX))),
            "this file was written with a historical EU4dll escape-set variant: it decodes correctly, \
             but saving the decoded view normalizes the triples to the canonical set".to_owned(),
        ));
    }
    if diagnostics.len() < MAX_TRANSCODE_DIAGNOSTICS {
        diagnostics.extend(unencodable_diagnostics(source, cancellation)?);
    }
    Ok(diagnostics)
}

/// Whole-file warnings for code points that the script encoder refuses.
/// Existing triples are byte carriers, so their payload is not readable input.
fn unencodable_diagnostics(
    source: &str,
    cancellation: &CancellationToken,
) -> Result<Vec<Diagnostic>, Cancelled> {
    let mut diagnostics = Vec::new();
    let mut characters = source.char_indices();
    while let Some((offset, character)) = characters.next() {
        cancellation.checkpoint()?;
        if (0x10..=0x13).contains(&u32::from(character)) {
            characters.next();
            characters.next();
            continue;
        }
        if let Some(kind) =
            transcode::file_unencodable_kind(u32::from(character), transcode::Profile::Script)
        {
            let reason = match kind {
                transcode::UnencodableKind::MangledLowPlane => {
                    "U+0100..U+0FFF characters are silently mangled by the EU4 transcoder"
                }
                transcode::UnencodableKind::BeyondBmp => {
                    "characters beyond the BMP are destroyed by the EU4 transcoder"
                }
            };
            diagnostics.push(Diagnostic::new(
                DiagnosticCode::LocalisationUnencodableCodePoint,
                Severity::Warning,
                char_range(source, offset).unwrap_or_else(|| TextRange::empty(0)),
                format!(
                    "{character} (U+{:04X}) cannot be round-tripped: {reason}",
                    u32::from(character)
                ),
            ));
            if diagnostics.len() >= MAX_TRANSCODE_DIAGNOSTICS {
                break;
            }
        }
    }
    Ok(diagnostics)
}

fn char_range(source: &str, offset: usize) -> Option<TextRange> {
    let start = u32::try_from(offset).ok()?;
    let character = source[offset..].chars().next()?;
    let end = start + (character.len_utf8() as u32);
    TextRange::new(start, end)
}
