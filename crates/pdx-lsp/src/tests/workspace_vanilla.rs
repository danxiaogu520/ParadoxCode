use std::fs;
use std::io::Cursor;

use lsp_types::SymbolInformation;
use pdx_engine::{
    AnalysisHost, SourceRoot, SourceRootId, SourceRootKind, VanillaIndexCache, WorkspaceChange,
};
use pdx_game::{DiscoveryOptions, DiscoveryOutcome, UserConfiguration, UserPaths};

use serde_json::{Value, json};

use super::*;

#[test]
fn workspace_root_is_scanned_as_current_mod_without_project_config() {
    let (root, root_uri) = temp_workspace_dir();
    fs::create_dir_all(root.join("common/country_tags")).expect("country tags directory");
    fs::create_dir_all(root.join("missions")).expect("missions directory");
    fs::write(
        root.join("common/country_tags/00_tags.txt"),
        "KTP = \"countries/KTP.txt\"\n",
    )
    .expect("country tag source");
    let mission = root.join("missions/test_missions.txt");
    fs::write(&mission, "country_event = { id = test.1 }\n").expect("mission source");
    let mission_uri = canonical_uri(&mission);
    let input = frames([
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"rootUri":root_uri,"capabilities":{}}}),
        json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
        json!({"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":mission_uri,"languageId":"eu4","version":1,"text":"country_event = { id = test.1 }\n"}}}),
        json!({"jsonrpc":"2.0","id":2,"method":"shutdown","params":{}}),
        json!({"jsonrpc":"2.0","method":"exit"}),
    ]);
    let mut output = Vec::new();
    let mut server = eu4_server(InitializeOptions).expect("embedded rules");
    server
        .run_transport(Cursor::new(input), &mut output)
        .expect("transport");
    assert!(
        server
            .snapshot()
            .index()
            .active_definition("country_tag", "KTP")
            .is_some()
    );
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn watched_file_registration_and_notification_update_the_disk_index() {
    let (root, root_uri) = temp_workspace_dir();
    let events = root.join("common/events");
    fs::create_dir_all(&events).expect("events directory");
    let definition = events.join("watched.txt");
    fs::write(&definition, "country_event = { id = old.1 }\n").expect("initial definition");
    let definition_uri = canonical_uri(&definition);
    let changed_definition = definition.clone();
    let input = ScriptedReader::new([
        (
            json!({
                "jsonrpc":"2.0",
                "id":1,
                "method":"initialize",
                "params":{
                    "rootUri":root_uri,
                    "capabilities":{
                        "workspace":{
                            "didChangeWatchedFiles":{
                                "dynamicRegistration":true,
                                "relativePatternSupport":true
                            }
                        }
                    }
                }
            }),
            None,
        ),
        (
            json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
            None,
        ),
        (
            json!({
                "jsonrpc":"2.0",
                "method":"workspace/didChangeWatchedFiles",
                "params":{"changes":[{"uri":definition_uri,"type":2}]}
            }),
            Some(Box::new(move || {
                fs::write(
                    changed_definition,
                    "country_event = { id = watched-new.1 }\n",
                )
                .expect("write watched definition");
            }) as Box<dyn FnOnce() + Send>),
        ),
        (
            json!({
                "jsonrpc":"2.0",
                "id":2,
                "method":"workspace/symbol",
                "params":{"query":"watched-new"}
            }),
            None,
        ),
        (
            json!({"jsonrpc":"2.0","id":3,"method":"shutdown","params":{}}),
            None,
        ),
        (json!({"jsonrpc":"2.0","method":"exit"}), None),
    ]);
    let mut output = Vec::new();
    let mut server = eu4_server(InitializeOptions).expect("embedded rules");
    server.run_transport(input, &mut output).expect("transport");
    let responses = decode_frames(&output);

    let registration = responses
        .iter()
        .find(|value| value["method"] == "client/registerCapability")
        .expect("watched-file dynamic registration");
    let watcher = &registration["params"]["registrations"][0]["registerOptions"]["watchers"][0];
    assert_eq!(watcher["globPattern"]["baseUri"], root_uri);
    assert_eq!(watcher["globPattern"]["pattern"], "**/*");
    assert_eq!(watcher["kind"], 7);

    let symbols = typed_result::<Vec<SymbolInformation>>(&responses, 2);
    assert_eq!(symbols.len(), 1);
    assert_eq!(symbols[0].name, "watched-new.1");
    assert!(
        server
            .snapshot()
            .index()
            .active_definition("event", "old.1")
            .is_none()
    );
    assert!(
        server
            .snapshot()
            .index()
            .active_definition("event", "watched-new.1")
            .is_some()
    );
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn project_config_loads_ordered_dependencies_and_keeps_them_read_only() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-lsp-project-config-{nonce}"));
    let config_dir = root.join(".pdx");
    let current = root.join("mod");
    let low = root.join("dependencies/low");
    let high = root.join("dependencies/high");
    let vanilla = root.join("vanilla");
    fs::create_dir_all(&config_dir).expect("config directory");
    for directory in [&current, &low, &high, &vanilla] {
        fs::create_dir_all(directory.join("common/events")).expect("fixture directory");
    }
    let canonical_root = fs::canonicalize(&root).expect("canonical root");
    let current = fs::canonicalize(&current).expect("canonical current");
    let low = fs::canonicalize(&low).expect("canonical low");
    let high = fs::canonicalize(&high).expect("canonical high");
    let vanilla = fs::canonicalize(&vanilla).expect("canonical vanilla");
    let inline = super::resolve_source_roots(
        Some(&canonical_root),
        Some(json!({
            "modDirectory": "mod",
            "dependencies": [
                {"id": "low", "path": "dependencies/low"},
                {"id": "high", "path": "dependencies/high"}
            ]
        })),
        &pdx_engine::WorkspaceScanToken::new(),
    )
    .expect("inline initializationOptions");
    assert_eq!(inline.roots.len(), 3);
    let overlap = super::resolve_source_roots(
        Some(&canonical_root),
        Some(json!({
            "modDirectory": "mod",
            "dependencies": [{"id": "nested", "path": "mod/common"}]
        })),
        &pdx_engine::WorkspaceScanToken::new(),
    )
    .expect_err("nested source roots must be rejected");
    assert_eq!(overlap.code, INVALID_PARAMS);
    assert!(overlap.message.contains("must not overlap"));
    fs::write(
        vanilla.join("common/events/definitions.txt"),
        "country_event = { id = vanilla.1 }\n",
    )
    .expect("Vanilla definition");
    let mut vanilla_host = AnalysisHost::with_profile(
        pdx_game::eu4::first_party_rules().expect("rules for Vanilla cache"),
        pdx_game::eu4::profile(),
    );
    vanilla_host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(0),
        SourceRootKind::Vanilla,
        fs::canonicalize(&vanilla).expect("canonical Vanilla root"),
    )]));
    vanilla_host
        .refresh_source_roots()
        .expect("scan Vanilla once");
    let vanilla_cache =
        VanillaIndexCache::from_snapshot(&vanilla_host.snapshot()).expect("build Vanilla cache");
    let vanilla_cache_path = config_dir.join("vanilla.pdxindex");
    vanilla_cache
        .save(&vanilla_cache_path)
        .expect("save Vanilla cache");
    fs::rename(&vanilla, root.join("vanilla-moved"))
        .expect("make Vanilla source unavailable after caching");
    fs::write(
        config_dir.join("project.toml"),
        r#"mod_directory = "mod"
vanilla_index_cache = ".pdx/vanilla.pdxindex"

[[dependencies]]
id = "low"
path = "dependencies/low"

[[dependencies]]
id = "high"
path = "dependencies/high"
"#,
    )
    .expect("project config");
    fs::write(
        low.join("common/events/definitions.txt"),
        concat!(
            "country_event = { id = shared.1 }\n",
            "country_event = { id = dependency-shared.1 }\n",
            "country_event = { id = dependency.1 }\n"
        ),
    )
    .expect("low dependency");
    fs::write(
        high.join("common/events/definitions.txt"),
        "country_event = { id = shared.1 }\ncountry_event = { id = dependency-shared.1 }\n",
    )
    .expect("high dependency");
    fs::write(
        current.join("common/events/definitions.txt"),
        "country_event = { id = shared.1 }\n",
    )
    .expect("current mod");
    let reference_path = current.join("common/events/reference.txt");
    fs::write(&reference_path, "event = dependency.1\nevent = vanilla.1\n")
        .expect("current reference");

    let reference_uri = canonical_uri(&reference_path);
    let root_uri = canonical_uri(&canonical_root);
    let input = frames([
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"rootUri":root_uri,"capabilities":{},"initializationOptions":{"projectConfig":".pdx/project.toml"}}}),
        json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
        json!({"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":reference_uri,"languageId":"eu4","version":1,"text":"event = dependency.1\nevent = vanilla.1\n"}}}),
        json!({"jsonrpc":"2.0","id":2,"method":"textDocument/rename","params":{"textDocument":{"uri":reference_uri},"position":{"line":0,"character":10},"newName":"renamed.1"}}),
        json!({"jsonrpc":"2.0","id":4,"method":"textDocument/definition","params":{"textDocument":{"uri":reference_uri},"position":{"line":1,"character":8}}}),
        json!({"jsonrpc":"2.0","id":3,"method":"shutdown","params":{}}),
        json!({"jsonrpc":"2.0","method":"exit"}),
    ]);
    let mut output = Vec::new();
    let mut server = eu4_server(InitializeOptions).expect("embedded rules");
    server
        .run_transport(Cursor::new(input), &mut output)
        .expect("transport");
    let responses = decode_frames(&output);

    let snapshot = server.snapshot();
    let roots = snapshot.source_roots();
    assert_eq!(roots.len(), 4);
    assert_eq!(roots[0].kind, pdx_engine::SourceRootKind::Vanilla);
    assert_eq!(roots[1].kind, pdx_engine::SourceRootKind::Dependency);
    assert_eq!(roots[1].order, 0);
    assert_eq!(roots[2].kind, pdx_engine::SourceRootKind::Dependency);
    assert_eq!(roots[2].order, 1);
    assert_eq!(roots[3].kind, pdx_engine::SourceRootKind::CurrentMod);
    assert!(roots[3].writable);
    let active = snapshot
        .index()
        .active_definition("event", "shared.1")
        .expect("current definition should win");
    assert!(
        snapshot
            .source_files()
            .get(&active.file_id)
            .is_some_and(|file| file.physical_path.starts_with(&current))
    );
    let active_dependency = snapshot
        .index()
        .active_definition("event", "dependency-shared.1")
        .expect("higher ordered dependency should win");
    assert!(
        snapshot
            .source_files()
            .get(&active_dependency.file_id)
            .is_some_and(|file| file.physical_path.starts_with(&high))
    );
    let vanilla_definition = snapshot
        .index()
        .active_definition("event", "vanilla.1")
        .expect("cached Vanilla definition");
    assert_eq!(
        snapshot
            .source_files()
            .get(&vanilla_definition.file_id)
            .expect("cached source metadata")
            .root_id,
        SourceRootId::new(0)
    );
    let vanilla_definition_response = responses
        .iter()
        .find(|value| value["id"] == 4)
        .expect("Vanilla definition response");
    assert_eq!(vanilla_definition_response["error"], Value::Null);
    assert_eq!(
        vanilla_definition_response["result"][0]["range"],
        json!({
            "start": {"line": 0, "character": 23},
            "end": {"line": 0, "character": 32}
        })
    );
    let rename = responses
        .iter()
        .find(|value| value["id"] == 2)
        .expect("rename response");
    assert_eq!(rename["error"]["code"], INVALID_PARAMS);
    assert!(
        rename["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("read-only"))
    );
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn missing_vanilla_cache_degrades_with_an_lsp_warning() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-lsp-missing-vanilla-cache-{nonce}"));
    fs::create_dir_all(root.join("common/events")).expect("workspace fixture");
    let input = frames([
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"rootUri":path_to_uri(&root),"capabilities":{},"initializationOptions":{"vanillaIndexCache":".pdx/missing.pdxindex"}}}),
        json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
        json!({"jsonrpc":"2.0","id":2,"method":"shutdown","params":{}}),
        json!({"jsonrpc":"2.0","method":"exit"}),
    ]);
    let mut output = Vec::new();
    let mut server = eu4_server(InitializeOptions).expect("embedded rules");
    server
        .run_transport(Cursor::new(input), &mut output)
        .expect("transport");
    let responses = decode_frames(&output);

    assert!(
        responses
            .iter()
            .any(|value| value["id"] == 1 && value.get("result").is_some())
    );
    let warning = responses
        .iter()
        .find(|value| value["method"] == "window/showMessage")
        .expect("missing cache warning");
    assert_eq!(warning["params"]["type"], 2);
    assert!(
        warning["params"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("continuing without Vanilla symbols"))
    );
    assert_eq!(server.snapshot().source_roots().len(), 1);
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn initialize_defers_an_existing_vanilla_cache() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let container = std::env::temp_dir().join(format!("pdx-lsp-deferred-cache-{nonce}"));
    let workspace = container.join("workspace");
    let vanilla = container.join("vanilla");
    fs::create_dir_all(&workspace).expect("workspace directory");
    fs::create_dir_all(&vanilla).expect("Vanilla directory");
    let vanilla = fs::canonicalize(&vanilla).expect("canonical Vanilla directory");
    let cache_path = container.join("vanilla.pdxindex");

    let mut vanilla_host = AnalysisHost::with_profile(
        pdx_game::eu4::first_party_rules().expect("embedded rules"),
        pdx_game::eu4::profile(),
    );
    vanilla_host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(0),
        SourceRootKind::Vanilla,
        vanilla,
    )]));
    let cache =
        VanillaIndexCache::from_snapshot(&vanilla_host.snapshot()).expect("empty Vanilla cache");
    cache.save(&cache_path).expect("save Vanilla cache");

    let params = serde_json::from_value(json!({
        "rootUri": path_to_uri(&workspace),
        "capabilities": {},
        "initializationOptions": {"vanillaIndexCache": cache_path}
    }))
    .expect("initialize params");
    let candidate = prepare_initialize_candidate(
        AnalysisHost::with_profile(
            pdx_game::eu4::first_party_rules().expect("embedded rules"),
            pdx_game::eu4::profile(),
        ),
        params,
        true,
        None,
        &pdx_engine::WorkspaceScanToken::new(),
    )
    .expect("prepare initialize candidate");

    assert_eq!(candidate.vanilla_cache, Some(cache_path.clone()));
    assert!(
        candidate
            .host
            .snapshot()
            .source_roots()
            .iter()
            .all(|root| root.kind != SourceRootKind::Vanilla)
    );
    fs::remove_dir_all(container).expect("cleanup");
}
#[test]
fn stale_vanilla_cache_is_regenerated_with_an_explicit_notification() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let container = std::env::temp_dir().join(format!("pdx-lsp-regen-cache-{nonce}"));
    let cache_path = stale_cache_fixture(&container);
    let first_party_rules = pdx_game::eu4::first_party_rules().expect("embedded rules");
    assert_ne!(
        VanillaIndexCache::load(&cache_path)
            .expect("stale cache reload")
            .metadata()
            .rule_hash,
        first_party_rules.rule_hash().to_hex()
    );

    let input = frames([
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"rootUri":path_to_uri(&container.join("workspace")),"capabilities":{},"initializationOptions":{"vanillaIndexCache":cache_path}}}),
        json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
        json!({"jsonrpc":"2.0","id":2,"method":"shutdown","params":{}}),
        json!({"jsonrpc":"2.0","method":"exit"}),
    ]);
    let mut output = Vec::new();
    let mut server = eu4_server(InitializeOptions).expect("embedded rules");
    server
        .run_transport(Cursor::new(input), &mut output)
        .expect("transport");
    let responses = decode_frames(&output);

    responses
        .iter()
        .find(|value| {
            value["method"] == "window/showMessage"
                && value["params"]["type"] == 3
                && value["params"]["message"]
                    .as_str()
                    .is_some_and(|message| message.contains("regenerated"))
        })
        .expect("regeneration info notification");
    assert!(
        responses
            .iter()
            .any(|value| value["method"] == "window/showMessage"
                && value["params"]["type"] == 3
                && value["params"]["message"]
                    .as_str()
                    .is_some_and(|message| message.contains("background"))),
        "without workDoneProgress the fallback start message keeps the user informed"
    );
    assert!(
        !responses.iter().any(|value| {
            value["method"] == "window/showMessage" && value["params"]["type"] == 2
        }),
        "no stale-cache warning should remain after a successful regeneration"
    );
    assert_eq!(
        VanillaIndexCache::load(&cache_path)
            .expect("regenerated cache reload")
            .metadata()
            .rule_hash,
        first_party_rules.rule_hash().to_hex(),
        "the cache file on disk must be replaced with the regenerated hash"
    );
    assert_eq!(server.snapshot().source_roots().len(), 2);
    fs::remove_dir_all(container).expect("cleanup");
}

