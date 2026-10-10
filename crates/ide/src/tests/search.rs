use super::support::*;
use crate::search::*;
use text::AbsPath;

fn roots_fixture(tag: &str) -> (std::path::PathBuf, AnalysisHost) {
    let root = temp_root(tag);
    let mut roots = Vec::new();
    for (order, name, kind) in [
        (0, "vanilla", SourceRootKind::Vanilla),
        (1, "dep-a", SourceRootKind::Dependency),
        (2, "dep-b", SourceRootKind::Dependency),
        (3, "mod", SourceRootKind::Project),
    ] {
        let directory = root.join(name);
        std::fs::create_dir_all(directory.join("localisation")).unwrap();
        std::fs::create_dir_all(directory.join("events")).unwrap();
        std::fs::write(
            directory.join(format!("localisation/{name}_l_english.yml")),
            format!("l_english:\nshared:0 \"{name} text\"\nunique_{order}:0 \"unique {name}\"\n"),
        )
        .unwrap();
        std::fs::write(
            directory.join(format!("events/{name}.txt")),
            format!(
                "country_event = {{ id = shared.1 }}\ncountry_event = {{ id = unique.{order} }}\n"
            ),
        )
        .unwrap();
        let mut source = SourceRoot::new(
            SourceRootId::new(order + 10),
            kind,
            AbsPath::normalize(&directory),
        );
        source.order = order;
        roots.push(source);
    }
    let mut host = eu4_host(game::eu4::bootstrap_rules());
    host.apply_change(WorkspaceChange::SetSourceRoots(roots));
    host.refresh_source_roots().unwrap();
    (root, host)
}

#[test]
fn editor_search_reuses_localisation_files_across_unrelated_edits_and_updates_one_buffer() {
    let (root, mut host) = roots_fixture("editor-search-incremental");
    let token = CancellationToken::new();
    let first = editor_localisations_with_epoch(&host.snapshot(), 10, &token).unwrap();
    let script = AbsPath::normalize(&root.join("mod/events/mod.txt"));
    host.open_document(
        DocumentId::new(format!("file://{}", script.display())),
        1,
        "country_event = { id = new.1 }\n".into(),
        Some(script),
    )
    .unwrap();
    let reused = editor_localisations_with_epoch(&host.snapshot(), 10, &token).unwrap();
    assert!(
        std::sync::Arc::ptr_eq(&first, &reused),
        "a script edit cannot rebuild localisation"
    );
    let dependency = first
        .entries
        .iter()
        .find(|entry| entry.value() == "dep-a text")
        .unwrap();
    let path = AbsPath::normalize(&root.join("mod/localisation/mod_l_english.yml"));
    let document = DocumentId::new(format!("file://{}", path.display()));
    host.open_document(
        document.clone(),
        3,
        "l_english:\nshared:0 \"new Mod text\"\n".into(),
        Some(path),
    )
    .unwrap();
    let changed = editor_localisations_with_epoch(&host.snapshot(), 10, &token).unwrap();
    assert_ne!(first.generation, changed.generation);
    let unchanged = changed
        .entries
        .iter()
        .find(|entry| entry.value() == "dep-a text")
        .unwrap();
    assert!(std::sync::Arc::ptr_eq(
        dependency.source(),
        unchanged.source()
    ));
    let current = changed
        .entries
        .iter()
        .find(|entry| entry.value() == "new Mod text")
        .unwrap();
    assert!(current.active);
    assert_eq!(current.document(), Some(&document));
    assert_eq!(current.version(), Some(3));
    assert!(
        !changed
            .entries
            .iter()
            .any(|entry| entry.key() == "unique_3")
    );
    assert!(
        first.entries.iter().any(|entry| entry.key() == "unique_3"),
        "old snapshots retain their values"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn editor_search_cached_sources_refresh_same_size_writes_offline_and_restore() {
    let (root, _) = roots_fixture("editor-search-cached-refresh");
    let mut builder = eu4_host(game::eu4::bootstrap_rules());
    builder.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(13),
        SourceRootKind::Vanilla,
        AbsPath::normalize(&root.join("mod")),
    )]));
    builder.refresh_source_roots().unwrap();
    let cache = root.join("owned.pdcindex");
    IndexCache::from_snapshot(&builder.snapshot())
        .unwrap()
        .save(&cache)
        .unwrap();
    let mut host = eu4_host(game::eu4::bootstrap_rules());
    host.install_index_cache(IndexCache::load(&cache).unwrap())
        .unwrap();
    let token = CancellationToken::new();
    let path = root.join("mod/localisation/mod_l_english.yml");
    let first = editor_localisations_with_epoch(&host.snapshot(), 1, &token).unwrap();
    let source = std::fs::read_to_string(&path).unwrap();
    let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
    std::fs::write(&path, source.replace("mod text", "new text")).unwrap();
    std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(modified))
        .unwrap();
    let changed = editor_localisations_with_epoch(&host.snapshot(), 2, &token).unwrap();
    assert!(
        changed
            .entries
            .iter()
            .any(|entry| entry.value() == "new text")
    );
    assert!(
        !changed
            .entries
            .iter()
            .any(|entry| entry.value() == "mod text")
    );
    assert_ne!(first.generation, changed.generation);
    std::fs::remove_file(&path).unwrap();
    let offline = editor_localisations_with_epoch(&host.snapshot(), 3, &token).unwrap();
    assert!(!offline.limitations.is_empty());
    assert!(
        !offline
            .entries
            .iter()
            .any(|entry| entry.value() == "new text")
    );
    std::fs::write(&path, source).unwrap();
    let restored = editor_localisations_with_epoch(&host.snapshot(), 4, &token).unwrap();
    assert!(
        restored
            .entries
            .iter()
            .any(|entry| entry.value() == "mod text")
    );
    assert!(restored.limitations.is_empty());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn editor_search_prepared_matching_preserves_unicode_lowercase_and_all_words() {
    for (value, query, expected) in [
        ("Empire reform", "REFORM empire", true),
        ("Empire reform", "empire missing", false),
        ("İstanbul", "i\u{0307}STANBUL", true),
        ("Kelvin 帝国改革", "kelvin 改革", true),
        ("Durch Reformen wächst das Reich", "REICH wächst", true),
        ("no text", "   ", false),
        ("Empire", "帝国", false),
    ] {
        assert_eq!(LocalisationQuery::new(query).matches(value), expected);
    }
}

