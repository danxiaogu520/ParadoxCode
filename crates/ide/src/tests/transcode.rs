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
