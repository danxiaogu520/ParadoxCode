//! Server-side transcode diagnostics (EU4dll escape-triple ecosystem).

use super::support::*;
use crate::{DiagnosticCode, Severity};
use text::AbsPath;

#[test]
fn script_unencodable_code_points_follow_profile_rules() {
    let mut host = eu4_host(game::eu4::first_party_rules().expect("first-party rules"));
    // In the script profile, the CP1252-mapped Š is a single byte and allowed…
    let script = DocumentId::new("file:///tmp/transcode/history.txt");
    host.open_document(
        script.clone(),
        1,
        "dynasty = \"\u{0160}trasse\"\n".to_owned(),
        Some(AbsPath::normalize(
            &std::env::temp_dir().join("transcode/history/countries/CHI.txt"),
        )),
    )
    .expect("open script");
    let diagnostics = crate::diagnostics(&host.snapshot(), &script);
    assert!(
        !diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == DiagnosticCode::LocalisationUnencodableCodePoint),
        "script profile keeps CP1252 letters as single bytes"
    );
    // …while beyond-BMP characters are refused in both profiles.
    host.apply_document_changes(
        &script,
        2,
        &[engine::TextChange::full(
            "dynasty = \"\u{1F600}\"\n".to_owned(),
        )],
    )
    .expect("change script");
    let diagnostics = crate::diagnostics(&host.snapshot(), &script);
    assert!(
        diagnostics.iter().any(|diagnostic| {
            diagnostic.code == DiagnosticCode::LocalisationUnencodableCodePoint
                && diagnostic.message.contains("U+1F600")
        }),
        "beyond-BMP characters are refused in the script profile"
    );
}

#[test]
fn legacy_escape_variant_script_files_get_a_hint() {
    let script_chars = |escape_set| -> String {
        let bytes = transcode::encode_file(
            "\u{5C3A}\u{5C3A}\u{5C3A}",
            transcode::Profile::Script,
            escape_set,
        )
        .expect("encode");
        bytes
            .iter()
            .map(|byte| {
                let code_point = transcode::CP1252_MAP
                    .iter()
                    .find(|(mapped, _)| mapped == byte)
                    .map_or(u32::from(*byte), |(_, code_point)| *code_point);
                char::from_u32(code_point).expect("scalar")
            })
            .collect()
    };
    let mut host = eu4_host(game::eu4::first_party_rules().expect("first-party rules"));

    let canonical = format!(
        "name = \"{}\"\n",
        script_chars(transcode::EscapeSet::Paratranz)
    );
    let canonical_id = DocumentId::new("file:///tmp/transcode/canonical.txt");
    host.open_document(
        canonical_id.clone(),
        1,
        canonical,
        Some(AbsPath::normalize(
            &std::env::temp_dir().join("transcode/history/countries/AAA.txt"),
        )),
    )
    .expect("open canonical script");
    let diagnostics = crate::diagnostics(&host.snapshot(), &canonical_id);
    assert!(
        !diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == DiagnosticCode::ScriptLegacyEscapeVariant),
        "a canonical file must not be flagged"
    );

    let legacy = format!(
        "name = \"{}\"\n",
        script_chars(transcode::EscapeSet::DllFull)
    );
    let legacy_id = DocumentId::new("file:///tmp/transcode/legacy.txt");
    host.open_document(
        legacy_id.clone(),
        1,
        legacy,
        Some(AbsPath::normalize(
            &std::env::temp_dir().join("transcode/history/countries/BBB.txt"),
        )),
    )
    .expect("open legacy script");
    let diagnostics = crate::diagnostics(&host.snapshot(), &legacy_id);
    let hint = diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == DiagnosticCode::ScriptLegacyEscapeVariant)
        .expect("legacy variant must produce a hint");
    assert_eq!(hint.severity, Severity::Hint);
    assert!(hint.message.contains("normalizes"));
}

fn transcode_codes(host: &AnalysisHost, id: &DocumentId) -> Vec<DiagnosticCode> {
    diagnostics(&host.snapshot(), id)
        .into_iter()
        .map(|diagnostic| diagnostic.code)
        .collect()
}

