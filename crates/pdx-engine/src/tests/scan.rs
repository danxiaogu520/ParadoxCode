use super::*;

#[test]
fn persistent_parse_cache_skips_reparsing_matching_disk_source() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-engine-parse-cache-{nonce}"));
    let events = root.join("events");
    let cache = root.join("cache");
    fs::create_dir_all(&events).expect("event directory");
    fs::write(
        events.join("cached.txt"),
        "country_event = { id = cached.1 }\n",
    )
    .expect("cached fixture");

    let configure = |host: &mut AnalysisHost| {
        host.apply_change(super::WorkspaceChange::SetSourceRoots(vec![
            SourceRoot::new(
                SourceRootId::new(1),
                SourceRootKind::CurrentMod,
                root.clone(),
            ),
        ]));
    };
    let mut first = eu4_host().with_parse_cache_dir(cache.clone());
    configure(&mut first);
    reset_pipeline_counts();
    first.refresh_source_roots().expect("initial scan");
    assert_eq!(pipeline_counts(), (1, 1));

    let mut second = eu4_host().with_parse_cache_dir(cache);
    configure(&mut second);
    reset_pipeline_counts();
    second.refresh_source_roots().expect("cached scan");
    assert_eq!(
        pipeline_counts(),
        (0, 1),
        "a matching persistent CST should skip parsing but still lower against active rules"
    );
    assert!(
        second
            .snapshot()
            .index()
            .active_definition("event", "cached.1")
            .is_some()
    );

    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn physical_path_lookup_follows_scan_and_targeted_disk_changes() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-engine-path-index-{nonce}"));
    let events = root.join("events");
    fs::create_dir_all(&events).expect("event directory");
    fs::write(events.join("kept.txt"), "country_event = { id = kept.1 }\n").expect("kept file");
    fs::write(events.join("gone.txt"), "country_event = { id = gone.1 }\n").expect("gone file");

    let mut host = eu4_host();
    host.apply_change(super::WorkspaceChange::SetSourceRoots(vec![
        SourceRoot::new(
            SourceRootId::new(1),
            SourceRootKind::CurrentMod,
            root.clone(),
        ),
    ]));
    host.refresh_source_roots().expect("scan");
    let snapshot = host.snapshot();
    assert!(
        snapshot
            .source_file_id_for_path(&events.join("kept.txt"))
            .is_some(),
        "scanned file is resolvable by physical path"
    );
    let gone_id = snapshot
        .source_file_id_for_path(&events.join("gone.txt"))
        .expect("scanned file is indexed");

    fs::remove_file(events.join("gone.txt")).expect("remove file");
    host.apply_disk_file_changes(&[DiskFileChange::new(
        events.join("gone.txt"),
        DiskFileChangeKind::Deleted,
    )])
    .expect("apply deletion");
    let snapshot = host.snapshot();
    assert_eq!(
        snapshot.source_file_id_for_path(&events.join("gone.txt")),
        None,
        "deleted file leaves the path index"
    );
    assert!(
        snapshot.source_files().get(&gone_id).is_none(),
        "deleted file leaves the file table"
    );

    fs::write(events.join("new.txt"), "country_event = { id = new.1 }\n").expect("new file");
    host.apply_disk_file_changes(&[DiskFileChange::new(
        events.join("new.txt"),
        DiskFileChangeKind::Created,
    )])
    .expect("apply creation");
    let snapshot = host.snapshot();
    assert!(
        snapshot
            .source_file_id_for_path(&events.join("new.txt"))
            .is_some(),
        "created file enters the path index"
    );
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn recoverable_file_failures_do_not_abort_the_workspace_scan() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-engine-isolation-{nonce}"));
    let events = root.join("events");
    fs::create_dir_all(&events).expect("event directory");
    fs::write(events.join("good.txt"), "country_event = { id = safe.1 }\n").expect("valid event");
    fs::write(events.join("invalid.txt"), [0xff, 0xfe]).expect("invalid UTF-8 event");
    fs::write(
        events.join("undefined-windows1252.txt"),
        b"country_event = { id = invalid.1 }\n# \x81\n",
    )
    .expect("invalid Windows-1252 event");
    fs::write(events.join("large.txt"), vec![b'x'; 65]).expect("oversized event");

    let mut host = eu4_host();
    host.apply_change(super::WorkspaceChange::SetSourceRoots(vec![
        SourceRoot::new(
            SourceRootId::new(1),
            SourceRootKind::CurrentMod,
            root.clone(),
        ),
    ]));
    let report = host
        .refresh_source_roots_with_limits(WorkspaceScanLimits {
            max_file_size: 64,
            ..WorkspaceScanLimits::default()
        })
        .expect("recoverable file failures should not abort scanning");

    assert_eq!(report.discovered_files, 4);
    assert_eq!(report.indexed_files, 2);
    assert_eq!(report.skipped_entries, 2);
    assert!(
        report
            .issues
            .iter()
            .any(|issue| issue.kind == WorkspaceScanIssueKind::InvalidUtf8)
    );
    assert!(
        report
            .issues
            .iter()
            .any(|issue| issue.kind == WorkspaceScanIssueKind::EncodingRecovered)
    );
    assert!(
        report
            .issues
            .iter()
            .any(|issue| issue.kind == WorkspaceScanIssueKind::FileTooLarge)
    );
    assert_eq!(host.snapshot().scan_report(), &report);
    assert_eq!(host.snapshot().source_files().len(), 2);
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn eu4_legacy_windows1252_text_is_decoded_before_indexing() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-engine-windows1252-{nonce}"));
    let events = root.join("events");
    fs::create_dir_all(&events).expect("event directory");
    fs::write(
        events.join("legacy.txt"),
        b"country_event = { id = legacy.1 }\n# caf\xe9\n",
    )
    .expect("legacy event");

    let mut host = eu4_host();
    host.apply_change(super::WorkspaceChange::SetSourceRoots(vec![
        SourceRoot::new(
            SourceRootId::new(1),
            SourceRootKind::CurrentMod,
            root.clone(),
        ),
    ]));
    let report = host.refresh_source_roots().expect("legacy scan");

    assert_eq!(report.indexed_files, 1);
    assert_eq!(report.legacy_encoded_files, 1);
    assert_eq!(report.skipped_entries, 0);
    let file_id = host
        .snapshot()
        .source_files()
        .values()
        .find(|file| file.logical_path.as_str() == "events/legacy.txt")
        .expect("legacy source file")
        .id;
    assert_eq!(
        host.snapshot().source_text(file_id),
        Some("country_event = { id = legacy.1 }\n# café\n")
    );
    assert_eq!(
        host.snapshot()
            .index()
            .definitions("event", "legacy.1")
            .len(),
        1
    );
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn game_encoded_text_with_control_characters_keeps_surrounding_definitions() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-engine-non-text-{nonce}"));
    let events = root.join("events");
    fs::create_dir_all(&events).expect("event directory");
    fs::write(
        events.join("encoded.txt"),
        b"country_event = { id = encoded.1 }\n# \x0c\x02garbage\n",
    )
    .expect("game-encoded event");

    let mut host = eu4_host();
    host.apply_change(super::WorkspaceChange::SetSourceRoots(vec![
        SourceRoot::new(
            SourceRootId::new(1),
            SourceRootKind::CurrentMod,
            root.clone(),
        ),
    ]));
    let report = host.refresh_source_roots().expect("game-encoded scan");

    assert_eq!(report.indexed_files, 1);
    assert_eq!(report.legacy_encoded_files, 0);
    assert_eq!(report.skipped_entries, 0);
    assert!(
        report.issues.iter().any(|issue| {
            issue.kind == super::WorkspaceScanIssueKind::EncodingRecovered
                && issue.path.ends_with("events/encoded.txt")
        }),
        "expected an EncodingRecovered issue: {:?}",
        report.issues
    );
    assert_eq!(host.snapshot().source_files().len(), 1);
    assert!(
        host.snapshot()
            .index()
            .definitions("event", "encoded.1")
            .len()
            == 1
    );
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn malformed_quoted_value_does_not_discard_the_parent_or_sibling() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-engine-encoded-block-{nonce}"));
    let events = root.join("events");
    fs::create_dir_all(&events).expect("event directory");
    fs::write(
        events.join("encoded.txt"),
        b"country_event = { id = parent.1 title = \"bad\x01value\" option = { name = ok } }\ncountry_event = { id = sibling.1 }\n",
    )
    .expect("encoded events");

    let mut host = eu4_host();
    host.apply_change(super::WorkspaceChange::SetSourceRoots(vec![
        SourceRoot::new(
            SourceRootId::new(1),
            SourceRootKind::CurrentMod,
            root.clone(),
        ),
    ]));
    let report = host.refresh_source_roots().expect("encoded scan");

    assert_eq!(report.indexed_files, 1);
    assert_eq!(report.skipped_entries, 0);
    let snapshot = host.snapshot();
    let index = snapshot.index();
    assert_eq!(index.definitions("event", "parent.1").len(), 1);
    assert_eq!(index.definitions("event", "sibling.1").len(), 1);
    let source = snapshot
        .source_files()
        .values()
        .next()
        .expect("encoded source");
    let text = snapshot.source_text(source.id).expect("source text");
    assert!(!text.contains('\u{1}'));
    assert!(text.contains("title = \""));
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn depth_limit_skips_nested_subtrees_with_a_reported_issue() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-engine-depth-{nonce}"));
    let events = root.join("events");
    fs::create_dir_all(&events).expect("event directory");
    fs::write(events.join("deep.txt"), "country_event = { id = deep.1 }\n").expect("deep event");

    let mut host = eu4_host();
    host.apply_change(super::WorkspaceChange::SetSourceRoots(vec![
        SourceRoot::new(
            SourceRootId::new(1),
            SourceRootKind::CurrentMod,
            root.clone(),
        ),
    ]));
    let report = host
        .refresh_source_roots_with_limits(WorkspaceScanLimits {
            max_depth: 0,
            ..WorkspaceScanLimits::default()
        })
        .expect("depth-limited scan");

    assert_eq!(report.indexed_files, 0);
    assert!(
        report
            .issues
            .iter()
            .any(|issue| issue.kind == WorkspaceScanIssueKind::DepthLimitExceeded)
    );
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn file_limit_failure_preserves_the_previous_snapshot() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-engine-file-limit-{nonce}"));
    let events = root.join("events");
    fs::create_dir_all(&events).expect("event directory");
    fs::write(events.join("a.txt"), "country_event = { id = limit.a }\n").expect("a event");

    let mut host = eu4_host();
    host.apply_change(super::WorkspaceChange::SetSourceRoots(vec![
        SourceRoot::new(
            SourceRootId::new(1),
            SourceRootKind::CurrentMod,
            root.clone(),
        ),
    ]));
    host.refresh_source_roots().expect("initial scan");
    let before = host.snapshot();
    fs::write(events.join("b.txt"), "country_event = { id = limit.b }\n").expect("b event");

    let error = host
        .refresh_source_roots_with_limits(WorkspaceScanLimits {
            max_files: 1,
            ..WorkspaceScanLimits::default()
        })
        .expect_err("the total file limit must be enforced");
    assert!(matches!(
        error,
        super::WorkspaceError::FileLimitExceeded { limit: 1 }
    ));
    let after = host.snapshot();
    assert_eq!(after.revision(), before.revision());
    assert_eq!(after.source_files(), before.source_files());
    assert_eq!(after.scan_report(), before.scan_report());
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn cancelled_scan_preserves_the_previous_snapshot_atomically() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-engine-cancel-scan-{nonce}"));
    let events = root.join("events");
    fs::create_dir_all(&events).expect("event directory");
    fs::write(
        events.join("baseline.txt"),
        "country_event = { id = baseline.1 }\n",
    )
    .expect("baseline event");

    let mut host = eu4_host();
    host.apply_change(super::WorkspaceChange::SetSourceRoots(vec![
        SourceRoot::new(
            SourceRootId::new(1),
            SourceRootKind::CurrentMod,
            root.clone(),
        ),
    ]));
    host.refresh_source_roots().expect("initial scan");
    let before = host.snapshot();
    for index in 0..32 {
        fs::write(
            events.join(format!("new-{index:02}.txt")),
            format!("country_event = {{ id = cancelled.{index} }}\n"),
        )
        .expect("new event");
    }

    let cancellation = WorkspaceScanToken::cancel_after(5);
    let error = host
        .refresh_source_roots_cancellable(&cancellation)
        .expect_err("scan should stop at an internal checkpoint");
    assert!(matches!(error, WorkspaceError::Cancelled));
    assert!(cancellation.is_cancelled());

    let after = host.snapshot();
    assert_eq!(after.revision(), before.revision());
    assert!(Arc::ptr_eq(&after.source_files, &before.source_files));
    assert!(Arc::ptr_eq(&after.file_states, &before.file_states));
    assert!(Arc::ptr_eq(&after.index, &before.index));
    assert!(Arc::ptr_eq(&after.scan_report, &before.scan_report));
    assert!(after.index().definitions("event", "cancelled.0").is_empty());
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn opaque_binary_assets_are_indexed_without_reading_them_as_utf8() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-engine-opaque-asset-{nonce}"));
    fs::create_dir_all(root.join("gfx")).expect("asset directory");
    fs::write(root.join("gfx/icon.png"), [0_u8, 159, 146, 150]).expect("binary asset");

    let mut profile = pdx_game::eu4::profile();
    profile.scan_extensions.clear();
    let mut host = AnalysisHost::with_profile(pdx_game::eu4::bootstrap_rules(), profile);
    host.apply_change(super::WorkspaceChange::SetSourceRoots(vec![
        SourceRoot::new(
            SourceRootId::new(1),
            SourceRootKind::CurrentMod,
            root.clone(),
        ),
    ]));
    let report = host.refresh_source_roots().expect("scan asset");

    assert_eq!(report.discovered_files, 1);
    assert_eq!(report.indexed_files, 1);
    assert!(
        !report
            .issues
            .iter()
            .any(|issue| issue.kind == WorkspaceScanIssueKind::InvalidUtf8)
    );
    let snapshot = host.snapshot();
    let file = snapshot
        .source_files()
        .values()
        .next()
        .expect("asset source file");
    assert_eq!(file.logical_path.as_str(), "gfx/icon.png");
    assert!(snapshot.index().shard(file.id).is_some());
    assert!(
        snapshot.file_state(file.id).is_some_and(|state| {
            state.parsed().is_none() && state.shard().definitions.is_empty()
        })
    );

    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn eu4_scan_uses_the_explicit_script_folder_whitelist() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-engine-whitelist-{nonce}"));
    fs::create_dir_all(root.join("events")).expect("events directory");
    fs::create_dir_all(root.join("events/nested")).expect("nested events directory");
    fs::create_dir_all(root.join("common/countries")).expect("common directory");
    fs::create_dir_all(root.join("common/countries/nested")).expect("nested common directory");
    fs::create_dir_all(root.join("gfx")).expect("gfx directory");
    fs::create_dir_all(root.join("gfx/sprite_packs")).expect("nested gfx directory");
    fs::create_dir_all(root.join("map/lakes")).expect("map lakes directory");
    fs::create_dir_all(root.join("map/random/tiles")).expect("map generated directory");
    fs::create_dir_all(root.join("ignored")).expect("ignored directory");
    fs::write(
        root.join("events/allowed.txt"),
        "country_event = { id = whitelist.event }\n",
    )
    .expect("event fixture");
    fs::write(
        root.join("events/nested/ignored.txt"),
        "country_event = { id = whitelist.events_nested }\n",
    )
    .expect("nested event fixture");
    fs::write(
        root.join("common/technology.txt"),
        "country_event = { id = whitelist.technology }\n",
    )
    .expect("bare common fixture");
    fs::write(
        root.join("common/unknown.txt"),
        "country_event = { id = whitelist.unknown_bare }\n",
    )
    .expect("unknown bare common fixture");
    fs::write(
        root.join("common/countries/allowed.txt"),
        "country_event = { id = whitelist.common }\n",
    )
    .expect("common fixture");
    fs::write(
        root.join("common/countries/nested/ignored.txt"),
        "country_event = { id = whitelist.common_nested }\n",
    )
    .expect("nested common fixture");
    fs::write(
        root.join("gfx/icon.gfx"),
        "spriteType = { name = whitelist.gfx }\n",
    )
    .expect("gfx fixture");
    fs::write(
        root.join("gfx/sprite_packs/allowed.txt"),
        "country_event = { id = whitelist.gfx_nested }\n",
    )
    .expect("nested gfx fixture");
    fs::write(
        root.join("map/area.txt"),
        "country_event = { id = whitelist.map_area }\n",
    )
    .expect("map fixture");
    fs::write(
        root.join("map/lakes/00_lakes.txt"),
        "country_event = { id = whitelist.map_lakes }\n",
    )
    .expect("nested map fixture");
    fs::write(
        root.join("map/random/tiles/tile0.txt"),
        "country_event = { id = whitelist.map_generated }\n",
    )
    .expect("generated map fixture");
    fs::write(
        root.join("map/unknown.txt"),
        "country_event = { id = whitelist.map_unknown }\n",
    )
    .expect("unknown map fixture");
    fs::write(root.join("gfx/icon.png"), [0_u8, 159, 146, 150]).expect("asset fixture");
    fs::write(
        root.join("ignored/not_scanned.txt"),
        "country_event = { id = whitelist.ignored }\n",
    )
    .expect("ignored fixture");
    fs::write(
        root.join("root_level.txt"),
        "country_event = { id = whitelist.root }\n",
    )
    .expect("root-level fixture");

    let mut host = eu4_host();
    host.apply_change(super::WorkspaceChange::SetSourceRoots(vec![
        SourceRoot::new(
            SourceRootId::new(1),
            SourceRootKind::CurrentMod,
            root.clone(),
        ),
    ]));
    let report = host.refresh_source_roots().expect("whitelist scan");
    assert_eq!(report.discovered_files, 10);
    assert_eq!(report.indexed_files, 7);
    let snapshot = host.snapshot();
    assert!(
        snapshot
            .index()
            .active_definition("event", "whitelist.event")
            .is_some()
    );
    assert!(
        snapshot
            .index()
            .active_definition("event", "whitelist.common")
            .is_some()
    );
    assert!(
        snapshot
            .index()
            .active_definition("event", "whitelist.technology")
            .is_some()
    );
    assert!(
        snapshot
            .index()
            .active_definition("event", "whitelist.gfx_nested")
            .is_some()
    );
    assert!(
        snapshot
            .index()
            .active_definition("event", "whitelist.map_area")
            .is_some()
    );
    assert!(
        snapshot
            .index()
            .active_definition("event", "whitelist.map_lakes")
            .is_some()
    );
    assert!(
        snapshot
            .index()
            .active_definition("event", "whitelist.unknown_bare")
            .is_none()
    );
    assert!(
        snapshot
            .index()
            .active_definition("event", "whitelist.common_nested")
            .is_none()
    );
    assert!(
        snapshot
            .index()
            .active_definition("event", "whitelist.events_nested")
            .is_none()
    );
    assert!(
        snapshot
            .index()
            .active_definition("event", "whitelist.map_generated")
            .is_none()
    );
    assert!(
        snapshot
            .index()
            .active_definition("event", "whitelist.map_unknown")
            .is_none()
    );
    assert!(
        snapshot
            .index()
            .active_definition("event", "whitelist.ignored")
            .is_none()
    );
    assert!(
        snapshot
            .index()
            .active_definition("event", "whitelist.root")
            .is_none()
    );
    assert!(
        snapshot
            .source_files()
            .values()
            .any(|file| file.logical_path.as_str() == "gfx/icon.gfx")
    );
    assert!(
        snapshot
            .source_files()
            .values()
            .all(|file| file.logical_path.as_str() != "gfx/icon.png")
    );
    let ignored_change = root.join("ignored/created_after_scan.txt");
    fs::write(
        &ignored_change,
        "country_event = { id = whitelist.watched_ignored }\n",
    )
    .expect("ignored watched fixture");
    host.apply_disk_file_changes(&[DiskFileChange::new(
        ignored_change,
        DiskFileChangeKind::Created,
    )])
    .expect("ignored watched change");
    assert!(
        host.snapshot()
            .index()
            .active_definition("event", "whitelist.watched_ignored")
            .is_none()
    );
    let ignored_extension_change = root.join("events/created_after_scan.png");
    fs::write(&ignored_extension_change, [0_u8, 159, 146, 150]).expect("ignored extension fixture");
    host.apply_disk_file_changes(&[DiskFileChange::new(
        ignored_extension_change,
        DiskFileChangeKind::Created,
    )])
    .expect("ignored extension change");
    assert!(
        host.snapshot()
            .source_files()
            .values()
            .all(|file| { file.logical_path.as_str() != "events/created_after_scan.png" })
    );
    fs::remove_dir_all(root).expect("cleanup");
}

