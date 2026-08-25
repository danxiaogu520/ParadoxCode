use super::*;

#[test]
fn targeted_disk_changes_replace_one_shard_without_overwriting_an_overlay() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-engine-targeted-disk-{nonce}"));
    let events = root.join("events");
    fs::create_dir_all(&events).expect("fixture directory");
    let changed_path = events.join("changed.txt");
    let untouched_path = events.join("untouched.txt");
    fs::write(&changed_path, "country_event = { id = old.1 }\n").expect("changed fixture");
    fs::write(&untouched_path, "country_event = { id = untouched.1 }\n")
        .expect("untouched fixture");

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
    let changed_id = before
        .source_files()
        .values()
        .find(|file| file.physical_path == changed_path)
        .expect("changed source")
        .id;
    let untouched_id = before
        .source_files()
        .values()
        .find(|file| file.physical_path == untouched_path)
        .expect("untouched source")
        .id;
    let untouched_state = Arc::clone(before.file_states.get(&untouched_id).expect("state"));
    let document = DocumentId::new("file:///targeted/changed.txt");
    host.open_document(
        document.clone(),
        1,
        "country_event = { id = overlay.1 }\n".to_owned(),
        Some(changed_path.clone()),
    )
    .expect("open overlay");

    fs::write(&changed_path, "country_event = { id = new.1 }\n").expect("disk edit");
    reset_pipeline_counts();
    host.apply_disk_file_changes(&[DiskFileChange::new(
        changed_path.clone(),
        DiskFileChangeKind::Changed,
    )])
    .expect("targeted change");
    assert_eq!(pipeline_counts(), (1, 1));
    let changed = host.snapshot();
    assert!(
        changed
            .index()
            .active_definition("event", "old.1")
            .is_none()
    );
    assert!(
        changed
            .index()
            .active_definition("event", "new.1")
            .is_some()
    );
    assert_eq!(
        changed.document(&document).expect("overlay remains").text(),
        "country_event = { id = overlay.1 }\n"
    );
    assert!(Arc::ptr_eq(
        changed
            .file_states
            .get(&untouched_id)
            .expect("untouched current state"),
        &untouched_state
    ));
    assert!(!Arc::ptr_eq(
        changed
            .file_states
            .get(&changed_id)
            .expect("changed current state"),
        before
            .file_states
            .get(&changed_id)
            .expect("changed old state")
    ));

    let created_path = events.join("created.txt");
    fs::write(&created_path, "country_event = { id = created.1 }\n").expect("created fixture");
    host.apply_disk_file_changes(&[DiskFileChange::new(
        created_path,
        DiskFileChangeKind::Created,
    )])
    .expect("targeted create");
    assert!(
        host.snapshot()
            .index()
            .active_definition("event", "created.1")
            .is_some()
    );

    fs::remove_file(&changed_path).expect("delete changed fixture");
    host.apply_disk_file_changes(&[DiskFileChange::new(
        changed_path,
        DiskFileChangeKind::Deleted,
    )])
    .expect("targeted delete");
    let deleted = host.snapshot();
    assert!(deleted.source_files().get(&changed_id).is_none());
    assert!(
        deleted
            .index()
            .active_definition("event", "new.1")
            .is_none()
    );
    assert_eq!(
        deleted
            .document(&document)
            .expect("overlay survives backing deletion")
            .text(),
        "country_event = { id = overlay.1 }\n"
    );
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn targeted_disk_changes_reindex_a_localisation_shard() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-engine-targeted-localisation-{nonce}"));
    let localisation = root.join("localisation/nested");
    fs::create_dir_all(&localisation).expect("localisation fixture directory");
    let changed_path = localisation.join("test_l_english.yml");
    fs::write(&changed_path, "l_english:\nold_name:0 \"Old\"\n").expect("localisation fixture");

    let mut host = eu4_host();
    host.apply_change(super::WorkspaceChange::SetSourceRoots(vec![
        SourceRoot::new(
            SourceRootId::new(1),
            SourceRootKind::CurrentMod,
            root.clone(),
        ),
    ]));
    host.refresh_source_roots()
        .expect("initial localisation scan");
    assert!(
        host.snapshot()
            .index()
            .active_definition("localisation", "old_name")
            .is_some()
    );

    fs::write(&changed_path, "l_english:\nnew_name:0 \"New\"\n").expect("localisation edit");
    reset_pipeline_counts();
    host.apply_disk_file_changes(&[DiskFileChange::new(
        changed_path,
        DiskFileChangeKind::Changed,
    )])
    .expect("targeted localisation change");
    assert_eq!(pipeline_counts(), (1, 1));
    assert!(
        host.snapshot()
            .index()
            .active_definition("localisation", "old_name")
            .is_none()
    );
    assert!(
        host.snapshot()
            .index()
            .active_definition("localisation", "new_name")
            .is_some()
    );

    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn source_file_ids_do_not_shift_when_an_earlier_path_is_added() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-engine-stable-ids-{nonce}"));
    let events = root.join("events");
    fs::create_dir_all(&events).expect("event directory");
    fs::write(events.join("b.txt"), "country_event = { id = stable.b }\n").expect("b event");
    fs::write(events.join("c.txt"), "country_event = { id = stable.c }\n").expect("c event");

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
    let b_before = before
        .source_files()
        .values()
        .find(|file| file.logical_path.as_str() == "events/b.txt")
        .expect("b source file")
        .id;
    let c_before = before
        .source_files()
        .values()
        .find(|file| file.logical_path.as_str() == "events/c.txt")
        .expect("c source file")
        .id;

    fs::write(events.join("a.txt"), "country_event = { id = stable.a }\n").expect("a event");
    host.refresh_source_roots().expect("second scan");
    let after = host.snapshot();
    let b_after = after
        .source_files()
        .values()
        .find(|file| file.logical_path.as_str() == "events/b.txt")
        .expect("b source file after insertion")
        .id;
    let c_after = after
        .source_files()
        .values()
        .find(|file| file.logical_path.as_str() == "events/c.txt")
        .expect("c source file after insertion")
        .id;

    assert_eq!(b_before, b_after);
    assert_eq!(c_before, c_after);
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn unchanged_file_states_are_reused_and_only_changed_files_advance() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-engine-file-state-{nonce}"));
    let events = root.join("events");
    fs::create_dir_all(&events).expect("event directory");
    fs::write(events.join("a.txt"), "country_event = { id = state.a }\n").expect("a event");
    fs::write(events.join("b.txt"), "country_event = { id = state.b }\n").expect("b event");

    let mut host = eu4_host();
    host.apply_change(super::WorkspaceChange::SetSourceRoots(vec![
        SourceRoot::new(
            SourceRootId::new(1),
            SourceRootKind::CurrentMod,
            root.clone(),
        ),
    ]));
    host.refresh_source_roots().expect("initial scan");
    let first = host.snapshot();
    let a = first
        .source_files()
        .values()
        .find(|file| file.logical_path.as_str() == "events/a.txt")
        .expect("a file")
        .id;
    let b = first
        .source_files()
        .values()
        .find(|file| file.logical_path.as_str() == "events/b.txt")
        .expect("b file")
        .id;
    assert!(first.file_state(a).expect("a state").parsed().is_some());
    assert!(first.file_state(a).expect("a state").hir().is_some());
    let Some(ParsedSource::Text(parsed)) = first.file_state(a).expect("a state").parsed() else {
        panic!("event file should retain a text parse");
    };
    assert!(std::ptr::eq(
        parsed.as_ref(),
        first
            .file_state(a)
            .expect("a state")
            .hir()
            .expect("a HIR")
            .syntax()
    ));

    host.refresh_source_roots().expect("unchanged scan");
    let second = host.snapshot();
    assert!(Arc::ptr_eq(
        first.file_states.get(&a).expect("first a state"),
        second.file_states.get(&a).expect("second a state")
    ));
    assert!(Arc::ptr_eq(
        first.file_states.get(&b).expect("first b state"),
        second.file_states.get(&b).expect("second b state")
    ));
    let old_range = second
        .index()
        .active_definition("event", "state.b")
        .expect("old b definition")
        .range;
    assert!(second.index().position_for(b, old_range).is_some());

    fs::write(
        events.join("b.txt"),
        "country_event = { id = state.changed }\n",
    )
    .expect("changed b event");
    host.refresh_source_roots().expect("changed scan");
    let third = host.snapshot();
    assert!(Arc::ptr_eq(
        second.file_states.get(&a).expect("second a state"),
        third.file_states.get(&a).expect("third a state")
    ));
    assert!(!Arc::ptr_eq(
        second.file_states.get(&b).expect("second b state"),
        third.file_states.get(&b).expect("third b state")
    ));
    assert_eq!(
        third.file_state(b).expect("changed b state").revision(),
        second
            .file_state(b)
            .expect("old b state")
            .revision()
            .saturating_add(1)
    );
    assert_eq!(third.index().definitions("event", "state.changed").len(), 1);
    let new_range = third
        .index()
        .active_definition("event", "state.changed")
        .expect("new b definition")
        .range;
    assert!(third.index().position_for(b, old_range).is_none());
    assert!(third.index().position_for(b, new_range).is_some());
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn one_overlay_edit_parses_and_lowers_exactly_once_in_a_populated_workspace() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-engine-pipeline-count-{nonce}"));
    let events = root.join("events");
    fs::create_dir_all(&events).expect("event directory");
    for index in 0..64 {
        fs::write(
            events.join(format!("event-{index:02}.txt")),
            format!("country_event = {{ id = synthetic.{index} }}\n"),
        )
        .expect("event fixture");
    }

    let mut host = eu4_host();
    host.apply_change(super::WorkspaceChange::SetSourceRoots(vec![
        SourceRoot::new(
            SourceRootId::new(1),
            SourceRootKind::CurrentMod,
            root.clone(),
        ),
    ]));
    host.refresh_source_roots().expect("initial scan");

    let path = events.join("event-00.txt");
    let id = DocumentId::new("file:///synthetic/events/event-00.txt");
    host.stage_open_document(
        id.clone(),
        1,
        "country_event = { id = synthetic.0 }\n".to_owned(),
        Some(path),
    )
    .expect("stage initial overlay");
    let initial = host
        .snapshot()
        .prepare_document(&id)
        .expect("prepare initial overlay");
    assert!(host.commit_prepared_document(initial));
    let before_edit = host.snapshot();

    reset_pipeline_counts();
    host.stage_document_text(
        &id,
        2,
        "country_event = { id = synthetic.changed }\n".to_owned(),
    )
    .expect("stage edit");
    assert_eq!(
        pipeline_counts(),
        (0, 0),
        "staging must not run semantic work"
    );

    let prepared = host
        .snapshot()
        .prepare_document(&id)
        .expect("prepare edited overlay");
    assert_eq!(pipeline_counts(), (1, 1));
    assert!(host.commit_prepared_document(prepared));
    assert_eq!(
        pipeline_counts(),
        (1, 1),
        "commit must not repeat worker work"
    );

    let after_edit = host.snapshot();
    for file_id in before_edit.source_files().keys() {
        assert!(Arc::ptr_eq(
            before_edit
                .file_states
                .get(file_id)
                .expect("old disk state"),
            after_edit
                .file_states
                .get(file_id)
                .expect("current disk state"),
        ));
    }
    assert!(
        after_edit
            .document(&id)
            .expect("edited overlay")
            .hir()
            .is_some_and(|hir| {
                hir.definitions()
                    .iter()
                    .any(|definition| definition.name == "synthetic.changed")
            })
    );
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn snapshots_share_immutable_state_and_preserve_old_revisions() {
    let mut host = AnalysisHost::new(RuleSet::empty());
    let first = host.snapshot();
    let second = host.snapshot();

    assert!(Arc::ptr_eq(&first.rules, &second.rules));
    assert!(Arc::ptr_eq(&first.profile, &second.profile));
    assert!(Arc::ptr_eq(&first.roots, &second.roots));
    assert!(Arc::ptr_eq(&first.documents, &second.documents));
    assert!(Arc::ptr_eq(&first.source_files, &second.source_files));
    assert!(Arc::ptr_eq(&first.file_states, &second.file_states));
    assert!(Arc::ptr_eq(&first.index, &second.index));
    assert!(Arc::ptr_eq(&first.scan_report, &second.scan_report));

    let id = DocumentId::new("file:///tmp/snapshot.txt");
    host.open_document(id.clone(), 1, "one".to_owned(), None)
        .expect("open should succeed");
    let third = host.snapshot();

    assert!(first.document(&id).is_none());
    assert_eq!(
        third
            .document(&id)
            .expect("new snapshot sees document")
            .text(),
        "one"
    );
    assert!(!Arc::ptr_eq(&first.documents, &third.documents));
    assert!(Arc::ptr_eq(&first.roots, &third.roots));
    assert!(Arc::ptr_eq(&first.profile, &third.profile));
    assert!(Arc::ptr_eq(&first.source_files, &third.source_files));
    assert!(Arc::ptr_eq(&first.file_states, &third.file_states));
    assert!(Arc::ptr_eq(&first.index, &third.index));
    assert!(Arc::ptr_eq(&first.scan_report, &third.scan_report));
}

