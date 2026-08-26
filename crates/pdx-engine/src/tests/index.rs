use super::*;

#[test]
fn grouped_position_map_keeps_sorted_lookup_and_replacement_semantics() {
    let first = SourceFileId::new(1);
    let second = SourceFileId::new(2);
    let first_range = TextRange::new(10, 14).expect("range");
    let second_range = TextRange::new(2, 8).expect("range");
    let first_position = PositionRange::new(Position::new(1, 2), Position::new(1, 6));
    let replacement = PositionRange::new(Position::new(4, 0), Position::new(4, 4));
    let second_position = PositionRange::new(Position::new(0, 2), Position::new(0, 8));

    let mut map = PositionMap::from_entries([
        ((first, first_range), first_position),
        ((first, first_range), replacement),
        ((second, second_range), second_position),
    ]);
    assert_eq!(map.len(), 2);
    assert_eq!(map.get((first, first_range)), Some(&replacement));
    assert_eq!(map.get((second, second_range)), Some(&second_position));

    map.replace_file(first, [(first_range, first_position)]);
    assert_eq!(map.len(), 2);
    assert_eq!(map.get((first, first_range)), Some(&first_position));
    map.remove_file(second);
    assert_eq!(map.len(), 1);
    assert!(map.get((second, second_range)).is_none());
}

#[test]
fn grouped_localisation_preview_map_keeps_sorted_lookup_and_replacement_semantics() {
    let first = SourceFileId::new(1);
    let second = SourceFileId::new(2);
    let first_range = TextRange::new(10, 14).expect("range");
    let second_range = TextRange::new(2, 8).expect("range");
    let first_preview = LocalisationPreview {
        language: Some("english".to_owned()),
        value: "first".to_owned(),
    };
    let replacement = LocalisationPreview {
        language: Some("french".to_owned()),
        value: "replacement".to_owned(),
    };
    let second_preview = LocalisationPreview {
        language: None,
        value: "second".to_owned(),
    };

    let mut map = LocalisationPreviewMap::from_entries([
        ((first, first_range), first_preview.clone()),
        ((first, first_range), replacement.clone()),
        ((second, second_range), second_preview.clone()),
    ]);
    assert_eq!(map.len(), 2);
    assert_eq!(map.get((first, first_range)), Some(&replacement));
    assert_eq!(map.get((second, second_range)), Some(&second_preview));

    map.replace_file(first, [(first_range, first_preview.clone())]);
    assert_eq!(map.len(), 2);
    assert_eq!(map.get((first, first_range)), Some(&first_preview));
    map.remove_file(second);
    assert_eq!(map.len(), 1);
    assert!(map.get((second, second_range)).is_none());
}

#[test]
fn bulk_index_build_retains_every_shard_and_definition() {
    let first_file = SourceFileId::new(1);
    let second_file = SourceFileId::new(2);
    let range = TextRange::new(0, 3).expect("range");
    let shards = [
        FileIndexShard {
            file_id: first_file,
            definitions: vec![Definition {
                kind: "event".to_owned(),
                name: "shared.1".to_owned(),
                file_id: first_file,
                range,
                active: true,
            }],
            references: Vec::new(),
            macro_definitions: Vec::new(),
            syntax_error_count: 0,
        },
        FileIndexShard {
            file_id: second_file,
            definitions: vec![Definition {
                kind: "event".to_owned(),
                name: "shared.1".to_owned(),
                file_id: second_file,
                range,
                active: true,
            }],
            references: Vec::new(),
            macro_definitions: Vec::new(),
            syntax_error_count: 0,
        },
    ];

    let index = WorkspaceIndex::from_shards(shards);

    assert!(index.shard(first_file).is_some());
    assert!(index.shard(second_file).is_some());
    assert_eq!(index.definitions("event", "SHARED.1").len(), 2);
}