#[cfg(unix)]
#[test]
fn directory_symlinks_are_reported_and_never_followed() {
    use std::os::unix::fs::symlink;

    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-engine-symlink-root-{nonce}"));
    let outside = std::env::temp_dir().join(format!("pdx-engine-symlink-outside-{nonce}"));
    fs::create_dir_all(&root).expect("source root");
    fs::create_dir_all(&outside).expect("outside directory");
    fs::write(
        outside.join("leak.txt"),
        "country_event = { id = leak.1 }\n",
    )
    .expect("outside event");
    symlink(&outside, root.join("events")).expect("directory symlink");

    let mut host = eu4_host();
    host.apply_change(super::WorkspaceChange::SetSourceRoots(vec![
        SourceRoot::new(
            SourceRootId::new(1),
            SourceRootKind::CurrentMod,
            root.clone(),
        ),
    ]));
    let report = host.refresh_source_roots().expect("symlink-safe scan");

    assert_eq!(report.discovered_files, 0);
    assert_eq!(report.indexed_files, 0);
    assert!(
        report
            .issues
            .iter()
            .any(|issue| issue.kind == WorkspaceScanIssueKind::SymlinkSkipped)
    );
    fs::remove_dir_all(root).expect("cleanup root");
    fs::remove_dir_all(outside).expect("cleanup outside directory");
}