#[test]
fn editor_search_parallel_and_serial_corpora_preserve_order_and_locations() {
    let (root, mut host) = roots_fixture("editor-search-reader-parity");
    let token = CancellationToken::new();
    let parallel = editor_localisations(&host.snapshot(), &token).unwrap();
    let mut limits = host.snapshot().scan_limits();
    limits.max_workers = 1;
    host.set_scan_limits(limits);
    let serial = editor_localisations(&host.snapshot(), &token).unwrap();
    let records = |data: &EditorLocalisations| {
        data.entries
            .iter()
            .map(|entry| {
                (
                    entry.key().to_owned(),
                    entry.value().to_owned(),
                    entry.language().to_owned(),
                    entry.active,
                    entry.location(),
                    entry.root(),
                    entry.version(),
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(records(&parallel), records(&serial));
    assert_eq!(parallel.limitations, serial.limitations);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn editor_search_localisation_retains_all_sources_versions_and_header_languages() {
    let (root, mut host) = roots_fixture("editor-search-languages");
    let path = root.join("mod/localisation/mismatched_l_english.yml");
    let text = format!(
        "l_french:\nlong_value:0 \"{} §REmpire§! réforme [Root.GetName] tail\"\nl_simp_chinese:\nlong_value:0 \"通过改革，让帝国重新繁荣\"\n",
        "a".repeat(1500)
    );
    std::fs::write(&path, text).unwrap();
    host.refresh_source_roots().unwrap();
    let data = editor_localisations(&host.snapshot(), &CancellationToken::new()).unwrap();
    let shared = data
        .entries
        .iter()
        .filter(|entry| entry.key() == "shared")
        .collect::<Vec<_>>();
    assert_eq!(shared.len(), 4);
    assert_eq!(shared.iter().filter(|entry| entry.active).count(), 1);
    assert_eq!(
        shared.iter().find(|entry| entry.active).unwrap().value(),
        "mod text"
    );
    assert!(
        shared
            .iter()
            .any(|entry| !entry.active && localisation_matches(&entry.value(), "vanilla"))
    );
    let french = data
        .entries
        .iter()
        .find(|entry| entry.key() == "long_value" && entry.language() == "french")
        .unwrap();
    assert!(localisation_matches(&french.value(), "tail Empire"));
    assert!(!french.value().contains('§'));
    assert!(french.value().contains("[Root.GetName]"));
    let position = editor_localisation_match_location(french, "Empire").unwrap();
    assert_eq!(
        &french.source()[position.range.start() as usize..position.range.end() as usize],
        "Empire"
    );
    let chinese = data
        .entries
        .iter()
        .find(|entry| entry.language() == "simp_chinese")
        .unwrap();
    assert!(localisation_matches(&chinese.value(), "帝国 改革"));
    assert_eq!(
        editor_localisation_languages(&data),
        vec!["english", "french", "simp_chinese"]
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn editor_search_uses_one_authoritative_overlay_in_every_source() {
    let (root, mut host) = roots_fixture("editor-search-overlays");
    let path = AbsPath::normalize(&root.join("dep-a/localisation/dep-a_l_english.yml"));
    let raw = DocumentId::new(format!("file://{}", path.display()));
    let decoded = DocumentId::new(format!("pdcloc://{}", path.display()));
    host.open_document(
        raw.clone(),
        1,
        "l_english:\nraw_only:0 \"wrong twin\"\n".to_owned(),
        Some(path.clone()),
    )
    .unwrap();
    host.open_document(decoded.clone(),2,"l_english:\nnew_key:0 \"unsaved dependency text\"\nshared:0 \"dependency still overridden\"\n".to_owned(),Some(path)).unwrap();
    let data = editor_localisations(&host.snapshot(), &CancellationToken::new()).unwrap();
    assert!(
        !data
            .entries
            .iter()
            .any(|entry| entry.key() == "raw_only" || entry.key() == "unique_1")
    );
    let new = data
        .entries
        .iter()
        .filter(|entry| entry.key() == "new_key")
        .collect::<Vec<_>>();
    assert_eq!(new.len(), 1);
    assert_eq!(new[0].version(), Some(2));
    assert_eq!(new[0].root(), Some(SourceRootId::new(11)));
    assert_eq!(new[0].location().document, Some(decoded));
    assert!(
        !data
            .entries
            .iter()
            .find(|entry| entry.value() == "dependency still overridden")
            .unwrap()
            .active
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn editor_search_definition_identity_keeps_overridden_versions() {
    let (root, host) = roots_fixture("editor-search-definitions");
    let definitions = editor_definitions(&host.snapshot(), &CancellationToken::new()).unwrap();
    let snapshot = host.snapshot();
    let resolver = crate::resolution::DirectResolutionContext::new(&snapshot);
    for entry in definitions.iter() {
        let expected = match resolver.resolve(&entry.kind, &entry.name) {
            crate::resolution::Resolution::Unique(winner) => {
                winner.location.document == entry.location.document
                    && winner.location.file == entry.location.file
                    && winner.selection_range == entry.location.range
            }
            crate::resolution::Resolution::Missing => false,
        };
        assert_eq!(
            entry.active, expected,
            "batched activity must agree with point resolution"
        );
    }
    let shared = definitions
        .iter()
        .filter(|entry| entry.name == "shared.1")
        .collect::<Vec<_>>();
    assert_eq!(shared.len(), 4);
    assert_eq!(shared.iter().filter(|entry| entry.active).count(), 1);
    assert_eq!(
        shared.iter().find(|entry| entry.active).unwrap().root,
        Some(SourceRootId::new(13))
    );
    assert_eq!(editor_name_score("shared.1", "shared.1"), Some(0));
    assert_eq!(editor_name_score("shared.1", "sha"), Some(1));
    assert_eq!(editor_name_score("shared.1", "are"), Some(2));
    assert_eq!(editor_name_score("shared.1", "srd1"), Some(3));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn editor_search_same_named_definition_types_and_translation_languages_keep_identity() {
    let (root, mut host) = roots_fixture("editor-search-distinct-identities");
    for (directory, body) in [
        ("scripted_effects", "add_prestige = 1"),
        ("scripted_triggers", "always = yes"),
    ] {
        let path = root.join("mod/common").join(directory);
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(
            path.join("owned.txt"),
            format!("same_name = {{ {body} }}\n"),
        )
        .unwrap();
    }
    std::fs::write(
        root.join("mod/localisation/two_languages.yml"),
        "l_english:\nsame_name:0 \"English needle\"\nl_french:\nsame_name:0 \"French needle\"\n",
    )
    .unwrap();
    host.refresh_source_roots().unwrap();
    let snapshot = host.snapshot();
    let definitions = editor_definitions(&snapshot, &CancellationToken::new()).unwrap();
    let matching = definitions
        .iter()
        .filter(|entry| entry.name == "same_name")
        .collect::<Vec<_>>();
    assert_eq!(matching.len(), 2);
    assert!(matching.iter().all(|entry| entry.active));
    assert_ne!(matching[0].kind, matching[1].kind);
    let localisations = editor_localisations(&snapshot, &CancellationToken::new()).unwrap();
    let matching = localisations
        .entries
        .iter()
        .filter(|entry| entry.key() == "same_name")
        .collect::<Vec<_>>();
    assert_eq!(matching.len(), 2);
    assert!(matching.iter().all(|entry| entry.active));
    assert_ne!(matching[0].language(), matching[1].language());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn editor_search_rules_keep_contexts_unrestricted_scopes_and_pattern_keys() {
    let host = eu4_host(game::eu4::bootstrap_rules());
    let snapshot = host.snapshot();
    let token = CancellationToken::new();
    let all = editor_rule_search(&snapshot, "", None, None, &token).unwrap();
    let country = editor_rule_search(&snapshot, "", None, Some("country"), &token).unwrap();
    assert!(!country.is_empty());
    let selected = country
        .iter()
        .map(|rule| &rule.id)
        .collect::<std::collections::BTreeSet<_>>();
    for rule in &all {
        assert_eq!(
            selected.contains(&rule.id),
            rule.allowed_scopes.is_empty()
                || rule.allowed_scopes.iter().any(|scope| scope == "country")
        );
    }
    let effect = editor_rule_search(
        &snapshot,
        "add_army_tradition",
        Some("effect"),
        None,
        &token,
    )
    .unwrap();
    assert!(!effect.is_empty());
    assert!(effect.iter().all(|rule| rule.context == "effect"));
    let mut contexts = std::collections::BTreeMap::<&str, std::collections::BTreeSet<&str>>::new();
    for rule in &all {
        contexts
            .entry(&rule.name)
            .or_default()
            .insert(&rule.context);
    }
    assert!(contexts.values().any(|contexts| contexts.len() > 1));
    let (schema, field) = snapshot
        .ir()
        .schemas
        .iter()
        .enumerate()
        .find_map(|(index, schema)| schema.patterns.first().map(|field| (index, field)))
        .unwrap();
    let id = format!("ir:{schema}:{}", field.index());
    let pattern = all.iter().find(|rule| rule.id == id).unwrap();
    let found = editor_rule_search(
        &snapshot,
        &pattern.name,
        Some(&pattern.context),
        None,
        &token,
    )
    .unwrap();
    assert!(found.iter().any(|rule| rule.id == id));
}

#[test]
fn editor_search_full_text_includes_unclassified_text_and_observes_globs_overlays_and_utf16() {
    let (root, mut host) = roots_fixture("editor-search-text");
    for name in ["vanilla", "dep-a", "dep-b", "mod"] {
        std::fs::create_dir_all(root.join(name).join("notes")).unwrap();
        std::fs::write(
            root.join(name).join("notes/free.txt"),
            "# 🙂 Empire Empire\n",
        )
        .unwrap();
    }
    let path = AbsPath::normalize(&root.join("dep-b/notes/free.txt"));
    let id = DocumentId::new(format!("file://{}", path.display()));
    host.open_document(
        id.clone(),
        8,
        "# 🙂 Empire unsaved\n".to_owned(),
        Some(path),
    )
    .unwrap();
    let snapshot = host.snapshot();
    let data = editor_text_search(
        &snapshot,
        "empire",
        None,
        false,
        &["notes/**".to_owned()],
        &[],
        &CancellationToken::new(),
    )
    .unwrap();
    assert_eq!(data.hits.len(), 7);
    assert_eq!(
        data.hits
            .iter()
            .filter(|hit| hit.root == SourceRootId::new(12))
            .count(),
        1
    );
    let hit = data
        .hits
        .iter()
        .find(|hit| hit.document == Some(id.clone()))
        .unwrap();
    assert_eq!(
        &hit.text[hit.range.start() as usize..hit.range.end() as usize],
        "Empire"
    );
    assert_eq!(hit.version, Some(8));
    let filtered = editor_text_search(
        &snapshot,
        "Empire",
        Some(&[13]),
        true,
        &["notes/**".to_owned()],
        &["**/free.txt".to_owned()],
        &CancellationToken::new(),
    )
    .unwrap();
    assert!(filtered.hits.is_empty());
    let token = CancellationToken::new();
    token.cancel();
    assert!(editor_text_search(&snapshot, "Empire", None, false, &[], &[], &token).is_err());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn editor_search_transcoded_hits_map_to_original_and_decoded_document_ranges() {
    let (root, mut host) = roots_fixture("editor-search-encoded");
    let encoded = transcode::encode_text("帝国", transcode::EscapeSet::Paratranz).unwrap();
    let path = AbsPath::normalize(&root.join("mod/localisation/encoded_l_english.yml"));
    let source = format!("l_english:\nencoded:0 \"§R{encoded}§! 改革\"\n");
    std::fs::write(&path, &source).unwrap();
    host.refresh_source_roots().unwrap();
    let data = editor_localisations(&host.snapshot(), &CancellationToken::new()).unwrap();
    let entry = data
        .entries
        .iter()
        .find(|entry| entry.key() == "encoded")
        .unwrap();
    assert!(localisation_matches(&entry.value(), "帝国"));
    let hit = editor_localisation_match_location(entry, "帝国").unwrap();
    assert_eq!(
        &source[hit.range.start() as usize..hit.range.end() as usize],
        encoded
    );
    let text = editor_text_search(
        &host.snapshot(),
        "帝国",
        None,
        false,
        &[],
        &[],
        &CancellationToken::new(),
    )
    .unwrap();
    let hit = text
        .hits
        .iter()
        .find(|hit| hit.physical_path == path)
        .unwrap();
    assert_eq!(
        &source[hit.range.start() as usize..hit.range.end() as usize],
        encoded
    );
    let decoded = DocumentId::new(format!("pdcloc://{}", path.display()));
    host.open_document(
        decoded.clone(),
        1,
        "l_english:\nencoded:0 \"§R帝国§! 改革\"\n".to_owned(),
        Some(path),
    )
    .unwrap();
    let data = editor_localisations(&host.snapshot(), &CancellationToken::new()).unwrap();
    let entry = data
        .entries
        .iter()
        .find(|entry| entry.key() == "encoded")
        .unwrap();
    let hit = editor_localisation_match_location(entry, "帝国").unwrap();
    assert_eq!(hit.document, Some(decoded));
    assert_eq!(
        &entry.source()[hit.range.start() as usize..hit.range.end() as usize],
        "帝国"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn editor_search_respects_file_overrides_and_does_not_promote_dependency_buffers() {
    let (root, mut host) = roots_fixture("editor-search-priority");
    let lower = root.join("dep-a/localisation/shared_l_english.yml");
    let higher = root.join("mod/localisation/shared_l_english.yml");
    std::fs::write(&lower, "l_english:\nmasked_only:0 \"old file value\"\n").unwrap();
    std::fs::write(&higher, "l_english:\nreplacement:0 \"new file value\"\n").unwrap();
    host.refresh_source_roots().unwrap();
    let path = AbsPath::normalize(&root.join("dep-a/events/dep-a.txt"));
    host.open_document(
        DocumentId::new(format!("file://{}", path.display())),
        1,
        "country_event = { id = shared.1 }\n".to_owned(),
        Some(path),
    )
    .unwrap();
    let snapshot = host.snapshot();
    let localisations = editor_localisations(&snapshot, &CancellationToken::new()).unwrap();
    assert!(
        !localisations
            .entries
            .iter()
            .find(|entry| entry.key() == "masked_only")
            .unwrap()
            .active
    );
    let definitions = editor_definitions(&snapshot, &CancellationToken::new()).unwrap();
    let shared = definitions
        .iter()
        .filter(|entry| entry.name == "shared.1")
        .collect::<Vec<_>>();
    assert_eq!(shared.iter().filter(|entry| entry.active).count(), 1);
    assert_eq!(
        shared.iter().find(|entry| entry.active).unwrap().root,
        Some(SourceRootId::new(13))
    );
    assert!(
        !shared
            .iter()
            .find(|entry| entry.location.document.is_some())
            .unwrap()
            .active
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn editor_search_merge_files_keep_other_sources_when_one_file_has_an_overlay() {
    let (root, mut host) = roots_fixture("editor-search-merge");
    for name in ["dep-a", "mod"] {
        std::fs::write(
            root.join(name).join("events/shared.txt"),
            "country_event = { id = merge.1 }\n",
        )
        .unwrap();
    }
    host.refresh_source_roots().unwrap();
    let path = AbsPath::normalize(&root.join("dep-a/events/shared.txt"));
    let id = DocumentId::new(format!("file://{}", path.display()));
    host.open_document(
        id.clone(),
        1,
        "country_event = { id = merge.2 }\n".to_owned(),
        Some(path.clone()),
    )
    .unwrap();
    let snapshot = host.snapshot();
    let candidates = snapshot.resolve(&text::LogicalPath::parse("events/shared.txt").unwrap());
    assert_eq!(
        candidates
            .iter()
            .filter(|candidate| candidate.active)
            .count(),
        2
    );
    assert!(
        candidates
            .iter()
            .any(|candidate| candidate.document_id.as_ref() == Some(&id) && candidate.active)
    );
    let old = snapshot.source_file_id_for_path(&path).unwrap();
    assert!(!snapshot.source_is_effective(None, Some(old)));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn editor_search_path_filters_precede_reads_and_external_epochs_refresh_unindexed_text() {
    let (root, host) = roots_fixture("editor-search-file-epochs");
    std::fs::create_dir_all(root.join("mod/notes")).unwrap();
    let path = root.join("mod/notes/readme.txt");
    std::fs::write(&path, "first value\n").unwrap();
    std::fs::write(root.join("mod/notes/bad.unknown"), [0xff, 0, 0xff]).unwrap();
    let snapshot = host.snapshot();
    let include = vec!["notes/readme.txt".to_owned()];
    let query = EditorTextQuery {
        query: "first",
        roots: Some(&[13]),
        case_sensitive: false,
        include: &include,
        exclude: &[],
        epoch: 1,
        buffers: &[],
    };
    let first = editor_text_search_query(&snapshot, &query, &CancellationToken::new()).unwrap();
    assert_eq!(first.hits.len(), 1);
    assert!(first.limitations.is_empty());
    std::fs::write(&path, "second value\n").unwrap();
    let frozen = editor_text_search_query(&snapshot, &query, &CancellationToken::new()).unwrap();
    assert_eq!(frozen.generation, first.generation);
    assert_eq!(frozen.hits.len(), 1);
    let fresh = editor_text_search_query(
        &snapshot,
        &EditorTextQuery {
            query: "second",
            epoch: 2,
            ..query
        },
        &CancellationToken::new(),
    )
    .unwrap();
    assert_eq!(fresh.hits.len(), 1);
    assert_ne!(fresh.generation, first.generation);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn editor_search_foreign_buffers_replace_disk_and_observe_source_exclusions() {
    let (root, mut host) = roots_fixture("editor-search-foreign-buffer");
    let path = AbsPath::normalize(&root.join("mod/notes/readme.md"));
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "old disk text\n").unwrap();
    let buffers = vec![EditorTextBuffer {
        document: DocumentId::new(format!("file://{}", path.display())),
        path: path.clone(),
        version: Some(4),
        text: std::sync::Arc::from("unsaved text needle\n"),
    }];
    let options = EditorTextQuery {
        query: "needle",
        roots: Some(&[13]),
        case_sensitive: false,
        include: &[],
        exclude: &[],
        epoch: 1,
        buffers: &buffers,
    };
    let result =
        editor_text_search_query(&host.snapshot(), &options, &CancellationToken::new()).unwrap();
    assert_eq!(result.hits.len(), 1);
    assert_eq!(result.hits[0].version, Some(4));
    let disk = editor_text_search_query(
        &host.snapshot(),
        &EditorTextQuery {
            query: "old disk",
            ..options
        },
        &CancellationToken::new(),
    )
    .unwrap();
    assert!(disk.hits.is_empty());
    host.set_scan_filters(
        engine::WorkspaceScanFilters::new(vec!["notes/**".to_owned()], Vec::new()).unwrap(),
    );
    let excluded =
        editor_text_search_query(&host.snapshot(), &options, &CancellationToken::new()).unwrap();
    assert!(excluded.hits.is_empty());
    assert_eq!(std::fs::read_to_string(path).unwrap(), "old disk text\n");
    std::fs::remove_dir_all(root).unwrap();
}
