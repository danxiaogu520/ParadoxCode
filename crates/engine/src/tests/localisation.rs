use super::*;
use text::AbsPath;

fn escaped_yml(readable: &str) -> String {
    transcode::encode_text(readable, transcode::EscapeSet::Paratranz)
        .expect("fixture must be encodable")
}

fn temp_root(label: &str) -> std::path::PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("engine-localisation-{label}-{nonce}"));
    fs::create_dir_all(&root).expect("fixture root");
    root
}

/// Previews of EU4dll-transcoded localisation files store the decoded readable
/// value (the text the game renders), not the raw escape triples. Readable
/// files keep their values unchanged.
#[test]
fn transcoded_localisation_previews_decode_to_readable_values() {
    let root = temp_root("decode");
    fs::create_dir_all(root.join("localisation/l_english")).expect("master dir");
    fs::create_dir_all(root.join("localisation/replace")).expect("release dir");
    fs::write(
        root.join("localisation/l_english/edg_l_english.yml"),
        "\u{feff}l_english:\r\n EDG_KEY:0 \"\u{6BCD}\u{672C}\"\r\n",
    )
    .expect("master fixture");
    fs::write(
        root.join("localisation/replace/edg_l_english.yml"),
        escaped_yml("\u{feff}l_english:\r\n EDG_KEY:0 \"\u{53D1}\u{884C}\u{672C}\"\r\n"),
    )
    .expect("release fixture");

    let mut host = eu4_host();
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(1),
        SourceRootKind::CurrentMod,
        AbsPath::normalize(&root),
    )]));
    host.refresh_source_roots().expect("scan roots");
    let snapshot = host.snapshot();

    let definitions = snapshot.index().definitions("localisation", "edg_key");
    assert_eq!(definitions.len(), 2, "master and release both indexed");
    for definition in definitions {
        let file = snapshot
            .source_files()
            .get(&definition.file_id)
            .expect("source file");
        let is_release = file
            .logical_path
            .as_str()
            .starts_with("localisation/replace/");
        // The same derivation that persists previews into the vanilla index cache.
        let state = snapshot
            .file_states
            .get(&definition.file_id)
            .expect("state");
        let cache_only = state.cache_only_from_existing();
        let previews = cache_only
            .cached_localisation_previews()
            .expect("derived previews");
        let preview = previews
            .iter()
            .find(|(range, _)| *range == definition.range)
            .map(|(_, preview)| preview)
            .expect("preview for entry");
        assert_eq!(preview.language.as_deref(), Some("l_english"));
        let expected = if is_release {
            "\u{53D1}\u{884C}\u{672C}" // 发行本
        } else {
            "\u{6BCD}\u{672C}" // 母本
        };
        assert_eq!(preview.value, expected, "file {:?}", file.physical_path);
    }
}

/// Short values below the classifier's triple threshold still decode: the
/// value-level gate keys on marker presence alone.
#[test]
fn single_triple_values_still_decode() {
    let root = temp_root("single");
    fs::create_dir_all(root.join("localisation/replace")).expect("release dir");
    fs::write(
        root.join("localisation/replace/short.yml"),
        escaped_yml("l_english:\r\n SHORT_KEY:0 \"\u{4E2D}\"\r\n"),
    )
    .expect("release fixture");

    let mut host = eu4_host();
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(1),
        SourceRootKind::CurrentMod,
        AbsPath::normalize(&root),
    )]));
    host.refresh_source_roots().expect("scan roots");
    let snapshot = host.snapshot();

    let definitions = snapshot.index().definitions("localisation", "short_key");
    assert_eq!(definitions.len(), 1);
    let state = snapshot
        .file_states
        .get(&definitions[0].file_id)
        .expect("state");
    let cache_only = state.cache_only_from_existing();
    let previews = cache_only
        .cached_localisation_previews()
        .expect("derived previews");
    assert!(
        previews
            .iter()
            .any(|(_, preview)| preview.value == "\u{4E2D}"), // 中
        "single-triple value must still decode: {previews:?}"
    );
}