#[test]
fn stale_cache_regeneration_reports_work_done_progress() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let container = std::env::temp_dir().join(format!("pdx-lsp-regen-progress-{nonce}"));
    let cache_path = stale_cache_fixture(&container);
    let events = container.join("vanilla/events");
    fs::create_dir_all(&events).expect("vanilla events directory");
    for index in 0..4 {
        fs::write(
            events.join(format!("probe_{index}.txt")),
            format!("country_event = {{ id = probe.{index} }}\n"),
        )
        .expect("vanilla event");
    }

    let input = frames([
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"rootUri":path_to_uri(&container.join("workspace")),"capabilities":{"window":{"workDoneProgress":true}},"initializationOptions":{"vanillaIndexCache":cache_path}}}),
        json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
        json!({"jsonrpc":"2.0","id":2,"method":"shutdown","params":{}}),
        json!({"jsonrpc":"2.0","method":"exit"}),
    ]);
    let mut output = Vec::new();
    let mut server = eu4_server(InitializeOptions).expect("embedded rules");
    server
        .run_transport(Cursor::new(input), &mut output)
        .expect("transport");
    let responses = decode_frames(&output);

    assert!(
        responses.iter().any(|value| {
            value["method"] == "window/workDoneProgress/create"
                && value["params"]["token"].as_str().is_some()
        }),
        "a work-done-progress create request must precede the reports"
    );
    assert!(
        responses.iter().any(|value| {
            value["method"] == "$/progress"
                && value["params"]["value"]["kind"] == "begin"
                && value["params"]["value"]["title"] == "ParadoxCode"
        }),
        "a begin report must be emitted"
    );
    assert!(
        responses.iter().any(|value| {
            value["method"] == "$/progress"
                && value["params"]["value"]["kind"] == "report"
                && value["params"]["value"]["percentage"].is_u64()
        }),
        "at least one indexed-files progress report must be emitted"
    );
    assert!(
        responses.iter().any(|value| {
            value["method"] == "$/progress" && value["params"]["value"]["kind"] == "end"
        }),
        "an end report must be emitted once the worker finishes"
    );
    assert_eq!(
        VanillaIndexCache::load(&cache_path)
            .expect("regenerated cache reload")
            .metadata()
            .rule_hash,
        pdx_game::eu4::first_party_rules()
            .expect("embedded rules")
            .rule_hash()
            .to_hex()
    );
    fs::remove_dir_all(container).expect("cleanup");
}

