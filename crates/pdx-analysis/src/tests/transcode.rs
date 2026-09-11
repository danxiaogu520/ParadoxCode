//! Server-side transcode diagnostics (EU4dll escape-triple ecosystem).

use super::support::*;
use crate::{DiagnosticCode, Severity};

fn escaped_yml(value: &str) -> String {
    pdx_codec::encode_text(
        &format!("l_english:\r\n edg_key:0 \"{value}\"\r\n"),
        pdx_codec::EscapeSet::Paratranz,
    )
    .expect("encode fixture")
}

/// A host with a Current Mod root, so overlay documents get the same
/// `localisation/…` logical paths the scanner derives for disk files.
fn rooted_host(tag: &str) -> (AnalysisHost, std::path::PathBuf) {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-analysis-transcode-{tag}-{nonce}"));
    std::fs::create_dir_all(root.join("localisation/replace")).expect("create release dir");
    std::fs::create_dir_all(root.join("localisation/l_english")).expect("create master dir");
    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(1),
        SourceRootKind::CurrentMod,
        root.clone(),
    )]));
    host.refresh_source_roots().expect("scan source root");
    (host, root)
}

#[test]
fn readable_release_file_is_flagged_not_transcoded() {
    let (mut host, root) = rooted_host("release");
    // The replace/ segment is what makes this a game-read release path.
    let id = DocumentId::new("file:///tmp/transcode/release.yml");
    host.open_document(
        id.clone(),
        1,
        "l_english:\n edg_key:0 \"\u{6F22}\u{5B57}\"\n".to_owned(),
        Some(root.join("localisation/replace/edg.yml")),
    )
    .expect("open release file");
    let diagnostics = crate::diagnostics(&host.snapshot(), &id);
    let flagged = diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == DiagnosticCode::LocalisationNotTranscoded)
        .expect("release file must be flagged");
    assert_eq!(flagged.severity, Severity::Warning);
    assert!(flagged.message.contains("mojibake"));
}

#[test]
fn readable_master_tree_stays_quiet() {
    let (mut host, root) = rooted_host("master");
    let id = DocumentId::new("file:///tmp/transcode/master.yml");
    host.open_document(
        id.clone(),
        1,
        "l_english:\n edg_key:0 \"\u{6F22}\u{5B57}\"\n".to_owned(),
        Some(root.join("localisation/l_english/edg.yml")),
    )
    .expect("open master file");
    let diagnostics = crate::diagnostics(&host.snapshot(), &id);
    assert!(
        !diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == DiagnosticCode::LocalisationNotTranscoded),
        "master-tree files are readable by convention"
    );
}

#[test]
fn mixed_encoding_is_an_error_at_first_evidence() {
    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    let text = "l_english:\n edg_key:0 \"\u{6F22}\u{0010}\u{0010}\"\n".to_owned();
    let id = DocumentId::new("file:///tmp/transcode/mixed.yml");
    host.open_document(
        id.clone(),
        1,
        text.clone(),
        Some(std::env::temp_dir().join("transcode/localisation/replace/mixed.yml")),
    )
    .expect("open mixed file");
    let diagnostics = crate::diagnostics(&host.snapshot(), &id);
    let mixed = diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == DiagnosticCode::LocalisationMixedEncoding)
        .expect("mixed file must be an error");
    assert_eq!(mixed.severity, Severity::Error);
    assert_eq!(
        mixed.range.start(),
        u32::try_from(text.find('\u{6F22}').expect("first evidence")).expect("offset")
    );
}

#[test]
fn escaped_files_report_orphan_markers_and_stay_otherwise_quiet() {
    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    // Clean escaped text plus one trailing orphan marker after the value.
    let escaped = escaped_yml("\u{5927}\u{660E}\u{738B}\u{671D}");
    let text = format!("{escaped}\u{0010}");
    let id = DocumentId::new("file:///tmp/transcode/escaped.yml");
    host.open_document(
        id.clone(),
        1,
        text.clone(),
        Some(std::env::temp_dir().join("transcode/localisation/replace/edg.yml")),
    )
    .expect("open escaped file");
    let diagnostics = crate::diagnostics(&host.snapshot(), &id);
    let broken = diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == DiagnosticCode::LocalisationBrokenEscapeSequence)
        .expect("orphan marker must be reported");
    assert_eq!(broken.severity, Severity::Warning);
    assert_eq!(
        broken.range.start(),
        u32::try_from(text.len() - 1).expect("orphan offset")
    );
    assert!(
        !diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == DiagnosticCode::LocalisationNotTranscoded),
        "an escaped release file is healthy"
    );
}

#[test]
fn unencodable_code_points_follow_the_profile_rules() {
    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    // Localisation profile: U+0160 (Š) is refused.
    let yml = DocumentId::new("file:///tmp/transcode/boundary.yml");
    host.open_document(
        yml.clone(),
        1,
        "l_english:\n edg_key:0 \"\u{0160}trasse\"\n".to_owned(),
        Some(std::env::temp_dir().join("transcode/localisation/l_english/edg.yml")),
    )
    .expect("open yml");
    let diagnostics = crate::diagnostics(&host.snapshot(), &yml);
    let refused = diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == DiagnosticCode::LocalisationUnencodableCodePoint)
        .expect("yml profile must refuse U+0160");
    assert!(refused.message.contains("U+0160"));

    // Script profile: the CP1252-mapped Š is a single byte and allowed…
    let script = DocumentId::new("file:///tmp/transcode/history.txt");
    host.open_document(
        script.clone(),
        1,
        "dynasty = \"\u{0160}trasse\"\n".to_owned(),
        Some(std::env::temp_dir().join("transcode/history/countries/CHI.txt")),
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
        &[pdx_engine::TextChange::full(
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
        let bytes = pdx_codec::encode_file(
            "\u{5C3A}\u{5C3A}\u{5C3A}",
            pdx_codec::Profile::Script,
            escape_set,
        )
        .expect("encode");
        bytes
            .iter()
            .map(|byte| {
                let code_point = pdx_codec::CP1252_MAP
                    .iter()
                    .find(|(mapped, _)| mapped == byte)
                    .map_or(u32::from(*byte), |(_, code_point)| *code_point);
                char::from_u32(code_point).expect("scalar")
            })
            .collect()
    };
    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));

    let canonical = format!(
        "name = \"{}\"\n",
        script_chars(pdx_codec::EscapeSet::Paratranz)
    );
    let canonical_id = DocumentId::new("file:///tmp/transcode/canonical.txt");
    host.open_document(
        canonical_id.clone(),
        1,
        canonical,
        Some(std::env::temp_dir().join("transcode/history/countries/AAA.txt")),
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
        script_chars(pdx_codec::EscapeSet::DllFull)
    );
    let legacy_id = DocumentId::new("file:///tmp/transcode/legacy.txt");
    host.open_document(
        legacy_id.clone(),
        1,
        legacy,
        Some(std::env::temp_dir().join("transcode/history/countries/BBB.txt")),
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
