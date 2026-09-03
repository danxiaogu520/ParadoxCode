use super::*;

#[test]
fn previous_cache_schema_is_rejected_before_table_loading() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-engine-old-schema-cache-{nonce}"));
    let vanilla = root.join("vanilla");
    fs::create_dir_all(vanilla.join("events")).expect("event directory");
    fs::write(
        vanilla.join("events/schema.txt"),
        "country_event = { id = schema.1 }\n",
    )
    .expect("schema fixture");

    let mut host = AnalysisHost::with_profile(
        pdx_game::eu4::first_party_rules().expect("first-party rules"),
        pdx_game::eu4::profile(),
    );
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(0),
        SourceRootKind::Vanilla,
        fs::canonicalize(&vanilla).expect("canonical Vanilla root"),
    )]));
    host.refresh_source_roots().expect("scan Vanilla");
    let cache = IndexCache::from_snapshot(&host.snapshot()).expect("build cache");
    let cache_path = root.join("cache/vanilla.pdxindex");
    cache.save(&cache_path).expect("save cache");

    let connection = rusqlite::Connection::open(&cache_path).expect("open cache metadata");
    connection
        .execute(
            "UPDATE metadata SET value = ?1 WHERE key = 'schema_version'",
            rusqlite::params![b"9".as_slice()],
        )
        .expect("mark cache as previous schema");
    drop(connection);
    assert!(matches!(
        IndexCache::load(&cache_path),
        Err(IndexCacheError::UnsupportedSchema(9))
    ));
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn vanilla_cache_preserves_scripted_macro_references_without_hir() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-engine-vanilla-scripted-cache-{nonce}"));
    let vanilla = root.join("vanilla");
    fs::create_dir_all(vanilla.join("common/scripted_effects")).expect("macro directory");
    fs::create_dir_all(vanilla.join("events")).expect("event directory");
    fs::write(
        vanilla.join("common/scripted_effects/defs.txt"),
        "cached_effect = { value = $amount$ [[optional] value = $optional$ ] }\n",
    )
    .expect("macro definitions");
    fs::write(
        vanilla.join("events/use.txt"),
        "country_event = { immediate = { cached_effect = { amount = 1 } } }\n",
    )
    .expect("macro call");

    let rules = pdx_game::eu4::first_party_rules().expect("first-party rules");
    let mut host = AnalysisHost::with_profile(rules, pdx_game::eu4::profile());
    host.apply_change(super::WorkspaceChange::SetSourceRoots(vec![
        SourceRoot::new(
            SourceRootId::new(0),
            SourceRootKind::Vanilla,
            fs::canonicalize(&vanilla).expect("canonical Vanilla root"),
        ),
    ]));
    host.refresh_source_roots().expect("scan Vanilla");
    let snapshot = host.snapshot();
    assert!(snapshot.source_files().keys().all(|file_id| {
        snapshot
            .file_state(*file_id)
            .is_some_and(|state| state.parsed().is_none() && state.hir().is_none())
    }));
    assert!(snapshot.index().references_iter().any(|reference| {
        reference.kind.as_ref() == "scripted_effect" && reference.name.as_ref() == "cached_effect"
    }));
    let signature = snapshot
        .index()
        .active_macro_definition("scripted_effect", "cached_effect")
        .expect("signature before caching");
    assert!(
        signature.template.is_some(),
        "live index omitted macro template"
    );
    assert_eq!(
        signature.parameters,
        vec![
            MacroParameterSignature {
                name: "amount".to_owned(),
                required: true,
            },
            MacroParameterSignature {
                name: "optional".to_owned(),
                required: false,
            },
        ]
    );

    let cache = IndexCache::from_snapshot(&snapshot).expect("build cache");
    let cache_path = root.join("cache/vanilla.pdxindex");
    cache.save(&cache_path).expect("save cache");
    let loaded = IndexCache::load(&cache_path).expect("load cache");
    assert!(loaded.index().references_iter().any(|reference| {
        reference.kind.as_ref() == "scripted_effect" && reference.name.as_ref() == "cached_effect"
    }));
    assert_eq!(
        loaded
            .index()
            .active_macro_definition("scripted_effect", "cached_effect")
            .expect("signature after caching"),
        signature
    );
    let connection = rusqlite::Connection::open(&cache_path).expect("open cache for corruption");
    connection
        .execute(
            "UPDATE macro_definitions SET template_payload = ?1 WHERE name = ?2",
            rusqlite::params![b"{}".as_slice(), "cached_effect"],
        )
        .expect("corrupt template payload");
    drop(connection);
    assert!(matches!(
        IndexCache::load(&cache_path),
        Err(IndexCacheError::InvalidData(_))
    ));
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn definition_attribute_summaries_survive_live_and_cached_indexing() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-engine-attr-cache-{nonce}"));
    let vanilla = root.join("vanilla");
    fs::create_dir_all(vanilla.join("common/event_modifiers")).expect("modifier directory");
    fs::write(
        vanilla.join("common/event_modifiers/00_test.txt"),
        "war_mod = { global_tax_modifier = 0.1 local_unrest = -1 }
",
    )
    .expect("event modifier");

    let rules = pdx_game::eu4::first_party_rules().expect("first-party rules");
    let mut host = AnalysisHost::with_profile(rules, pdx_game::eu4::profile());
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(0),
        SourceRootKind::Vanilla,
        fs::canonicalize(&vanilla).expect("canonical Vanilla root"),
    )]));
    host.refresh_source_roots().expect("scan Vanilla");
    let snapshot = host.snapshot();
    let live = snapshot
        .index()
        .active_definition_attributes("event_modifier", "war_mod")
        .expect("live attribute summary")
        .clone();
    assert_eq!(
        live.attribute_keys,
        vec!["global_tax_modifier".to_owned(), "local_unrest".to_owned()]
    );

    let cache = IndexCache::from_snapshot(&snapshot).expect("build cache");
    let cache_path = root.join("cache/vanilla.pdxindex");
    cache.save(&cache_path).expect("save cache");
    let loaded = IndexCache::load(&cache_path).expect("load cache");
    assert_eq!(
        loaded
            .index()
            .active_definition_attributes("event_modifier", "war_mod")
            .expect("cached attribute summary"),
        &live
    );
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn corrupted_navigation_position_is_rejected_without_symbol_table_scans() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-engine-vanilla-position-cache-{nonce}"));
    let vanilla = root.join("vanilla");
    fs::create_dir_all(vanilla.join("events")).expect("event directory");
    fs::write(
        vanilla.join("events/position.txt"),
        "country_event = { id = corrupted.position }",
    )
    .expect("vanilla event");

    let rules = pdx_game::eu4::first_party_rules().expect("first-party rules");
    let mut host = AnalysisHost::with_profile(rules, pdx_game::eu4::profile());
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(0),
        SourceRootKind::Vanilla,
        fs::canonicalize(&vanilla).expect("canonical Vanilla root"),
    )]));
    host.refresh_source_roots().expect("scan Vanilla");
    let cache = IndexCache::from_snapshot(&host.snapshot()).expect("build cache");
    let cache_path = root.join("cache/vanilla.pdxindex");
    cache.save(&cache_path).expect("save cache");
    assert!(IndexCache::load(&cache_path).is_ok(), "valid cache loads");

    let connection = rusqlite::Connection::open(&cache_path).expect("open cache for corruption");
    connection
        .execute("UPDATE navigation_positions SET payload = X'00'", [])
        .expect("corrupt navigation payload");
    drop(connection);
    assert!(matches!(
        IndexCache::load(&cache_path),
        Err(IndexCacheError::InvalidData(_))
    ));
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn refreshed_cache_reindexes_changed_files_and_drops_deleted_ones() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-engine-refresh-cache-{nonce}"));
    let vanilla = root.join("vanilla");
    fs::create_dir_all(vanilla.join("events")).expect("event directory");
    fs::create_dir_all(vanilla.join("common/scripted_effects")).expect("macro directory");
    fs::write(
        vanilla.join("events/a.txt"),
        "country_event = { id = refresh.1 }\n",
    )
    .expect("first fixture");
    fs::write(
        vanilla.join("events/b.txt"),
        "country_event = { id = refresh.2 }\n",
    )
    .expect("second fixture");
    fs::write(
        vanilla.join("common/scripted_effects/effect.txt"),
        "refresh_effect = { value = $amount$ }\n",
    )
    .expect("macro fixture");

    let rules = pdx_game::eu4::first_party_rules().expect("first-party rules");
    let mut host = AnalysisHost::with_profile(rules.clone(), pdx_game::eu4::profile());
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(0),
        SourceRootKind::Vanilla,
        fs::canonicalize(&vanilla).expect("canonical Vanilla root"),
    )]));
    host.refresh_source_roots().expect("scan Vanilla");
    let cache = IndexCache::from_snapshot(&host.snapshot()).expect("build cache");
    let cache_path = root.join("cache/vanilla.pdxindex");
    cache.save(&cache_path).expect("save cache");
    let file_ids = cache.source_files().keys().copied().collect::<Vec<_>>();
    assert_eq!(file_ids.len(), 3);

    // Change one file, delete another, and add a third before refreshing.
    fs::write(
        vanilla.join("events/a.txt"),
        "country_event = { id = refresh.1b }\n",
    )
    .expect("modified fixture");
    fs::remove_file(vanilla.join("events/b.txt")).expect("remove fixture");
    fs::write(
        vanilla.join("events/c.txt"),
        "country_event = { id = refresh.3 }\n",
    )
    .expect("added fixture");
    let loaded = IndexCache::load(&cache_path).expect("load cache");
    let refreshed = loaded
        .refresh(&rules, &pdx_game::eu4::profile())
        .expect("refresh");
    assert_eq!(refreshed.metadata().indexed_files, 3);
    assert_ne!(
        refreshed.metadata().source_fingerprint,
        loaded.metadata().source_fingerprint,
        "the tree fingerprint must track the changed content"
    );
    // The refreshed cache round-trips through a save and a full load, which derives the
    // symbol lookup maps exactly like a freshly built cache.
    let refreshed_path = root.join("cache/refreshed.pdxindex");
    refreshed
        .save(&refreshed_path)
        .expect("save refreshed cache");
    let reloaded = IndexCache::load(&refreshed_path).expect("load refreshed cache");
    assert_eq!(reloaded.metadata(), refreshed.metadata());
    let definitions = reloaded
        .index()
        .definitions_iter()
        .map(|definition| &*definition.name)
        .collect::<Vec<_>>();
    assert!(
        definitions.contains(&"refresh.1b"),
        "definitions: {definitions:?}"
    );
    assert!(
        definitions.contains(&"refresh.3"),
        "definitions: {definitions:?}"
    );
    assert!(
        !definitions.contains(&"refresh.1"),
        "definitions: {definitions:?}"
    );
    assert!(
        !definitions.contains(&"refresh.2"),
        "definitions: {definitions:?}"
    );
    assert!(
        reloaded
            .index()
            .active_macro_definition("scripted_effect", "refresh_effect")
            .is_some(),
        "unchanged macro definition must survive the refresh"
    );
    // The unchanged file keeps its identity and fingerprint.
    let unchanged = reloaded
        .source_files()
        .values()
        .find(|file| file.logical_path.as_str() == "events/a.txt")
        .expect("unchanged path stays indexed");
    assert!(file_ids.contains(&unchanged.id));
    assert!(reloaded.file_fingerprint(unchanged.id).is_some());
    assert!(
        reloaded.file_metadata_fingerprint(unchanged.id).is_some(),
        "metadata fingerprint enables a no-read refresh on supported filesystems"
    );
    // Positions must be retained for every surviving symbol.
    let definition = reloaded
        .index()
        .definitions_iter()
        .find(|definition| definition.file_id == unchanged.id)
        .expect("definition in refreshed file");
    assert!(
        reloaded
            .index()
            .position_for(unchanged.id, definition.range)
            .is_some()
    );
    // A refresh with no changes reproduces the same tree fingerprint.
    let refreshed_again = refreshed
        .refresh(&rules, &pdx_game::eu4::profile())
        .expect("idempotent refresh");
    assert_eq!(
        refreshed_again.metadata().source_fingerprint,
        refreshed.metadata().source_fingerprint
    );
    assert_eq!(
        refreshed_again.metadata().indexed_files,
        refreshed.metadata().indexed_files
    );
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn refresh_rejects_stale_rules_and_mismatched_games() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-engine-refresh-reject-{nonce}"));
    let vanilla = root.join("vanilla");
    fs::create_dir_all(vanilla.join("events")).expect("event directory");
    fs::write(
        vanilla.join("events/a.txt"),
        "country_event = { id = reject.1 }\n",
    )
    .expect("fixture");

    let bootstrap = pdx_game::eu4::bootstrap_rules();
    let mut host = AnalysisHost::with_profile(bootstrap.clone(), pdx_game::eu4::profile());
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(0),
        SourceRootKind::Vanilla,
        fs::canonicalize(&vanilla).expect("canonical Vanilla root"),
    )]));
    host.refresh_source_roots().expect("scan Vanilla");
    let cache = IndexCache::from_snapshot(&host.snapshot()).expect("build cache");
    let cache_path = root.join("cache/vanilla.pdxindex");
    cache.save(&cache_path).expect("save cache");
    let loaded = IndexCache::load(&cache_path).expect("load cache");

    let first_party = pdx_game::eu4::first_party_rules().expect("first-party rules");
    assert_ne!(
        first_party.rule_hash().to_hex(),
        loaded.metadata().rule_hash,
        "bootstrap and first-party rules must differ for this test"
    );
    assert!(matches!(
        loaded.refresh(&first_party, &pdx_game::eu4::profile()),
        Err(IndexCacheError::RuleHashMismatch { .. })
    ));
    assert!(matches!(
        loaded.refresh(&RuleSet::empty(), &pdx_game::eu4::profile()),
        Err(IndexCacheError::GameMismatch { .. })
    ));
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn save_reclaims_free_pages_when_rebuilding_a_smaller_cache() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-engine-shrink-cache-{nonce}"));
    let cache_path = root.join("cache/vanilla.pdxindex");

    let rules = pdx_game::eu4::bootstrap_rules();
    let build_cache = |files: usize, suffix: &str| {
        let vanilla = root.join(format!("vanilla-{suffix}"));
        fs::create_dir_all(vanilla.join("events")).expect("event directory");
        let mut body = String::new();
        for index in 0..files {
            body.push_str(&format!(
                "country_event = {{ id = shrink.{suffix}.{index} }}\n"
            ));
        }
        fs::write(vanilla.join("events/a.txt"), body).expect("fixture");
        let mut host = AnalysisHost::with_profile(rules.clone(), pdx_game::eu4::profile());
        host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
            SourceRootId::new(0),
            SourceRootKind::Vanilla,
            fs::canonicalize(&vanilla).expect("canonical Vanilla root"),
        )]));
        host.refresh_source_roots().expect("scan Vanilla");
        IndexCache::from_snapshot(&host.snapshot()).expect("build cache")
    };

    let large = build_cache(20_000, "large");
    large.save(&cache_path).expect("save large cache");
    let large_len = fs::metadata(&cache_path)
        .expect("large cache metadata")
        .len();

    let small = build_cache(100, "small");
    small
        .save(&cache_path)
        .expect("overwrite with smaller cache");
    let small_len = fs::metadata(&cache_path)
        .expect("small cache metadata")
        .len();
    assert!(
        small_len < large_len,
        "rebuild must reclaim dropped pages: {small_len} bytes after {large_len} bytes"
    );
    let connection = rusqlite::Connection::open(&cache_path).expect("open cache");
    let freelist: i64 = connection
        .query_row("PRAGMA freelist_count", [], |row| row.get(0))
        .expect("freelist count");
    assert_eq!(freelist, 0, "no free pages may remain after the rebuild");
    let auto_vacuum: i64 = connection
        .pragma_query_value(None, "auto_vacuum", |row| row.get(0))
        .expect("auto-vacuum mode");
    assert_eq!(
        auto_vacuum, 2,
        "fresh caches must run in incremental auto-vacuum"
    );
    drop(connection);
    IndexCache::load(&cache_path).expect("rebuilt cache loads");
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn persistent_vanilla_cache_round_trips_and_is_never_rescanned() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-engine-vanilla-cache-{nonce}"));
    let vanilla = root.join("vanilla");
    let current = root.join("current");
    fs::create_dir_all(vanilla.join("events")).expect("Vanilla fixture directory");
    fs::create_dir_all(vanilla.join("localisation/nested/deeper"))
        .expect("Vanilla localisation fixture directory");
    fs::create_dir_all(current.join("events")).expect("current fixture directory");
    fs::write(
        vanilla.join("events/definitions.txt"),
        "country_event = { id = shared.1 }\ncountry_event = { id = vanilla.1 }\n",
    )
    .expect("Vanilla definitions");
    fs::write(
        vanilla.join("localisation/nested/deeper/test_l_english.yml"),
        "l_english:\nvanilla_name:0 \"Vanilla text\"\n",
    )
    .expect("Vanilla localisation");
    fs::write(
        current.join("events/definitions.txt"),
        "country_event = { id = shared.1 }\n",
    )
    .expect("current definition");

    let mut vanilla_host = eu4_host();
    vanilla_host.apply_change(super::WorkspaceChange::SetSourceRoots(vec![
        SourceRoot::new(
            SourceRootId::new(0),
            SourceRootKind::Vanilla,
            fs::canonicalize(&vanilla).expect("canonical Vanilla root"),
        ),
    ]));
    vanilla_host
        .refresh_source_roots()
        .expect("scan Vanilla once");
    let vanilla_snapshot = vanilla_host.snapshot();
    assert!(vanilla_snapshot.source_files().keys().all(|file_id| {
        vanilla_snapshot
            .file_state(*file_id)
            .is_some_and(|state| state.parsed().is_none() && state.hir().is_none())
    }));
    let cache = IndexCache::from_snapshot(&vanilla_host.snapshot()).expect("build cache");
    let vanilla_positions = cache.index().position_ranges().clone();
    let cache_path = root.join("cache/vanilla.pdxindex");
    cache.save(&cache_path).expect("save cache");
    let cancelled = WorkspaceScanToken::new();
    cancelled.cancel();
    assert!(matches!(
        IndexCache::load_cancellable(&cache_path, &cancelled),
        Err(IndexCacheError::Cancelled)
    ));
    let loaded = IndexCache::load(&cache_path).expect("load cache");
    assert_eq!(loaded.metadata(), cache.metadata());
    assert_eq!(loaded.source_files(), cache.source_files());
    assert_eq!(loaded.index(), cache.index());
    assert_eq!(
        loaded.localisation_previews(),
        cache.localisation_previews()
    );

    let foreign_path = root.join("foreign.sqlite");
    let foreign = rusqlite::Connection::open(&foreign_path).expect("foreign database");
    foreign
        .execute("CREATE TABLE marker(value TEXT)", [])
        .expect("foreign schema");
    drop(foreign);
    assert!(matches!(
        cache.save(&foreign_path),
        Err(IndexCacheError::NotIndexCache)
    ));
    let foreign = rusqlite::Connection::open(&foreign_path).expect("reopen foreign database");
    assert_eq!(
        foreign
            .query_row("SELECT count(*) FROM marker", [], |row| row
                .get::<_, i64>(0))
            .expect("foreign table remains"),
        0
    );
    drop(foreign);

    fs::rename(&vanilla, root.join("vanilla-moved")).expect("make original source unavailable");
    let mut host = eu4_host();
    host.apply_change(super::WorkspaceChange::SetSourceRoots(vec![
        SourceRoot::new(
            SourceRootId::new(u32::MAX),
            SourceRootKind::CurrentMod,
            fs::canonicalize(&current).expect("canonical current root"),
        ),
    ]));
    host.refresh_source_roots().expect("scan current root");
    let current_positions = host.snapshot().index().position_ranges().clone();
    host.install_index_cache(loaded)
        .expect("install cache without Vanilla source access");
    host.refresh_source_roots()
        .expect("refresh must skip unavailable Vanilla root");

    let snapshot = host.snapshot();
    assert_eq!(snapshot.source_roots()[0].kind, SourceRootKind::Vanilla);
    let shared = snapshot
        .index()
        .active_definition("event", "shared.1")
        .expect("current definition wins");
    assert_eq!(
        snapshot
            .source_files()
            .get(&shared.file_id)
            .expect("shared file")
            .root_id,
        SourceRootId::new(u32::MAX)
    );
    assert_eq!(
        snapshot.index().position_ranges().len(),
        vanilla_positions.len() + current_positions.len(),
        "install must retain both cached and live position ranges"
    );
    for (key, position) in vanilla_positions.iter().chain(current_positions.iter()) {
        assert_eq!(
            snapshot.index().position_ranges().get(key),
            Some(position),
            "position range must survive cache installation"
        );
    }
    let vanilla_definition = snapshot
        .index()
        .active_definition("event", "vanilla.1")
        .expect("cached Vanilla-only definition remains available");
    assert_eq!(
        snapshot
            .source_files()
            .get(&vanilla_definition.file_id)
            .expect("Vanilla file metadata")
            .root_id,
        SourceRootId::new(0)
    );
    assert!(snapshot.file_state(vanilla_definition.file_id).is_none());
    let vanilla_localisation = snapshot
        .index()
        .active_definition("localisation", "vanilla_name")
        .expect("cached Vanilla localisation remains available");
    let preview = snapshot
        .localisation_preview(vanilla_localisation.file_id, vanilla_localisation.range)
        .expect("cached Vanilla localisation preview");
    assert_eq!(preview.language.as_deref(), Some("l_english"));
    assert_eq!(preview.value, "Vanilla text");
    assert!(snapshot.file_state(vanilla_localisation.file_id).is_none());

    // Installing another cache must retain positions from the first cache as well as the live
    // workspace.  The old per-file replacement loop discarded previously installed cache ranges.
    let dependency = root.join("dependency");
    fs::create_dir_all(dependency.join("events")).expect("dependency events directory");
    fs::write(
        dependency.join("events/dependency.txt"),
        "country_event = { id = dependency.1 }\n",
    )
    .expect("dependency definition");
    let dependency_root = SourceRoot::new(
        SourceRootId::new(7),
        SourceRootKind::Dependency,
        fs::canonicalize(&dependency).expect("canonical dependency root"),
    );
    let mut dependency_builder = eu4_host();
    dependency_builder.apply_change(super::WorkspaceChange::SetSourceRoots(vec![
        dependency_root.clone(),
    ]));
    dependency_builder
        .refresh_source_roots()
        .expect("scan dependency");
    let dependency_cache =
        IndexCache::from_snapshot(&dependency_builder.snapshot()).expect("build dependency cache");
    let dependency_positions = dependency_cache.index().position_ranges().clone();
    let dependency_path = root.join("cache/dependency.pdxindex");
    dependency_cache
        .save(&dependency_path)
        .expect("save dependency cache");
    host.install_index_cache(IndexCache::load(&dependency_path).expect("load dependency cache"))
        .expect("install dependency cache");
    let after_dependency = host.snapshot();
    assert_eq!(
        after_dependency.index().position_ranges().len(),
        vanilla_positions.len() + current_positions.len() + dependency_positions.len(),
        "successive cache installs must retain all position ranges"
    );
    for (key, position) in vanilla_positions
        .iter()
        .chain(current_positions.iter())
        .chain(dependency_positions.iter())
    {
        assert_eq!(
            after_dependency.index().position_ranges().get(key),
            Some(position),
            "successive cache installation must preserve every position range"
        );
    }
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn vanilla_cache_previews_retain_only_preferred_languages() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-engine-preview-retention-{nonce}"));
    let vanilla = root.join("vanilla");
    fs::create_dir_all(vanilla.join("localisation")).expect("fixture directory");
    fs::write(
        vanilla.join("localisation/test_l_english.yml"),
        "l_english:\nenglish_key:0 \"English text\"\n",
    )
    .expect("English localisation");
    fs::write(
        vanilla.join("localisation/test_l_french.yml"),
        "l_french:\nfrench_key:0 \"Texte francais\"\n",
    )
    .expect("French localisation");
    fs::write(
        vanilla.join("localisation/unmarked.yml"),
        "l_english:\nplain_key:0 \"Unmarked file\"\n",
    )
    .expect("localisation without a language marker in its path");

    let mut builder = eu4_host();
    builder.apply_change(super::WorkspaceChange::SetSourceRoots(vec![
        SourceRoot::new(
            SourceRootId::new(0),
            SourceRootKind::Vanilla,
            fs::canonicalize(&vanilla).expect("canonical Vanilla root"),
        ),
    ]));
    builder.refresh_source_roots().expect("scan Vanilla");

    let install = |preferred: Vec<String>| {
        let mut host = eu4_host();
        host.set_preferred_localisation_languages(preferred);
        host.install_index_cache(IndexCache::from_snapshot(&builder.snapshot()).expect("cache"))
            .expect("install cache");
        host.snapshot()
    };

    let default_preferences = install(Vec::new());
    let preview_is_present = |snapshot: &AnalysisSnapshot, key: &str| {
        snapshot
            .index()
            .active_definition("localisation", key)
            .is_some_and(|definition| {
                snapshot
                    .localisation_preview(definition.file_id, definition.range)
                    .is_some()
            })
    };
    assert!(
        preview_is_present(&default_preferences, "english_key"),
        "English stays retained as the fallback language"
    );
    assert!(
        preview_is_present(&default_preferences, "plain_key"),
        "files without a path language marker stay retained"
    );
    assert!(
        !preview_is_present(&default_preferences, "french_key"),
        "unpreferred languages are dropped at install while their definitions remain indexed"
    );
    assert!(
        default_preferences
            .index()
            .active_definition("localisation", "french_key")
            .is_some(),
        "dropping a preview must not drop the indexed definition"
    );

    let french_preferences = install(vec!["french".to_owned()]);
    assert!(
        preview_is_present(&french_preferences, "french_key"),
        "configured preference order is retained"
    );
    assert!(
        preview_is_present(&french_preferences, "english_key"),
        "English fallback remains retained alongside a preference"
    );
    assert!(preview_is_present(&french_preferences, "plain_key"));

    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn dependency_index_cache_installs_into_a_configured_root_without_rescanning() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-engine-dependency-cache-{nonce}"));
    let dependency = root.join("dependency");
    fs::create_dir_all(dependency.join("common/scripted_effects")).expect("macro directory");
    fs::create_dir_all(dependency.join("events")).expect("event directory");
    fs::write(
        dependency.join("common/scripted_effects/dep_effects.txt"),
        "dep_cached_effect = { value = $amount$ }\n",
    )
    .expect("macro definitions");
    fs::write(
        dependency.join("events/dep_events.txt"),
        "country_event = { id = dep.1 immediate = { dep_cached_effect = { amount = 1 } } }\n",
    )
    .expect("dependency events");
    let dependency_path = fs::canonicalize(&dependency).expect("canonical dependency root");
    let dependency_root = SourceRoot::new(
        SourceRootId::new(42),
        SourceRootKind::Dependency,
        dependency_path.clone(),
    );

    // Build the cache from a dedicated dependency-only workspace.
    let rules = pdx_game::eu4::first_party_rules().expect("first-party rules");
    let mut builder = AnalysisHost::with_profile(rules, pdx_game::eu4::profile());
    builder.apply_change(WorkspaceChange::SetSourceRoots(vec![
        dependency_root.clone(),
    ]));
    builder.refresh_source_roots().expect("scan dependency");
    let cache = IndexCache::from_snapshot(&builder.snapshot()).expect("build cache");
    let cache_path = root.join("cache/dependency.pdxindex");
    cache.save(&cache_path).expect("save cache");

    // The cache restores the non-Vanilla root identity.
    let loaded = IndexCache::load(&cache_path).expect("load cache");
    assert_eq!(loaded.source_root().id, dependency_root.id);
    assert_eq!(loaded.source_root().kind, SourceRootKind::Dependency);

    // Install into a workspace where the dependency root is configured but not scanned.
    let mut host = AnalysisHost::with_profile(
        pdx_game::eu4::first_party_rules().unwrap(),
        pdx_game::eu4::profile(),
    );
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![
        dependency_root.clone(),
    ]));
    host.install_index_cache(loaded)
        .expect("install dependency cache");
    let snapshot = host.snapshot();
    let definition = snapshot
        .index()
        .active_definition("event", "dep.1")
        .expect("cached dependency definition is queryable");
    assert_eq!(
        snapshot
            .source_files()
            .get(&definition.file_id)
            .expect("dependency file metadata")
            .root_id,
        dependency_root.id
    );
    assert!(
        snapshot.file_state(definition.file_id).is_none(),
        "cached dependency files are never materialized"
    );
    assert!(snapshot.index().references_iter().any(|reference| {
        &*reference.kind == "scripted_effect" && &*reference.name == "dep_cached_effect"
    }));
    let macro_after_install = snapshot
        .index()
        .active_macro_definition("scripted_effect", "dep_cached_effect")
        .expect("cached dependency macro remains active");
    assert!(macro_after_install.template.is_some());
    let kinds = snapshot
        .index()
        .references_iter()
        .map(|reference| (&*reference.kind, &*reference.name))
        .collect::<Vec<_>>();
    assert!(
        kinds.contains(&("scripted_effect", "dep_cached_effect")),
        "scripted_effect references after install: {kinds:?}"
    );

    // A subsequent root refresh must skip the installed cache root.
    host.refresh_source_roots().expect("refresh workspace");
    assert!(
        host.snapshot()
            .index()
            .active_definition("event", "dep.1")
            .is_some()
    );
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn batch_dependency_cache_install_rebuilds_the_workspace_index_once() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-engine-batch-dependency-cache-{nonce}"));
    let rules = pdx_game::eu4::first_party_rules().expect("first-party rules");
    let mut caches = Vec::new();
    for (id, name) in [(1_u32, "first"), (2_u32, "second")] {
        let dependency = root.join(name);
        fs::create_dir_all(dependency.join("events")).expect("dependency events directory");
        fs::write(
            dependency.join("events/definition.txt"),
            format!("country_event = {{ id = {name}.1 }}\n"),
        )
        .expect("dependency definition");
        let dependency_path = fs::canonicalize(&dependency).expect("canonical dependency root");
        let dependency_root = SourceRoot::new(
            SourceRootId::new(id),
            SourceRootKind::Dependency,
            dependency_path,
        );
        let mut builder = AnalysisHost::with_profile(rules.clone(), pdx_game::eu4::profile());
        builder.apply_change(WorkspaceChange::SetSourceRoots(vec![dependency_root]));
        builder.refresh_source_roots().expect("scan dependency");
        caches.push(IndexCache::from_snapshot(&builder.snapshot()).expect("build cache"));
    }

    let current = root.join("current");
    fs::create_dir_all(current.join("events")).expect("current mod directory");
    fs::write(
        current.join("events/current.txt"),
        "country_event = { id = current.1 }\n",
    )
    .expect("current mod definition");
    let current_root = SourceRoot::new(
        SourceRootId::new(u32::MAX),
        SourceRootKind::CurrentMod,
        fs::canonicalize(&current).expect("canonical current mod root"),
    );
    let mut host = AnalysisHost::with_profile(rules, pdx_game::eu4::profile());
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![current_root]));
    host.refresh_source_roots().expect("scan current mod");
    let before_install = host.snapshot().revision();

    host.install_index_caches(caches)
        .expect("install dependency caches as one batch");
    let snapshot = host.snapshot();
    assert_eq!(snapshot.revision(), before_install + 1);
    assert!(
        snapshot
            .index()
            .active_definition("event", "first.1")
            .is_some()
    );
    assert!(
        snapshot
            .index()
            .active_definition("event", "second.1")
            .is_some()
    );
    assert_eq!(
        snapshot
            .source_roots()
            .iter()
            .filter(|root| root.kind == SourceRootKind::Dependency)
            .count(),
        2
    );
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn dependency_index_cache_rejects_an_unrelated_configured_root() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-engine-dependency-mismatch-{nonce}"));
    let dependency = root.join("dependency");
    fs::create_dir_all(dependency.join("events")).expect("event directory");
    fs::write(
        dependency.join("events/mismatch.txt"),
        "country_event = { id = mismatch.1 }\n",
    )
    .expect("dependency events");
    let dependency_path = fs::canonicalize(&dependency).expect("canonical dependency root");

    let rules = pdx_game::eu4::first_party_rules().expect("first-party rules");
    let mut builder = AnalysisHost::with_profile(rules, pdx_game::eu4::profile());
    builder.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(7),
        SourceRootKind::Dependency,
        dependency_path.clone(),
    )]));
    builder.refresh_source_roots().expect("scan dependency");
    let cache = IndexCache::from_snapshot(&builder.snapshot()).expect("build cache");
    let cache_path = root.join("cache/dependency.pdxindex");
    cache.save(&cache_path).expect("save cache");
    let loaded = IndexCache::load(&cache_path).expect("load cache");

    // The configured root claims the same id but a different directory.
    let other = root.join("other");
    fs::create_dir_all(&other).expect("other directory");
    let mut host = AnalysisHost::with_profile(
        pdx_game::eu4::first_party_rules().unwrap(),
        pdx_game::eu4::profile(),
    );
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(7),
        SourceRootKind::Dependency,
        fs::canonicalize(&other).expect("canonical other root"),
    )]));
    assert!(matches!(
        host.install_index_cache(loaded),
        Err(IndexCacheError::InvalidData(_))
    ));
    fs::remove_dir_all(root).expect("cleanup");
}