#[test]
fn workspace_scan_skips_tool_generated_directories() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-engine-ignored-tools-{nonce}"));
    let events = root.join("events");
    let generated_events = root.join("target/debug/events");
    fs::create_dir_all(&events).expect("events directory");
    fs::create_dir_all(&generated_events).expect("generated events directory");
    fs::write(
        events.join("indexed.txt"),
        "country_event = { id = indexed.1 }\n",
    )
    .expect("indexed fixture");
    fs::write(
        generated_events.join("ignored.txt"),
        "country_event = { id = ignored.1 }\n",
    )
    .expect("ignored fixture");

    let mut host = eu4_host();
    host.apply_change(super::WorkspaceChange::SetSourceRoots(vec![
        SourceRoot::new(
            SourceRootId::new(1),
            SourceRootKind::CurrentMod,
            root.clone(),
        ),
    ]));
    let report = host.refresh_source_roots().expect("bounded workspace scan");

    assert_eq!(report.discovered_files, 1);
    assert_eq!(report.indexed_files, 1);
    assert!(
        host.snapshot()
            .index()
            .definitions("event", "indexed.1")
            .len()
            == 1
    );
    assert!(
        host.snapshot()
            .index()
            .definitions("event", "ignored.1")
            .is_empty()
    );
    fs::remove_dir_all(root).expect("cleanup");
}