fn open_script(host: &mut AnalysisHost, name: &str, text: String) -> DocumentId {
    let id = DocumentId::new(format!("file:///tmp/transcode/{name}"));
    host.open_document(
        id.clone(),
        1,
        text,
        Some(AbsPath::normalize(
            &std::env::temp_dir().join("transcode/history/countries/TST.txt"),
        )),
    )
    .expect("open script");
    id
}

#[test]
fn scoped_shaped_text_is_not_mixed_encoding() {
    // A scoped file's char layer: one escaped string, one readable string, a
    // readable comment. Escape markers sit only inside the quoted span, so no
    // mixed-encoding error, no orphan warnings, nothing to fix.
    let escaped = transcode::encode_text(
        "\u{5927}\u{660E}\u{738B}\u{671D}",
        transcode::EscapeSet::Paratranz,
    )
    .expect("encode");
    let text =
        format!("# \u{6CE8}\u{91CA}\r\nname = \"{escaped}\"\r\ntitle = \"\u{5E1D}\u{56FD}\"\r\n");
    let mut host = eu4_host(game::eu4::first_party_rules().expect("first-party rules"));
    let id = open_script(&mut host, "scoped.txt", text);
    let codes = transcode_codes(&host, &id);
    assert!(
        !codes.contains(&DiagnosticCode::LocalisationMixedEncoding)
            && !codes.contains(&DiagnosticCode::LocalisationBrokenEscapeSequence)
            && !codes.contains(&DiagnosticCode::LocalisationUnencodableCodePoint),
        "scoped-shaped text must stay diagnosis-free, got {codes:?}"
    );
}

#[test]
fn marker_outside_strings_is_a_mixed_error() {
    // A marker in code position (outside quotes and comments) anchors the
    // mixed-encoding error at itself.
    let mut host = eu4_host(game::eu4::first_party_rules().expect("first-party rules"));
    let id = open_script(
        &mut host,
        "damaged.txt",
        "name = \"x\"\r\na \u{0010}AB\r\n".to_owned(),
    );
    let diagnostics = diagnostics(&host.snapshot(), &id);
    let error = diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == DiagnosticCode::LocalisationMixedEncoding)
        .expect("out-of-span marker must be a mixed-encoding error");
    assert_eq!(error.severity, Severity::Error);
    assert!(error.message.contains("outside every quoted string"));
    // Anchored at the marker byte (14 bytes precede it in `name = "x"\r\na `).
    assert_eq!(error.range.start(), 14);
}

#[test]
fn in_span_orphan_markers_report_as_repairable_warnings() {
    // A marker with no payload inside a string is an orphan, not damage: the
    // scoped path passes it through and reports it for manual repair.
    let mut host = eu4_host(game::eu4::first_party_rules().expect("first-party rules"));
    let id = open_script(
        &mut host,
        "orphan.txt",
        "name = \"\u{0010}\"\r\n".to_owned(),
    );
    let codes = transcode_codes(&host, &id);
    assert!(
        !codes.contains(&DiagnosticCode::LocalisationMixedEncoding),
        "an in-span orphan is not mixed encoding"
    );
    assert!(codes.contains(&DiagnosticCode::LocalisationBrokenEscapeSequence));
}

#[test]
fn unencodable_code_points_outside_strings_are_not_flagged() {
    // Scoped saving keeps everything outside strings verbatim, so a refusal
    // only applies inside a quoted span.
    let mut host = eu4_host(game::eu4::first_party_rules().expect("first-party rules"));
    let id = open_script(
        &mut host,
        "comment-emoji.txt",
        "# note \u{1F600}\r\nname = \"x\"\r\n".to_owned(),
    );
    let codes = transcode_codes(&host, &id);
    assert!(
        !codes.contains(&DiagnosticCode::LocalisationUnencodableCodePoint),
        "comment-position characters are never escaped, got {codes:?}"
    );
}
