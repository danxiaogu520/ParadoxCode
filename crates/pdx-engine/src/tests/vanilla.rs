use super::*;

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
        reference.kind == "scripted_effect" && reference.name == "cached_effect"
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

    let cache = VanillaIndexCache::from_snapshot(&snapshot).expect("build cache");
    let cache_path = root.join("cache/vanilla.pdxindex");
    cache.save(&cache_path).expect("save cache");
    let loaded = VanillaIndexCache::load(&cache_path).expect("load cache");
    assert!(loaded.index().references_iter().any(|reference| {
        reference.kind == "scripted_effect" && reference.name == "cached_effect"
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
        VanillaIndexCache::load(&cache_path),
        Err(VanillaCacheError::InvalidData(_))
    ));
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
    let cache = VanillaIndexCache::from_snapshot(&host.snapshot()).expect("build cache");
    let cache_path = root.join("cache/vanilla.pdxindex");
    cache.save(&cache_path).expect("save cache");
    assert!(
        VanillaIndexCache::load(&cache_path).is_ok(),
        "valid cache loads"
    );

    let connection = rusqlite::Connection::open(&cache_path).expect("open cache for corruption");
    connection
        .execute(
            "UPDATE navigation_positions SET range_start = range_start + 1",
            [],
        )
        .expect("corrupt navigation range");
    drop(connection);
    assert!(matches!(
        VanillaIndexCache::load(&cache_path),
        Err(VanillaCacheError::InvalidData(_))
    ));
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
    fs::create_dir_all(vanilla.join("common/events")).expect("Vanilla fixture directory");
    fs::create_dir_all(vanilla.join("localisation/nested/deeper"))
        .expect("Vanilla localisation fixture directory");
    fs::create_dir_all(current.join("common/events")).expect("current fixture directory");
    fs::write(
        vanilla.join("common/events/definitions.txt"),
        "country_event = { id = shared.1 }\ncountry_event = { id = vanilla.1 }\n",
    )
    .expect("Vanilla definitions");
    fs::write(
        vanilla.join("localisation/nested/deeper/test_l_english.yml"),
        "l_english:\nvanilla_name:0 \"Vanilla text\"\n",
    )
    .expect("Vanilla localisation");
    fs::write(
        current.join("common/events/definitions.txt"),
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
    let cache = VanillaIndexCache::from_snapshot(&vanilla_host.snapshot()).expect("build cache");
    let cache_path = root.join("cache/vanilla.pdxindex");
    cache.save(&cache_path).expect("save cache");
    let cancelled = WorkspaceScanToken::new();
    cancelled.cancel();
    assert!(matches!(
        VanillaIndexCache::load_cancellable(&cache_path, &cancelled),
        Err(VanillaCacheError::Cancelled)
    ));
    let loaded = VanillaIndexCache::load(&cache_path).expect("load cache");
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
        Err(VanillaCacheError::NotVanillaCache)
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
    host.install_vanilla_cache(loaded)
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
        .vanilla_localisation_preview(vanilla_localisation.file_id, vanilla_localisation.range)
        .expect("cached Vanilla localisation preview");
    assert_eq!(preview.language.as_deref(), Some("l_english"));
    assert_eq!(preview.value, "Vanilla text");
    assert!(snapshot.file_state(vanilla_localisation.file_id).is_none());
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
    let cache = VanillaIndexCache::from_snapshot(&builder.snapshot()).expect("build cache");
    let cache_path = root.join("cache/dependency.pdxindex");
    cache.save(&cache_path).expect("save cache");

    // The cache restores the non-Vanilla root identity.
    let loaded = VanillaIndexCache::load(&cache_path).expect("load cache");
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
        reference.kind == "scripted_effect" && reference.name == "dep_cached_effect"
    }));
    let macro_after_install = snapshot
        .index()
        .active_macro_definition("scripted_effect", "dep_cached_effect")
        .expect("cached dependency macro remains active");
    assert!(macro_after_install.template.is_some());
    let kinds = snapshot
        .index()
        .references_iter()
        .map(|reference| (reference.kind.as_str(), reference.name.as_str()))
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
    let cache = VanillaIndexCache::from_snapshot(&builder.snapshot()).expect("build cache");
    let cache_path = root.join("cache/dependency.pdxindex");
    cache.save(&cache_path).expect("save cache");
    let loaded = VanillaIndexCache::load(&cache_path).expect("load cache");

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
        Err(VanillaCacheError::InvalidData(_))
    ));
    fs::remove_dir_all(root).expect("cleanup");
}