#[test]
fn stale_document_versions_are_rejected_atomically() {
    let mut host = AnalysisHost::new(RuleSet::empty());
    let id = DocumentId::new("file:///tmp/example.txt");
    host.open_document(id.clone(), 1, "a😀z".to_owned(), None)
        .expect("open should succeed");
    let first = host.snapshot();
    let first_document = first.document(&id).expect("first document");
    let Some(ParsedSource::Text(first_parse)) = first_document.parsed() else {
        panic!("txt overlay should retain a text parse");
    };
    assert!(std::ptr::eq(
        first_parse.as_ref(),
        first_document.hir().expect("overlay HIR").syntax()
    ));
    let range = TextRange::new(1, 5).expect("emoji range");
    let error = host
        .apply_document_changes(&id, 1, &[TextChange::ranged(range, "x")])
        .expect_err("same version must be rejected");
    assert!(matches!(error, super::DocumentError::StaleVersion { .. }));
    assert_eq!(
        host.snapshot()
            .document(&id)
            .expect("document exists")
            .text(),
        "a😀z"
    );
    host.apply_document_changes(&id, 2, &[TextChange::ranged(range, "x")])
        .expect("new version should succeed");
    let second = host.snapshot();
    let second_document = second.document(&id).expect("document exists");
    assert_eq!(second_document.text(), "axz");
    let Some(ParsedSource::Text(second_parse)) = second_document.parsed() else {
        panic!("changed txt overlay should retain a text parse");
    };
    assert!(!Arc::ptr_eq(first_parse, second_parse));
    assert_eq!(
        first
            .document(&id)
            .expect("old snapshot remains valid")
            .text(),
        "a😀z"
    );
}

