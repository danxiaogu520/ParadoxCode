use std::fs;
use std::io::Cursor;

use serde_json::{Value, json};

use crate::server::WATCHED_BULK_CAP;

use super::*;

#[test]
fn execute_reindex_workspace_refreshes_sources_and_reports_capability() {
    let (root, root_uri) = temp_workspace_dir();
    fs::create_dir_all(root.join("events")).expect("events directory");
    let source = root.join("events/reindexed.txt");
    let write_source = source.clone();
    let input = ScriptedReader::new([
        (
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "workspaceFolders": [{"uri": root_uri, "name": "test"}],
                    "capabilities": {}
                }
            }),
            None,
        ),
        (
            json!({"jsonrpc": "2.0", "method": "initialized", "params": {}}),
            None,
        ),
        (
            json!({
                "jsonrpc": "2.0",
                "method": "workspace/didChangeConfiguration",
                "params": {"settings": {}}
            }),
            Some(Box::new(move || {
                fs::write(write_source, "country_event = { id = reindexed.1 }\n")
                    .expect("write source before explicit reindex");
            }) as Box<dyn FnOnce() + Send>),
        ),
        (
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "workspace/executeCommand",
                "params": {"command": "pdx/reindexWorkspace", "arguments": []}
            }),
            None,
        ),
        (
            json!({"jsonrpc": "2.0", "id": 3, "method": "shutdown", "params": {}}),
            None,
        ),
        (json!({"jsonrpc": "2.0", "method": "exit"}), None),
    ]);
    let mut output = Vec::new();
    let mut server = eu4_server(InitializeOptions).expect("embedded rules");
    server.run_transport(input, &mut output).expect("transport");
    let responses = decode_frames(&output);

    let initialize = responses
        .iter()
        .find(|value| value["id"] == 1)
        .expect("initialize response");
    assert_eq!(
        initialize["result"]["capabilities"]["executeCommandProvider"]["commands"],
        json!(["pdx/reindexWorkspace", "validateWorkspace"])
    );

    let reindex = responses
        .iter()
        .find(|value| value["id"] == 2)
        .expect("reindex response");
    assert_eq!(reindex["error"], Value::Null);
    assert_eq!(reindex["result"]["sourceFiles"], 1);
    assert!(reindex["result"]["revision"].as_u64().unwrap_or(0) > 0);
    assert!(
        server
            .snapshot()
            .index()
            .active_definition("event", "reindexed.1")
            .is_some(),
        "explicit reindex installs the newly created source file"
    );
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn validate_workspace_returns_a_bounded_diagnostic_summary() {
    let (root, root_uri) = temp_workspace_dir();
    let events = root.join("events");
    fs::create_dir_all(&events).expect("events directory");
    fs::write(events.join("invalid.txt"), "scope = nowhere\n").expect("invalid source");
    let input = frames([
        json!({
            "jsonrpc":"2.0",
            "id":1,
            "method":"initialize",
            "params":{
                "workspaceFolders":[{"uri":root_uri,"name":"test"}],
                "capabilities":{}
            }
        }),
        json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
        json!({
            "jsonrpc":"2.0",
            "id":2,
            "method":"workspace/executeCommand",
            "params":{"command":"validateWorkspace","arguments":[]}
        }),
        json!({"jsonrpc":"2.0","id":3,"method":"shutdown","params":{}}),
        json!({"jsonrpc":"2.0","method":"exit"}),
    ]);
    let mut output = Vec::new();
    let mut server = eu4_server(InitializeOptions).expect("embedded rules");
    server
        .run_transport(Cursor::new(input), &mut output)
        .expect("transport");
    let responses = decode_frames(&output);
    let validation = responses
        .iter()
        .find(|value| value["id"] == 2)
        .expect("validateWorkspace response");
    assert_eq!(validation["error"], Value::Null);
    assert_eq!(validation["result"]["totalFiles"], 1);
    assert_eq!(validation["result"]["validatedFiles"], 1);
    assert_eq!(validation["result"]["filesWithErrors"], 1);
    assert!(validation["result"]["totalErrors"].as_u64().unwrap_or(0) > 0);
    let published = responses
        .iter()
        .find(|value| {
            value["method"] == "textDocument/publishDiagnostics"
                && value["params"]["uri"].as_str().is_some_and(|uri| {
                    uri.ends_with("/events/invalid.txt") || uri.ends_with("\\events\\invalid.txt")
                })
        })
        .expect("closed-file diagnostics publication");
    assert!(
        published["params"]["diagnostics"]
            .as_array()
            .is_some_and(|items| !items.is_empty())
    );
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn workspace_wide_diagnostics_can_be_disabled_without_changing_validation() {
    let (root, root_uri) = temp_workspace_dir();
    let events = root.join("events");
    fs::create_dir_all(&events).expect("events directory");
    fs::write(events.join("invalid.txt"), "scope = nowhere\n").expect("invalid source");
    let input = frames([
        json!({
            "jsonrpc":"2.0",
            "id":1,
            "method":"initialize",
            "params":{
                "workspaceFolders":[{"uri":root_uri,"name":"test"}],
                "capabilities":{},
                "initializationOptions":{"workspaceWideDiagnostics":false}
            }
        }),
        json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
        json!({
            "jsonrpc":"2.0",
            "id":2,
            "method":"workspace/executeCommand",
            "params":{"command":"validateWorkspace","arguments":[]}
        }),
        json!({"jsonrpc":"2.0","id":3,"method":"shutdown","params":{}}),
        json!({"jsonrpc":"2.0","method":"exit"}),
    ]);
    let mut output = Vec::new();
    let mut server = eu4_server(InitializeOptions).expect("embedded rules");
    server
        .run_transport(Cursor::new(input), &mut output)
        .expect("transport");
    let responses = decode_frames(&output);
    let validation = responses
        .iter()
        .find(|value| value["id"] == 2)
        .expect("validateWorkspace response");
    assert_eq!(validation["result"]["totalFiles"], 1);
    assert!(validation["result"]["totalErrors"].as_u64().unwrap_or(0) > 0);
    assert!(!responses.iter().any(|value| {
        value["method"] == "textDocument/publishDiagnostics"
            && value["params"]["uri"]
                .as_str()
                .is_some_and(|uri| uri.ends_with("/events/invalid.txt"))
    }));
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn initial_ready_pass_publishes_closed_current_mod_diagnostics() {
    let (root, root_uri) = temp_workspace_dir();
    let events = root.join("events");
    fs::create_dir_all(&events).expect("events directory");
    let source = events.join("initial-invalid.txt");
    fs::write(&source, "scope = nowhere\n").expect("invalid source");
    let input = frames([
        json!({
            "jsonrpc":"2.0",
            "id":1,
            "method":"initialize",
            "params":{
                "workspaceFolders":[{"uri":root_uri,"name":"test"}],
                "capabilities":{}
            }
        }),
        json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
        json!({"jsonrpc":"2.0","id":2,"method":"shutdown","params":{}}),
        json!({"jsonrpc":"2.0","method":"exit"}),
    ]);
    let mut output = Vec::new();
    let mut server = eu4_server(InitializeOptions).expect("embedded rules");
    server
        .run_transport(Cursor::new(input), &mut output)
        .expect("transport");
    let uri = canonical_uri(&source);
    let responses = decode_frames(&output);
    let published = responses
        .iter()
        .find(|value| {
            value["method"] == "textDocument/publishDiagnostics" && value["params"]["uri"] == uri
        })
        .expect("initial closed-file diagnostics publication");
    assert!(
        published["params"]["diagnostics"]
            .as_array()
            .is_some_and(|items| !items.is_empty())
    );
    fs::remove_dir_all(root).expect("cleanup");
}

/// Forces the initial scan to outlast the message flow so it completes after
/// `shutdown` was processed, then asserts the ready pass still publishes
/// closed-file diagnostics. Regression test: the pass used to be suppressed once
/// the server entered `ShuttingDown`, dropping the very publication the shutdown
/// drain had just waited for.
///
/// The `initialized` read is delayed past the initialize worker on purpose:
/// without the delay the message arrives while the server is still initializing,
/// gets deferred behind the handshake, and serializes the whole shutdown after
/// every publication — which never exercises the race this test pins down.
#[test]
fn ready_pass_publishes_when_scan_completes_after_shutdown() {
    let (root, root_uri) = temp_workspace_dir();
    let events = root.join("events");
    fs::create_dir_all(&events).expect("events directory");
    let source = events.join("post-shutdown-invalid.txt");
    fs::write(&source, "scope = nowhere\n").expect("invalid source");
    for index in 0..400 {
        // Sizing the bulk corpus so the initial scan reliably outlasts the
        // message flow is what forces the interleaving under test; small files
        // let the scan win the race on fast machines.
        let bulk = "a = 1\n".repeat(400);
        fs::write(events.join(format!("bulk-{index:03}.txt")), &bulk).expect("bulk source");
    }
    let wait_for_initialize: ReadAction = Some(Box::new(|| {
        std::thread::sleep(std::time::Duration::from_millis(150));
    }));
    let input = ScriptedReader::new([
        (
            json!({
                "jsonrpc":"2.0",
                "id":1,
                "method":"initialize",
                "params":{
                    "workspaceFolders":[{"uri":root_uri,"name":"test"}],
                    "capabilities":{}
                }
            }),
            None,
        ),
        (
            json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
            wait_for_initialize,
        ),
        (
            json!({"jsonrpc":"2.0","id":2,"method":"shutdown","params":{}}),
            None,
        ),
        (json!({"jsonrpc":"2.0","method":"exit"}), None),
    ]);
    let mut output = Vec::new();
    let mut server = eu4_server(InitializeOptions).expect("embedded rules");
    server.run_transport(input, &mut output).expect("transport");
    let uri = canonical_uri(&source);
    let responses = decode_frames(&output);
    // Vacuity guard: the pass under test only runs when `shutdown` was
    // processed while the initial scan was still in flight. If a faster
    // machine finishes the scan before the shutdown response is written, the
    // publication below would be produced by the ordinary Initialized path
    // and prove nothing — fail loudly so the corpus or delay gets retuned.
    let shutdown_at = responses
        .iter()
        .position(|value| value["id"] == 2 && value.get("result").is_some())
        .expect("shutdown response");
    let ready_at = responses
        .iter()
        .position(|value| value["method"] == "pdx/ready")
        .expect("pdx/ready notification after the initial scan");
    assert!(
        shutdown_at < ready_at,
        "initial scan completed before `shutdown` was processed; enlarge the bulk corpus or the `initialized` delay"
    );
    let published = responses
        .iter()
        .find(|value| {
            value["method"] == "textDocument/publishDiagnostics" && value["params"]["uri"] == uri
        })
        .expect("ready-pass publication after shutdown");
    assert!(
        published["params"]["diagnostics"]
            .as_array()
            .is_some_and(|items| !items.is_empty())
    );
    fs::remove_dir_all(root).expect("cleanup");
}

/// Regression test for the Linux CI deadlock: an overlay edit bumps the
/// workspace revision while the initial scan runs, and the scan reschedules
/// itself after `shutdown` was processed. The pending retry used to be
/// unspawnable during `ShuttingDown` while the shutdown drain waited on it
/// forever, wedging the event loop; the retry must now respawn during the
/// drain and still exit cleanly, with the overlay's diagnostics published.
///
/// Like the ready-pass test above, the `initialized` read is delayed past the
/// initialize worker so `didOpen`/`shutdown` are processed while the (bulk-slow)
/// initial scan is still in flight — the exact interleaving that wedged CI.
///
/// The client advertises `window.workDoneProgress` so the scan reports its
/// completion as a `$/progress` end frame; that frame is the only observable
/// the reschedule path still writes, and the ordering guard below uses it to
/// fail loudly if a faster machine lets the scan win the race.
#[test]
fn scan_reschedule_during_shutdown_still_exits_cleanly() {
    let (root, root_uri) = temp_workspace_dir();
    let events = root.join("events");
    fs::create_dir_all(&events).expect("events directory");
    let source = events.join("rescheduled-scan-overlay.txt");
    fs::write(&source, "scope = nowhere\n").expect("invalid source");
    for index in 0..400 {
        // Sizing the bulk corpus so the initial scan reliably outlasts the
        // message flow is what forces the interleaving under test; small files
        // let the scan win the race on fast machines.
        let bulk = "a = 1\n".repeat(400);
        fs::write(events.join(format!("bulk-{index:03}.txt")), &bulk).expect("bulk source");
    }
    let uri = canonical_uri(&source);
    let wait_for_initialize: ReadAction = Some(Box::new(|| {
        std::thread::sleep(std::time::Duration::from_millis(150));
    }));
    let input = ScriptedReader::new([
        (
            json!({
                "jsonrpc":"2.0",
                "id":1,
                "method":"initialize",
                "params":{
                    "workspaceFolders":[{"uri":root_uri,"name":"test"}],
                    "capabilities":{"window":{"workDoneProgress":true}}
                }
            }),
            None,
        ),
        (
            json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
            wait_for_initialize,
        ),
        (
            json!({"jsonrpc":"2.0","method":"textDocument/didOpen","params":{
                "textDocument":{"uri":uri,"languageId":"eu4","version":1,"text":"scope = nowhere\n"}
            }}),
            None,
        ),
        (
            json!({"jsonrpc":"2.0","id":2,"method":"shutdown","params":{}}),
            None,
        ),
        (json!({"jsonrpc":"2.0","method":"exit"}), None),
    ]);
    let mut output = Vec::new();
    let mut server = eu4_server(InitializeOptions).expect("embedded rules");
    server.run_transport(input, &mut output).expect("transport");
    let responses = decode_frames(&output);
    // Vacuity guard: the wedge only reproduced when `shutdown` was processed
    // before the initial scan finished; the scan's `$/progress` end frame is
    // written before the reschedule branch runs, so its position pins the
    // scan's completion relative to the shutdown response.
    let shutdown_at = responses
        .iter()
        .position(|value| value["id"] == 2 && value.get("result").is_some())
        .expect("shutdown response");
    let scan_end_at = responses
        .iter()
        .position(|value| {
            value["method"] == "$/progress"
                && value["params"]["token"]
                    .as_str()
                    .is_some_and(|token| token.starts_with("pdx-scan-"))
                && value["params"]["value"]["kind"] == "end"
        })
        .expect("scan completion progress frame");
    assert!(
        shutdown_at < scan_end_at,
        "initial scan completed before `shutdown` was processed; enlarge the bulk corpus or the `initialized` delay"
    );
    assert!(
        responses.iter().any(|value| {
            value["method"] == "textDocument/publishDiagnostics" && value["params"]["uri"] == uri
        }),
        "overlay diagnostics were never published"
    );
    fs::remove_dir_all(root).expect("cleanup");
}

/// Regression test for a Linux CI flake: while the initial scan was still
/// running, the shutdown-time force spawn published the overlay's diagnostics
/// and the scan commit then re-ran the same version "against the now-complete
/// index" — publishing a byte-identical batch a second time. Identical
/// revalidation results must not be republished; only changed batches are.
///
/// The setup mirrors the other shutdown-race tests: a bulk corpus keeps the
/// scan in flight past `shutdown`, and the ordering guard fails loudly on a
/// machine fast enough to let the scan win.
#[test]
fn identical_overlay_revalidation_is_not_republished_after_scan_commit() {
    let (root, root_uri) = temp_workspace_dir();
    let events = root.join("events");
    fs::create_dir_all(&events).expect("events directory");
    for index in 0..400 {
        let bulk = "a = 1\n".repeat(400);
        fs::write(events.join(format!("bulk-{index:03}.txt")), &bulk).expect("bulk source");
    }
    let source = events.join("overlay-fresh.txt");
    fs::write(&source, "scope = country\n").expect("overlay base source");
    let uri = canonical_uri(&source);
    let wait_for_initialize: ReadAction = Some(Box::new(|| {
        std::thread::sleep(std::time::Duration::from_millis(150));
    }));
    let input = ScriptedReader::new([
        (
            json!({
                "jsonrpc":"2.0",
                "id":1,
                "method":"initialize",
                "params":{
                    "workspaceFolders":[{"uri":root_uri,"name":"test"}],
                    "capabilities":{"window":{"workDoneProgress":true}}
                }
            }),
            None,
        ),
        (
            json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
            wait_for_initialize,
        ),
        (
            json!({"jsonrpc":"2.0","method":"textDocument/didOpen","params":{
                "textDocument":{"uri":uri,"languageId":"eu4","version":1,"text":"scope = nowhere\n"}
            }}),
            None,
        ),
        (
            json!({"jsonrpc":"2.0","method":"textDocument/didChange","params":{
                "textDocument":{"uri":uri,"version":2},
                "contentChanges":[{"text":"scope = country\n"}]
            }}),
            None,
        ),
        (
            json!({"jsonrpc":"2.0","id":2,"method":"shutdown","params":{}}),
            None,
        ),
        (json!({"jsonrpc":"2.0","method":"exit"}), None),
    ]);
    let mut output = Vec::new();
    let mut server = eu4_server(InitializeOptions).expect("embedded rules");
    server.run_transport(input, &mut output).expect("transport");
    let responses = decode_frames(&output);
    let shutdown_at = responses
        .iter()
        .position(|value| value["id"] == 2 && value.get("result").is_some())
        .expect("shutdown response");
    let scan_end_at = responses
        .iter()
        .position(|value| {
            value["method"] == "$/progress"
                && value["params"]["token"]
                    .as_str()
                    .is_some_and(|token| token.starts_with("pdx-scan-"))
                && value["params"]["value"]["kind"] == "end"
        })
        .expect("scan completion progress frame");
    assert!(
        shutdown_at < scan_end_at,
        "initial scan completed before `shutdown` was processed; enlarge the bulk corpus or the `initialized` delay"
    );
    let batches = responses
        .iter()
        .filter(|value| {
            value["method"] == "textDocument/publishDiagnostics" && value["params"]["uri"] == uri
        })
        .map(|value| value["params"]["diagnostics"].clone())
        .collect::<Vec<_>>();
    assert!(
        !batches.is_empty(),
        "overlay diagnostics were never published"
    );
    assert!(
        batches.windows(2).all(|pair| pair[0] != pair[1]),
        "an identical diagnostics batch was republished: {batches:?}"
    );
    assert!(
        batches.last().is_some_and(|latest| latest
            .as_array()
            .is_some_and(|items| { items.iter().all(|item| item["code"] != "UnknownScope") })),
        "the latest overlay text was not the published one: {batches:?}"
    );
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn watched_refresh_republishes_closed_file_diagnostics() {
    let (root, root_uri) = temp_workspace_dir();
    let events = root.join("events");
    fs::create_dir_all(&events).expect("events directory");
    let source = events.join("watched-diagnostics.txt");
    fs::write(&source, "scope = nowhere\n").expect("invalid source");
    let changed_source = source.clone();
    let source_uri = canonical_uri(&source);
    let input = ScriptedReader::new([
        (
            json!({
                "jsonrpc":"2.0",
                "id":1,
                "method":"initialize",
                "params":{
                    "workspaceFolders":[{"uri":root_uri,"name":"test"}],
                    "capabilities":{}
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
                "params":{"changes":[{"uri":source_uri,"type":2}]}
            }),
            Some(Box::new(move || {
                fs::write(changed_source, "").expect("write fixed source");
            }) as Box<dyn FnOnce() + Send>),
        ),
        (
            json!({"jsonrpc":"2.0","id":2,"method":"shutdown","params":{}}),
            None,
        ),
        (json!({"jsonrpc":"2.0","method":"exit"}), None),
    ]);
    let mut output = Vec::new();
    let mut server = eu4_server(InitializeOptions).expect("embedded rules");
    server.run_transport(input, &mut output).expect("transport");
    let responses = decode_frames(&output);
    let publications = responses
        .iter()
        .filter(|value| {
            value["method"] == "textDocument/publishDiagnostics"
                && value["params"]["uri"] == source_uri
        })
        .collect::<Vec<_>>();
    assert!(publications.len() >= 2, "initial and watched publications");
    assert!(
        publications
            .last()
            .and_then(|value| value["params"]["diagnostics"].as_array())
            .is_some_and(Vec::is_empty)
    );
    fs::remove_dir_all(root).expect("cleanup");
}

/// Regression test for a Linux CI flake in the same family as the ready-pass
/// one. A watched-file change pending while the initial scan ran meant the
/// post-ready workspace pass stayed queued behind the disk batch; two
/// independent holes then dropped it before `exit` ran. First, the
/// deferred-replay gate waited for the disk worker and the scan but not for
/// the pass their completions had queued, so the replayed `exit` skipped its
/// publication. Second, publishing the incremental watched-file batch
/// cleared the queued-pass flag even though that batch covers only the files
/// it touched. A deferred `exit` must wait for every worker the shutdown
/// drain waits on, and only a whole-workspace validation may consume the
/// queued pass.
#[test]
fn deferred_exit_waits_for_the_ready_pass_queued_behind_a_disk_change() {
    let (root, root_uri) = temp_workspace_dir();
    let events = root.join("events");
    fs::create_dir_all(&events).expect("events directory");
    let source = events.join("deferred-exit-refresh.txt");
    fs::write(&source, "scope = nowhere\n").expect("invalid source");
    let changed_source = source.clone();
    let source_uri = canonical_uri(&source);
    for index in 0..400 {
        // The bulk corpus keeps the initial scan in flight past
        // `shutdown`, which is what queues the ready pass behind the
        // pending watched-file change in the first place.
        let bulk = "a = 1\n".repeat(400);
        fs::write(events.join(format!("bulk-{index:03}.txt")), &bulk).expect("bulk source");
    }
    let wait_for_initialize: ReadAction = Some(Box::new(|| {
        std::thread::sleep(std::time::Duration::from_millis(150));
    }));
    let input = ScriptedReader::new([
        (
            json!({
                "jsonrpc":"2.0",
                "id":1,
                "method":"initialize",
                "params":{
                    "workspaceFolders":[{"uri":root_uri,"name":"test"}],
                    "capabilities":{"window":{"workDoneProgress":true}}
                }
            }),
            None,
        ),
        (
            json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
            wait_for_initialize,
        ),
        (
            json!({
                "jsonrpc":"2.0",
                "method":"workspace/didChangeWatchedFiles",
                "params":{"changes":[{"uri":source_uri,"type":2}]}
            }),
            Some(Box::new(move || {
                fs::write(changed_source, "").expect("write fixed source");
            }) as Box<dyn FnOnce() + Send>),
        ),
        (
            json!({"jsonrpc":"2.0","id":2,"method":"shutdown","params":{}}),
            None,
        ),
        (json!({"jsonrpc":"2.0","method":"exit"}), None),
    ]);
    let mut output = Vec::new();
    let mut server = eu4_server(InitializeOptions).expect("embedded rules");
    server.run_transport(input, &mut output).expect("transport");
    let responses = decode_frames(&output);
    let shutdown_at = responses
        .iter()
        .position(|value| value["id"] == 2 && value.get("result").is_some())
        .expect("shutdown response");
    let scan_end_at = responses
        .iter()
        .position(|value| {
            value["method"] == "$/progress"
                && value["params"]["token"]
                    .as_str()
                    .is_some_and(|token| token.starts_with("pdx-scan-"))
                && value["params"]["value"]["kind"] == "end"
        })
        .expect("scan completion progress frame");
    assert!(
        shutdown_at < scan_end_at,
        "initial scan completed before `shutdown` was processed; enlarge the bulk corpus or the `initialized` delay"
    );
    let publications = responses
        .iter()
        .filter(|value| {
            value["method"] == "textDocument/publishDiagnostics"
                && value["params"]["uri"] == source_uri
        })
        .count();
    assert!(
        publications >= 2,
        "the watched-file republication or the queued ready pass was skipped before `exit` ran"
    );
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn initialize_ignore_filters_are_applied_before_workspace_scan() {
    let (root, root_uri) = temp_workspace_dir();
    let events = root.join("events");
    fs::create_dir_all(&events).expect("events directory");
    fs::write(
        events.join("kept.txt"),
        "country_event = { id = filter.keep }\\n",
    )
    .expect("kept source");
    fs::write(
        events.join("ignored.txt"),
        "country_event = { id = filter.ignore }\\n",
    )
    .expect("ignored source");
    let input = frames([
        json!({
            "jsonrpc":"2.0",
            "id":1,
            "method":"initialize",
            "params":{
                "workspaceFolders":[{"uri":root_uri,"name":"test"}],
                "capabilities":{},
                "initializationOptions":{"ignoreFilePatterns":["ignored.txt"]}
            }
        }),
        json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
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
            .active_definition("event", "filter.keep")
            .is_some()
    );
    assert!(
        server
            .snapshot()
            .index()
            .active_definition("event", "filter.ignore")
            .is_none()
    );
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn watched_file_bulk_burst_falls_back_to_a_full_rescan() {
    let (root, root_uri) = temp_workspace_dir();
    let events = root.join("events");
    fs::create_dir_all(&events).expect("events directory");

    // Keep one file outside the watcher batch. A bulk rescan must discover its edit too; a
    // targeted pass over the 201 reported paths would leave this definition stale.
    let paths = (0..=WATCHED_BULK_CAP + 1)
        .map(|index| events.join(format!("burst-{index}.txt")))
        .collect::<Vec<_>>();
    for (index, path) in paths.iter().enumerate() {
        fs::write(
            path,
            format!("country_event = {{ id = burst-old-{index} }}\n"),
        )
        .expect("initial burst source");
    }
    let changed_path = paths[0].clone();
    let unreported_path = paths[WATCHED_BULK_CAP + 1].clone();
    let changes = paths[..=WATCHED_BULK_CAP]
        .iter()
        .map(|path| json!({"uri": canonical_uri(path), "type": 2}))
        .collect::<Vec<_>>();
    let input = ScriptedReader::new([
        (
            json!({
                "jsonrpc":"2.0",
                "id":1,
                "method":"initialize",
                "params":{
                    "workspaceFolders":[{"uri":root_uri,"name":"test"}],
                    "capabilities":{}
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
                "params":{"changes":changes}
            }),
            Some(Box::new(move || {
                fs::write(changed_path, "country_event = { id = burst-reported }\n")
                    .expect("reported burst edit");
                fs::write(
                    unreported_path,
                    "country_event = { id = burst-unreported }\n",
                )
                .expect("unreported burst edit");
            }) as Box<dyn FnOnce() + Send>),
        ),
        (
            json!({
                "jsonrpc":"2.0",
                "id":2,
                "method":"workspace/symbol",
                "params":{"query":"burst-unreported"}
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
    let symbols = typed_result::<Vec<lsp_types::SymbolInformation>>(&responses, 2);
    assert_eq!(symbols.len(), 1);
    assert_eq!(symbols[0].name, "burst-unreported");
    fs::remove_dir_all(root).expect("cleanup");
}
