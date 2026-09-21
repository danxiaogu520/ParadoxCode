//! Transcode diagnostics for the EU4dll double-byte patch ecosystem.
//!
//! The server sees documents as text (disk bytes were already read through the
//! UTF-8/CP1252 layer), so these checks classify the document text and report:
//!
//! * escape markers outside every quoted string — code or comment position —
//!   (`LocalisationMixedEncoding`); inside strings a marker is either part of
//!   a scoped escape (readable CJK sharing the span self-heals on save
//!   through the decoded view) or a repairable orphan;
//! * orphan escape markers inside strings that decoding passed through
//!   (`LocalisationBrokenEscapeSequence`);
//! * code points inside strings that the transcoder refuses
//!   (`LocalisationUnencodableCodePoint`), while the script profile accepts
//!   the 27 CP1252 letters — outside strings scoped saving keeps bytes
//!   verbatim, so no refusal applies there;
//! * script files that decode correctly but re-encode differently
//!   (`ScriptLegacyEscapeVariant`, Hint).
//!
//! The extension's save gate refuses double-encoding client-side
//! (`LocalisationEscapeRefused`); that code is registered here for
//! filtering/severity overrides but never emitted by the server.

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
    match transcode::classify_text(source) {
        transcode::Classification::Mixed => {
            let bytes = source.as_bytes();
            match transcode::marker_outside_spans(bytes).first() {
                Some(&offset) => {
                    if let Some(range) = char_range(source, offset) {
                        diagnostics.push(Diagnostic::new(
                            DiagnosticCode::LocalisationMixedEncoding,
                            Severity::Error,
                            range,
                            "escape marker outside every quoted string: damaged transcoded \
                             content — no transformation is applied, fix the file manually"
                                .to_owned(),
                        ));
                    }
                }
                None => {
                    // Scoped shape: orphan markers survive only inside strings.
                    // Span boundaries fall on ASCII structural bytes, so text
                    // slices at them are char-safe.
                    for (start, end) in transcode::scan_string_spans(bytes) {
                        cancellation.checkpoint()?;
                        let Some(span) = source.get(start..end) else {
                            continue;
                        };
                        for offset in transcode::decode_text(span).broken_sequences {
                            if diagnostics.len() >= MAX_TRANSCODE_DIAGNOSTICS {
                                break;
                            }
                            if let Some(range) = char_range(source, start + offset) {
                                diagnostics.push(Diagnostic::new(
                                    DiagnosticCode::LocalisationBrokenEscapeSequence,
                                    Severity::Warning,
                                    range,
                                    "orphan EU4dll escape marker: the surrounding triple is \
                                     damaged and was passed through undecoded"
                                        .to_owned(),
                                ));
                            }
                        }
                    }
                }
            }
        }
        transcode::Classification::Escaped => {
            if input.format == FileFormat::Script
                && !transcode::script_roundtrip_is_canonical(source)
            {
                diagnostics.push(Diagnostic::new(
                    DiagnosticCode::ScriptLegacyEscapeVariant,
                    Severity::Hint,
                    TextRange::empty(TextSize::from(
                        u32::try_from(source.len()).unwrap_or(u32::MAX),
                    )),
                    "this file was written with a historical EU4dll escape-set variant: it \
                     decodes correctly, but saving normalizes the triples to the canonical set"
                        .to_owned(),
                ));
            }
        }
        transcode::Classification::Readable => {}
        transcode::Classification::Ascii => {}
    }
    if diagnostics.len() < MAX_TRANSCODE_DIAGNOSTICS {
        diagnostics.extend(unencodable_diagnostics(source, cancellation)?);
    }
    Ok(diagnostics)
}

/// Per-character warnings for code points inside strings that the script
/// transcoder refuses. The 27 CP1252-mapped letters remain valid single bytes.
fn unencodable_diagnostics(
    source: &str,
    cancellation: &CancellationToken,
) -> Result<Vec<Diagnostic>, Cancelled> {
    let mut diagnostics = Vec::new();
    for (start, end) in transcode::scan_string_spans(source.as_bytes()) {
        cancellation.checkpoint()?;
        let Some(span) = source.get(start..end) else {
            continue;
        };
        for (offset, character) in span.char_indices() {
            if diagnostics.len() >= MAX_TRANSCODE_DIAGNOSTICS {
                return Ok(diagnostics);
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
                    char_range(source, start + offset).unwrap_or_else(|| TextRange::empty(0)),
                    format!(
                        "{character} (U+{:04X}) cannot be round-tripped: {reason}",
                        u32::from(character)
                    ),
                ));
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
