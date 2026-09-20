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
        SourceRootKind::Project,
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
        SourceRootKind::Project,
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

/// A scan of a live workspace already carries localisation previews in its
/// serving map, so hover and mission-card lookups hit the map instead of
/// reparsing whole files per query. The rendering matches the CST fallback
/// (decoded readable value).
#[test]
fn scanned_localisation_previews_serve_without_reparsing() {
    let root = temp_root("serve");
    fs::create_dir_all(root.join("localisation")).expect("fixture dir");
    let fixture = root.join("localisation/mod_l_english.yml");
    fs::write(
        &fixture,
        escaped_yml("\u{feff}l_english:\r\n MOD_TITLE:0 \"\u{4EFB}\u{52A1}\"\r\n"),
    )
    .expect("fixture");

    let mut host = eu4_host();
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(1),
        SourceRootKind::Project,
        AbsPath::normalize(&root),
    )]));
    host.refresh_source_roots().expect("scan roots");
    let snapshot = host.snapshot();
    let definition = snapshot
        .index()
        .definitions("localisation", "mod_title")
        .into_iter()
        .next()
        .expect("definition");
    let preview = snapshot
        .localisation_previews()
        .get((definition.file_id, definition.range))
        .expect("serving preview for scanned file");
    assert_eq!(preview.language.as_deref(), Some("l_english"));
    assert_eq!(preview.value, "\u{4EFB}\u{52A1}"); // 任务
}

/// Opening an overlay for a scanned localisation file must stop the map from
/// serving scan-era text; closing restores the scanned entries.
#[test]
fn overlay_open_suppresses_and_close_restores_scanned_previews() {
    let root = temp_root("overlay");
    fs::create_dir_all(root.join("localisation")).expect("fixture dir");
    let fixture = root.join("localisation/mod_l_english.yml");
    fs::write(
        &fixture,
        "\u{feff}l_english:\r\n MOD_TITLE:0 \"original\"\r\n",
    )
    .expect("fixture");

    let mut host = eu4_host();
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(1),
        SourceRootKind::Project,
        AbsPath::normalize(&root),
    )]));
    host.refresh_source_roots().expect("scan roots");
    let snapshot = host.snapshot();
    let definition = snapshot
        .index()
        .definitions("localisation", "mod_title")
        .into_iter()
        .next()
        .expect("definition");
    let key = (definition.file_id, definition.range);

    let document = DocumentId::new(format!("file:///{}", fixture.to_string_lossy()));
    host.open_document(
        document.clone(),
        1,
        "\u{feff}l_english:\r\n MOD_TITLE:0 \"edited\"\r\n".to_owned(),
        Some(AbsPath::normalize(&fixture)),
    )
    .expect("open overlay");
    let snapshot = host.snapshot();
    assert!(
        snapshot.localisation_previews().get(key).is_none(),
        "overlay text must not be shadowed by scan-era previews"
    );

    host.close_document(&document).expect("close overlay");
    let snapshot = host.snapshot();
    let restored = snapshot
        .localisation_previews()
        .get(key)
        .expect("scanned preview restored after close");
    assert_eq!(restored.value, "original");
}

/// A disk change to a scanned localisation file replaces its serving entries.
#[test]
fn disk_change_replaces_scanned_previews() {
    let root = temp_root("disk");
    fs::create_dir_all(root.join("localisation")).expect("fixture dir");
    let fixture = root.join("localisation/mod_l_english.yml");
    fs::write(
        &fixture,
        "\u{feff}l_english:\r\n MOD_TITLE:0 \"before\"\r\n",
    )
    .expect("fixture");

    let mut host = eu4_host();
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(1),
        SourceRootKind::Project,
        AbsPath::normalize(&root),
    )]));
    host.refresh_source_roots().expect("scan roots");
    fs::write(&fixture, "\u{feff}l_english:\r\n MOD_TITLE:0 \"after\"\r\n")
        .expect("rewrite fixture");
    host.apply_disk_file_changes(&[DiskFileChange::new(
        AbsPath::normalize(&fixture),
        DiskFileChangeKind::Changed,
    )])
    .expect("apply disk change");
    let snapshot = host.snapshot();
    let definition = snapshot
        .index()
        .definitions("localisation", "mod_title")
        .into_iter()
        .next()
        .expect("definition");
    let preview = snapshot
        .localisation_previews()
        .get((definition.file_id, definition.range))
        .expect("replaced preview");
    assert_eq!(preview.value, "after");
}

/// Scanned previews follow the same preferred-language retention as cache
/// installation: with no explicit preference only English files serve from
/// the map, other languages keep their (unchanged) fallback resolution.
#[test]
fn scanned_previews_retain_preferred_languages_only() {
    let root = temp_root("retain");
    fs::create_dir_all(root.join("localisation")).expect("fixture dir");
    fs::write(
        root.join("localisation/mod_l_english.yml"),
        "\u{feff}l_english:\r\n MOD_TITLE:0 \"english\"\r\n",
    )
    .expect("english fixture");
    fs::write(
        root.join("localisation/mod_l_french.yml"),
        "\u{feff}l_french:\r\n MOD_TITLE:0 \"french\"\r\n",
    )
    .expect("french fixture");

    let mut host = eu4_host();
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(1),
        SourceRootKind::Project,
        AbsPath::normalize(&root),
    )]));
    host.refresh_source_roots().expect("scan roots");
    let snapshot = host.snapshot();
    for (name, expect_preview) in [("mod_title", true)] {
        for definition in snapshot.index().definitions("localisation", name) {
            let file = snapshot
                .source_files()
                .get(&definition.file_id)
                .expect("source file");
            let is_english = file.logical_path.as_str().contains("l_english");
            let served = snapshot
                .localisation_previews()
                .get((definition.file_id, definition.range))
                .is_some();
            assert_eq!(
                served,
                is_english || !expect_preview,
                "file {} served={} english={}",
                file.logical_path.as_str(),
                served,
                is_english
            );
        }
    }
}