#[test]
fn prepared_document_commit_rejects_superseded_text_and_version() {
    let mut host = eu4_host();
    let id = DocumentId::new("file:///tmp/events/prepared.txt");
    host.stage_open_document(
        id.clone(),
        1,
        "country_event = { id = stale.1 }\n".to_owned(),
        None,
    )
    .expect("stage open");
    let staged = host.snapshot();
    assert!(
        staged
            .document(&id)
            .expect("staged document")
            .parsed()
            .is_none()
    );
    let stale = staged
        .prepare_document(&id)
        .expect("prepare stale candidate");

    host.stage_document_text(&id, 2, "country_event = { id = current.1 }\n".to_owned())
        .expect("stage newer text");
    assert!(!host.commit_prepared_document(stale));
    let current = host
        .snapshot()
        .prepare_document(&id)
        .expect("prepare current candidate");
    assert!(host.commit_prepared_document(current));

    let committed = host.snapshot();
    let document = committed.document(&id).expect("committed document");
    assert_eq!(document.version(), Some(2));
    assert!(document.parsed().is_some());
    assert!(document.hir().is_some_and(|hir| {
        hir.definitions()
            .iter()
            .any(|definition| definition.kind == "event" && definition.name == "current.1")
    }));
}

#[test]
fn close_restores_the_backing_disk_candidate() {
    let path = std::env::temp_dir().join(format!("pdx-engine-{}.txt", std::process::id()));
    fs::write(&path, "disk").expect("write fixture");
    let mut host = AnalysisHost::new(RuleSet::empty());
    host.apply_change(super::WorkspaceChange::SetSourceRoots(vec![
        SourceRoot::new(
            SourceRootId::new(1),
            SourceRootKind::CurrentMod,
            path.parent().expect("temp parent").to_owned(),
        ),
    ]));
    let id = DocumentId::new("file:///tmp/pdx-engine.txt");
    host.open_document(id.clone(), 1, "overlay".to_owned(), Some(path.clone()))
        .expect("open should succeed");
    host.close_document(&id).expect("close should succeed");
    let snapshot = host.snapshot();
    let document = snapshot.document(&id).expect("disk candidate exists");
    assert_eq!(document.source(), DocumentSource::Disk);
    assert_eq!(document.version(), None);
    assert_eq!(document.text(), "disk");
    fs::remove_file(path).expect("remove fixture");
}

