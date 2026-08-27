use std::fs;
use std::io::Cursor;

use serde_json::{Value, json};

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
        json!(["pdx/reindexWorkspace"])
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