#[test]
fn stale_vanilla_cache_reports_regeneration_failure_explicitly() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let container = std::env::temp_dir().join(format!("pdx-lsp-regen-failure-{nonce}"));
    let cache_path = stale_cache_fixture(&container);
    fs::remove_dir_all(container.join("vanilla")).expect("remove Vanilla directory");

    let input = frames([
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"rootUri":path_to_uri(&container.join("workspace")),"capabilities":{},"initializationOptions":{"vanillaIndexCache":cache_path}}}),
        json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
        json!({"jsonrpc":"2.0","id":2,"method":"shutdown","params":{}}),
        json!({"jsonrpc":"2.0","method":"exit"}),
    ]);
    let mut output = Vec::new();
    let mut server = eu4_server(InitializeOptions).expect("embedded rules");
    server
        .run_transport(Cursor::new(input), &mut output)
        .expect("transport");
    let responses = decode_frames(&output);

    let warning = responses
        .iter()
        .find(|value| value["method"] == "window/showMessage" && value["params"]["type"] == 2)
        .expect("regeneration failure warning");
    let message = warning["params"]["message"]
        .as_str()
        .expect("warning message");
    assert!(
        message.contains("regeneration") && message.contains("using the existing cache"),
        "the failure must be explicit and keep the stale cache fallback: {message}"
    );
    fs::remove_dir_all(container).expect("cleanup");
}