#[test]
fn parallel_file_state_materialization_is_deterministic() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-engine-parallel-{nonce}"));
    let events = root.join("events");
    fs::create_dir_all(&events).expect("event directory");
    for index in 0..64 {
        fs::write(
            events.join(format!("event-{index:02}.txt")),
            format!("country_event = {{ id = parallel.{index} }}\n"),
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
    let report = host.refresh_source_roots().expect("parallel scan");
    assert_eq!(report.indexed_files, 64);
    let first = host.snapshot();
    assert_eq!(first.index().definitions("event", "parallel.63").len(), 1);

    host.refresh_source_roots()
        .expect("unchanged parallel scan");
    assert_eq!(host.snapshot().index(), first.index());
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn type_per_file_definition_is_emitted_once_without_generic_pseudo_members() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-engine-type-per-file-{nonce}"));
    let countries = root.join("common/countries");
    fs::create_dir_all(&countries).expect("country directory");
    fs::write(
        countries.join("AAA.txt"),
        "country = { color = { 1 2 3 } }\n",
    )
    .expect("country fixture");

    let rules = pdx_game::eu4::first_party_rules().expect("first-party rules");
    let mut host = AnalysisHost::with_profile(rules, pdx_game::eu4::profile());
    host.apply_change(super::WorkspaceChange::SetSourceRoots(vec![
        SourceRoot::new(SourceRootId::new(1), SourceRootKind::CurrentMod, root),
    ]));
    host.refresh_source_roots().expect("scan country file");

    let snapshot = host.snapshot();
    let definitions = snapshot.index().definitions("country_file", "AAA");
    assert_eq!(definitions.len(), 1);
    assert_eq!(
        snapshot
            .index()
            .definitions_for_kind("country_file")
            .count(),
        1
    );
    assert!(
        snapshot
            .index()
            .definitions("country_file", "country")
            .is_empty()
    );
    assert!(
        snapshot
            .index()
            .active_definition("country_file", "AAA")
            .is_some()
    );
    let file_id = definitions[0].file_id;
    let hir_definition = snapshot
        .file_state(file_id)
        .and_then(|state| state.hir())
        .and_then(|hir| {
            hir.definitions()
                .iter()
                .find(|definition| definition.kind == "country_file" && definition.name == "AAA")
        })
        .expect("HIR country definition");
    assert_eq!(definitions[0].range, hir_definition.range);
    fs::remove_dir_all(
        countries
            .parent()
            .and_then(std::path::Path::parent)
            .expect("fixture root"),
    )
    .expect("cleanup");
}

#[test]
fn symbol_case_policy_controls_definition_lookup_identity() {
    let file_id = SourceFileId::new(9);
    let range = TextRange::new(0, 3).expect("range");
    let mut model = RulesModel {
        game_id: "test".to_owned(),
        ..RulesModel::default()
    };
    model.symbol_descriptors.push(SymbolDescriptor {
        kind_id: "case_sensitive_kind".to_owned(),
        resolution: SymbolResolutionPolicy::ReplaceBySymbol,
        case_sensitive: true,
    });
    let rules = RuleSet::from_model(model);
    let mut index = WorkspaceIndex::from_shards([FileIndexShard {
        file_id,
        definitions: vec![Definition {
            kind: "case_sensitive_kind".to_owned(),
            name: "MixedName".to_owned(),
            file_id,
            range,
            active: true,
        }],
        references: Vec::new(),
        macro_definitions: Vec::new(),
        syntax_error_count: 0,
    }]);
    index.configure_case_sensitivity(&rules);

    assert_eq!(
        index.definitions("case_sensitive_kind", "MixedName").len(),
        1
    );
    assert!(
        index
            .definitions("case_sensitive_kind", "mixedname")
            .is_empty()
    );
}

#[test]
fn identity_only_host_does_not_leak_eu4_dynamic_symbols() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-engine-generic-profile-{nonce}"));
    let cultures = root.join("common/cultures");
    let scripted_effects = root.join("common/scripted_effects");
    for directory in [&cultures, &scripted_effects] {
        fs::create_dir_all(directory).expect("fixture directory");
    }
    fs::write(
        cultures.join("cultures.txt"),
        "germanic = { set_country_flag = generic_flag }\n",
    )
    .expect("culture fixture");
    fs::write(
        scripted_effects.join("effects.txt"),
        "example = { value = $AMOUNT$ }\n",
    )
    .expect("scripted effect fixture");

    let mut host = AnalysisHost::new(pdx_game::eu4::bootstrap_rules());
    host.apply_change(super::WorkspaceChange::SetSourceRoots(vec![
        SourceRoot::new(
            SourceRootId::new(1),
            SourceRootKind::CurrentMod,
            root.clone(),
        ),
    ]));
    host.refresh_source_roots().expect("scan roots");
    let snapshot = host.snapshot();

    assert!(
        snapshot
            .index()
            .definitions("culture", "germanic")
            .is_empty()
    );
    assert!(
        snapshot
            .index()
            .definitions("country_flag", "generic_flag")
            .is_empty()
    );
    assert!(
        snapshot
            .index()
            .definitions("scripted_effect_param", "AMOUNT")
            .is_empty()
    );
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn shard_replacement_updates_only_its_definition_and_reference_buckets() {
    let first_file = SourceFileId::new(1);
    let second_file = SourceFileId::new(2);
    let range = TextRange::new(0, 3).expect("range");
    let definition = |file_id, name: &str| Definition {
        kind: "event".to_owned(),
        name: name.to_owned(),
        file_id,
        range,
        active: true,
    };
    let reference = |file_id, name: &str| Reference {
        kind: "event".to_owned(),
        name: name.to_owned(),
        file_id,
        range,
    };
    let mut index = WorkspaceIndex::from_shards([
        FileIndexShard {
            file_id: first_file,
            definitions: vec![definition(first_file, "old.1")],
            references: vec![reference(first_file, "old.1")],
            macro_definitions: Vec::new(),
            syntax_error_count: 0,
        },
        FileIndexShard {
            file_id: second_file,
            definitions: vec![definition(second_file, "untouched.1")],
            references: vec![reference(second_file, "untouched.1")],
            macro_definitions: Vec::new(),
            syntax_error_count: 0,
        },
    ]);

    index.replace_shard(FileIndexShard {
        file_id: first_file,
        definitions: vec![definition(first_file, "new.1")],
        references: vec![reference(first_file, "new.1")],
        macro_definitions: Vec::new(),
        syntax_error_count: 1,
    });

    assert!(index.definitions("event", "old.1").is_empty());
    assert_eq!(index.definitions("event", "new.1").len(), 1);
    assert_eq!(index.definitions("event", "untouched.1").len(), 1);
    assert_eq!(index.references(first_file)[0].name, "new.1");
    assert_eq!(index.references(second_file)[0].name, "untouched.1");
    assert_eq!(
        index
            .shard(first_file)
            .expect("replacement shard")
            .syntax_error_count,
        1
    );

    index.remove_shard(first_file);
    assert!(index.definitions("event", "new.1").is_empty());
    assert!(index.references(first_file).is_empty());
    assert_eq!(index.definitions("event", "untouched.1").len(), 1);
}

#[test]
fn replacement_re_resolves_only_affected_symbol_buckets_without_hiding_ties() {
    let first_file = SourceFileId::new(1);
    let second_file = SourceFileId::new(2);
    let range = TextRange::new(0, 3).expect("range");
    let definition = |file_id| Definition {
        kind: "event".to_owned(),
        name: "shared.1".to_owned(),
        file_id,
        range,
        active: true,
    };
    let mut index = WorkspaceIndex::from_shards([
        FileIndexShard {
            file_id: first_file,
            definitions: vec![definition(first_file)],
            references: Vec::new(),
            macro_definitions: Vec::new(),
            syntax_error_count: 0,
        },
        FileIndexShard {
            file_id: second_file,
            definitions: vec![definition(second_file)],
            references: Vec::new(),
            macro_definitions: Vec::new(),
            syntax_error_count: 0,
        },
    ]);
    let rules = pdx_game::eu4::bootstrap_rules();
    let tied = BTreeMap::from([(first_file, 10), (second_file, 10)]);
    index.resolve_priorities(&tied, &rules);
    assert_eq!(
        index
            .definitions("event", "shared.1")
            .iter()
            .filter(|item| item.active)
            .count(),
        2
    );
    assert!(index.active_definition("event", "shared.1").is_none());

    let ordered = BTreeMap::from([(first_file, 10), (second_file, 20)]);
    index.resolve_priorities(&ordered, &rules);
    assert_eq!(
        index
            .active_definition("event", "shared.1")
            .expect("higher priority definition")
            .file_id,
        second_file
    );
    index.remove_shard_resolved(second_file, &ordered, &rules);
    assert_eq!(
        index
            .active_definition("event", "shared.1")
            .expect("remaining definition")
            .file_id,
        first_file
    );
}

#[test]
fn identical_collector_records_resolve_as_one_physical_definition() {
    let file_id = SourceFileId::new(1);
    let range = TextRange::new(4, 12).expect("range");
    let definition = Definition {
        kind: "scripted_effect".to_owned(),
        name: "apply".to_owned(),
        file_id,
        range,
        active: true,
    };
    let index = WorkspaceIndex::from_shards([FileIndexShard {
        file_id,
        definitions: vec![definition.clone(), definition],
        references: Vec::new(),
        macro_definitions: Vec::new(),
        syntax_error_count: 0,
    }]);

    assert_eq!(
        index
            .active_definition("scripted_effect", "apply")
            .expect("identical records are one physical definition")
            .range,
        range
    );

    let distinct_range = TextRange::new(20, 28).expect("distinct range");
    let distinct = WorkspaceIndex::from_shards([FileIndexShard {
        file_id,
        definitions: vec![
            Definition {
                kind: "scripted_effect".to_owned(),
                name: "apply".to_owned(),
                file_id,
                range,
                active: true,
            },
            Definition {
                kind: "scripted_effect".to_owned(),
                name: "apply".to_owned(),
                file_id,
                range: distinct_range,
                active: true,
            },
        ],
        references: Vec::new(),
        macro_definitions: Vec::new(),
        syntax_error_count: 0,
    }]);
    assert!(
        distinct
            .active_definition("scripted_effect", "apply")
            .is_none()
    );
}
