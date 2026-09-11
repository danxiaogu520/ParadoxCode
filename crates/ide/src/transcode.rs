//! Transcode diagnostics for the EU4dll double-byte patch ecosystem.
//!
//! The server sees documents as text (disk bytes were already read through the
//! UTF-8/CP1252 layer), so these checks classify the document text and report:
//!
//! * readable CJK on a `replace/` release path (`LocalisationNotTranscoded`) —
//!   the master-tree convention keeps readable sources under other
//!   `localisation/` directories on purpose, so only release paths are flagged;
//! * files mixing readable CJK with escape triples or carrying stray markers
//!   (`LocalisationMixedEncoding`);
//! * orphan escape markers that decoding passed through
//!   (`LocalisationBrokenEscapeSequence`);
//! * code points the transcoder refuses (`LocalisationUnencodableCodePoint`),
//!   profile-aware: the script profile accepts the 27 CP1252 letters;
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
    let source: &str = input.source.as_ref();
    let counts = transcode::classify_text_counts(source);
    let mut diagnostics = Vec::new();
    // A file that is escaped overall (at least three triples, no raw CJK) but
    // carries orphan markers is a *damaged transcoded file*: report each
    // orphan precisely instead of the harsher mixed-encoding error. Everything
    // else that is not clean gets the mixed-encoding diagnosis.
    let escaped_with_damage =
        counts.escaped_triples >= 3 && counts.raw_cjk == 0 && counts.broken_markers > 0;
    match transcode::classify_text(source) {
        transcode::Classification::Escaped | transcode::Classification::Mixed
            if escaped_with_damage =>
        {
            let decoded = transcode::decode_text(source);
            for offset in decoded
                .broken_sequences
                .iter()
                .take(MAX_TRANSCODE_DIAGNOSTICS)
            {
                cancellation.checkpoint()?;
                if let Some(range) = char_range(source, *offset) {
                    diagnostics.push(Diagnostic::new(
                        DiagnosticCode::LocalisationBrokenEscapeSequence,
                        Severity::Warning,
                        range,
                        "orphan EU4dll escape marker: the surrounding triple is damaged and \
                             was passed through undecoded"
                            .to_owned(),
                    ));
                }
            }
            if input.format == FileFormat::Script
                && diagnostics.len() < MAX_TRANSCODE_DIAGNOSTICS
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
        transcode::Classification::Mixed => {
            if let Some(range) = first_mixed_evidence(source) {
                diagnostics.push(Diagnostic::new(
                    DiagnosticCode::LocalisationMixedEncoding,
                    Severity::Error,
                    range,
                    "this file mixes readable CJK with EU4dll escape triples (or stray escape \
                     markers); no transformation is applied — fix the file manually"
                        .to_owned(),
                ));
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
        transcode::Classification::Readable => {
            let release_localisation = input.format == FileFormat::Localisation
                && input
                    .path
                    .as_ref()
                    .is_some_and(|path| is_release_localisation_path(path.as_str()));
            if release_localisation && let Some(range) = first_raw_cjk(source) {
                diagnostics.push(Diagnostic::new(
                    DiagnosticCode::LocalisationNotTranscoded,
                    Severity::Warning,
                    range,
                    "this file sits on the game read path (replace/) but contains readable \
                     CJK text: the game will render mojibake. Transcode it or keep it out \
                     of the release tree"
                        .to_owned(),
                ));
            }
        }
        transcode::Classification::Ascii => {}
    }
    if diagnostics.len() < MAX_TRANSCODE_DIAGNOSTICS {
        diagnostics.extend(unencodable_diagnostics(input, source, cancellation)?);
    }
    Ok(diagnostics)
}

/// Per-character warnings for code points the transcoder refuses. Applies to
/// readable and plain documents of both profiles; the script profile keeps the
/// 27 CP1252-mapped letters.
fn unencodable_diagnostics(
    input: &ParsedInput,
    source: &str,
    cancellation: &CancellationToken,
) -> Result<Vec<Diagnostic>, Cancelled> {
    let profile = match input.format {
        FileFormat::Localisation => transcode::Profile::Localisation,
        FileFormat::Script => transcode::Profile::Script,
    };
    let mut diagnostics = Vec::new();
    for (offset, character) in source.char_indices() {
        cancellation.checkpoint()?;
        if let Some(kind) = transcode::file_unencodable_kind(u32::from(character), profile) {
            if diagnostics.len() >= MAX_TRANSCODE_DIAGNOSTICS {
                break;
            }
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
        }
    }
    Ok(diagnostics)
}

/// A `localisation/…` path containing a `replace` segment: the dual-tree
/// release layout where readable CJK means the game renders mojibake. Other
/// localisation directories are master copies by convention and stay quiet.
fn is_release_localisation_path(path: &str) -> bool {
    let mut segments = path.split('/');
    let under_localisation = segments
        .next()
        .is_some_and(|first| first.eq_ignore_ascii_case("localisation"));
    under_localisation && segments.any(|segment| segment.eq_ignore_ascii_case("replace"))
}

fn char_range(source: &str, offset: usize) -> Option<TextRange> {
    let start = u32::try_from(offset).ok()?;
    let character = source[offset..].chars().next()?;
    let end = start + (character.len_utf8() as u32);
    TextRange::new(start, end)
}

fn first_raw_cjk(source: &str) -> Option<TextRange> {
    source
        .char_indices()
        .find(|(_, character)| transcode::is_raw_cjk(u32::from(*character)))
        .and_then(|(offset, _)| char_range(source, offset))
}

/// Anchors the mixed-encoding error at the first marker or CJK character,
/// whichever comes first, so the reported position is actual evidence.
fn first_mixed_evidence(source: &str) -> Option<TextRange> {
    source
        .char_indices()
        .find(|(_, character)| {
            let code_point = u32::from(*character);
            (0x10..=0x13).contains(&code_point) || transcode::is_raw_cjk(code_point)
        })
        .and_then(|(offset, _)| char_range(source, offset))
}