#[test]
fn automatic_vanilla_setup_builds_cache_and_records_single_attempt() {
    let (root, _) = temp_workspace_dir();
    let source = root.join("library/Europa Universalis IV");
    for directory in pdx_game::eu4::INSTALL_DESCRIPTOR.validation_directories {
        fs::create_dir_all(source.join(directory)).expect("validation directory");
    }
    #[cfg(target_os = "windows")]
    let executable = source.join("eu4.exe");
    #[cfg(target_os = "linux")]
    let executable = source.join("eu4");
    #[cfg(target_os = "macos")]
    let executable = source.join("Europa Universalis IV.app/Contents/MacOS/eu4");
    fs::create_dir_all(executable.parent().expect("executable parent"))
        .expect("executable parent directory");
    fs::write(executable, b"fixture executable").expect("executable marker");
    fs::create_dir_all(source.join("common/events")).expect("indexed directory");
    fs::write(
        source.join("common/events/definitions.txt"),
        "country_event = { id = vanilla.1 }\n",
    )
    .expect("fixture source");
    let automatic = AutoVanillaConfiguration {
        descriptor: pdx_game::eu4::INSTALL_DESCRIPTOR,
        user_paths: UserPaths {
            config_file: root.join("user/config.toml"),
            cache_root: root.join("user/cache"),
        },
    };
    let options = DiscoveryOptions {
        roots: vec![root.join("library")],
        include_platform_locations: false,
        ..DiscoveryOptions::default()
    };
    let (cache, message) = run_auto_vanilla_setup_with_options(
        &automatic,
        pdx_game::eu4::first_party_rules().expect("rules"),
        pdx_game::eu4::profile(),
        None,
        &VanillaSetupCancellation::new(),
        &options,
    )
    .expect("automatic setup");
    assert!(message.contains("Vanilla symbols are now enabled"));
    assert_eq!(cache.metadata().game_id, "eu4");
    assert!(automatic.user_paths.vanilla_cache("eu4").is_file());
    let configuration =
        UserConfiguration::load(&automatic.user_paths.config_file).expect("configuration");
    let game = configuration.games.get("eu4").expect("EU4 configuration");
    assert!(game.auto_discovery_attempted);
    assert_eq!(game.discovery_outcome, Some(DiscoveryOutcome::Configured));

    let repeated = run_auto_vanilla_setup_with_options(
        &automatic,
        pdx_game::eu4::first_party_rules().expect("rules"),
        pdx_game::eu4::profile(),
        None,
        &VanillaSetupCancellation::new(),
        &options,
    )
    .expect_err("automatic setup only runs once");
    assert!(repeated.contains("already attempted"));
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn explicit_project_cache_precedes_user_discovery_configuration() {
    let (root, _) = temp_workspace_dir();
    let automatic = AutoVanillaConfiguration {
        descriptor: pdx_game::eu4::INSTALL_DESCRIPTOR,
        user_paths: UserPaths {
            config_file: root.join("user/config.toml"),
            cache_root: root.join("user/cache"),
        },
    };
    let mut configuration = UserConfiguration::default();
    let game = configuration.games.entry("eu4".to_owned()).or_default();
    game.auto_discovery_attempted = true;
    game.discovery_outcome = Some(DiscoveryOutcome::Configured);
    game.vanilla_cache = Some(root.join("user/cache/eu4/vanilla.pdxindex"));
    configuration
        .save(&automatic.user_paths.config_file)
        .expect("save user configuration");

    let project_cache = root.join("project/vanilla.pdxindex");
    let mut resolved = ResolvedSourceRoots {
        workspace_root: None,
        roots: Vec::new(),
        vanilla_cache: Some(project_cache.clone()),
        vanilla_explicit: true,
    };
    let mut warnings = Vec::new();
    let setup =
        apply_user_vanilla_configuration(&mut resolved, Some(&automatic), "eu4", &mut warnings);
    assert!(setup.is_none());
    assert_eq!(resolved.vanilla_cache, Some(project_cache));
    assert!(warnings.is_empty());
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn unsuccessful_automatic_discovery_is_recorded_and_not_repeated() {
    let (root, _) = temp_workspace_dir();
    let automatic = AutoVanillaConfiguration {
        descriptor: pdx_game::eu4::INSTALL_DESCRIPTOR,
        user_paths: UserPaths {
            config_file: root.join("user/config.toml"),
            cache_root: root.join("user/cache"),
        },
    };
    let options = DiscoveryOptions {
        roots: Vec::new(),
        include_platform_locations: false,
        ..DiscoveryOptions::default()
    };
    let first = run_auto_vanilla_setup_with_options(
        &automatic,
        pdx_game::eu4::first_party_rules().expect("rules"),
        pdx_game::eu4::profile(),
        None,
        &VanillaSetupCancellation::new(),
        &options,
    )
    .expect_err("empty search has no candidate");
    assert!(first.contains("was not found"));
    let configuration =
        UserConfiguration::load(&automatic.user_paths.config_file).expect("configuration");
    let game = configuration.games.get("eu4").expect("EU4 configuration");
    assert!(game.auto_discovery_attempted);
    assert_eq!(game.discovery_outcome, Some(DiscoveryOutcome::NotFound));

    let second = run_auto_vanilla_setup_with_options(
        &automatic,
        pdx_game::eu4::first_party_rules().expect("rules"),
        pdx_game::eu4::profile(),
        None,
        &VanillaSetupCancellation::new(),
        &options,
    )
    .expect_err("failed automatic search is not repeated");
    assert!(second.contains("already attempted"));
    fs::remove_dir_all(root).expect("cleanup");
}