#[test]
fn roots_overlay_and_shards_preserve_shadowed_semantic_definitions() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-engine-phase4-{nonce}"));
    let vanilla = root.join("vanilla");
    let dependency = root.join("dependency");
    let current = root.join("current");
    for directory in [
        vanilla.join("events"),
        dependency.join("events"),
        dependency.join("common/scripted_effects"),
        current.join("events"),
        current.join("common/scripted_triggers"),
        current.join("localisation/nested/deeper"),
    ] {
        fs::create_dir_all(directory).expect("fixture directory");
    }
    fs::write(
        vanilla.join("events/foo.txt"),
        "country_event = { id = foo.1 }\n",
    )
    .expect("vanilla event");
    fs::write(
        dependency.join("events/foo.txt"),
        "country_event = { id = foo.1 }\n",
    )
    .expect("dependency event");
    fs::write(
        dependency.join("common/scripted_effects/effects.txt"),
        "heal_army = { add_manpower = 1 }\n",
    )
    .expect("effect");
    let current_event = current.join("events/foo.txt");
    fs::write(&current_event, "country_event = { id = foo.1 }\n").expect("current event");
    fs::write(
        current.join("common/scripted_triggers/triggers.txt"),
        "is_ready = { always = yes }\n",
    )
    .expect("trigger");
    fs::write(
        current.join("localisation/nested/deeper/test_l_english.yml"),
        "l_english:\n foo_name:0 \"Foo\"\n",
    )
    .expect("localisation");
    fs::write(
        current.join("outside.yml"),
        "l_english:\n ignored_name:0 \"Ignored\"\n",
    )
    .expect("outside localisation");

    let mut host = eu4_host();
    host.apply_change(super::WorkspaceChange::SetSourceRoots(vec![
        SourceRoot {
            id: SourceRootId::new(1),
            kind: SourceRootKind::Vanilla,
            path: vanilla,
            order: 0,
            writable: false,
        },
        SourceRoot {
            id: SourceRootId::new(2),
            kind: SourceRootKind::Dependency,
            path: dependency,
            order: 1,
            writable: false,
        },
        SourceRoot {
            id: SourceRootId::new(3),
            kind: SourceRootKind::CurrentMod,
            path: current.clone(),
            order: 2,
            writable: true,
        },
    ]));
    host.refresh_source_roots().expect("scan roots");
    let snapshot = host.snapshot();
    let event_definitions = snapshot.index().definitions("event", "foo.1");
    assert_eq!(event_definitions.len(), 3);
    assert_eq!(
        snapshot
            .index()
            .active_definition("event", "foo.1")
            .expect("active event")
            .file_id,
        event_definitions[0].file_id
    );
    assert_eq!(
        snapshot
            .index()
            .definitions("scripted_effect", "heal_army")
            .len(),
        1
    );
    assert_eq!(
        snapshot
            .index()
            .definitions("scripted_trigger", "is_ready")
            .len(),
        1
    );
    assert_eq!(
        snapshot
            .index()
            .definitions("localisation", "foo_name")
            .len(),
        1
    );
    assert!(
        snapshot
            .index()
            .definitions("localisation", "ignored_name")
            .is_empty()
    );

    let logical = LogicalPath::new("events/foo.txt");
    assert_eq!(
        snapshot
            .resolve(&logical)
            .iter()
            .filter(|candidate| candidate.active)
            .count(),
        1
    );
    host.open_document(
        DocumentId::new("file:///current/foo.txt"),
        1,
        "country_event = { id = foo.1 }\n".to_owned(),
        Some(current_event.clone()),
    )
    .expect("overlay");
    let overlay_snapshot = host.snapshot();
    let resolved = overlay_snapshot.resolve(&logical);
    assert!(
        resolved
            .first()
            .and_then(|candidate| candidate.document_id.as_ref())
            .is_some()
    );
    assert!(resolved.first().is_some_and(|candidate| candidate.active));
    fs::remove_dir_all(root).expect("cleanup");
}
