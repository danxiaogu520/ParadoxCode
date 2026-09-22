use std::fs;

use serde_json::{Value, json};

use super::*;

/// One whole `pdc/formatWorkspace` pass over a mixed Project workspace.
///
/// The five script fixtures pin every summary bucket and the on-disk outcome:
/// a messy file is rewritten to canonical text, an asset-path file has its
/// separators normalized to forward slashes, an already-canonical file is
/// left untouched (unchanged), a syntactically broken file is refused
/// (unsafe skip, bytes preserved), and a legacy-encoded file is refused
/// (legacy skip, bytes preserved). The localisation file is out of scope by
/// design and must not be counted or touched.
#[test]
fn format_workspace_rewrites_scripts_and_reports_a_summary() {
    let (root, root_uri) = temp_workspace_dir();
    let events = root.join("events");
    let localisation = root.join("localisation");
    fs::create_dir_all(&events).expect("events directory");
    fs::create_dir_all(&localisation).expect("localisation directory");
    let messy = events.join("messy.txt");
    let paths = events.join("paths.txt");
    let canonical = events.join("canonical.txt");
    let broken = events.join("broken.txt");
    let legacy = events.join("legacy.txt");
    let localised = localisation.join("test_l_english.yml");
    fs::write(&messy, "root = {\r\n  child = yes\r\n}\r\n").expect("messy source");
    fs::write(
        &paths,
        "sprite = {\r\n\ttexturefile = \"gfx\\\\interface\\\\alert.dds\"\r\n}\r\n",
    )
    .expect("asset-path source");
    fs::write(&canonical, "ROOT = { child = yes }\n").expect("canonical source");
    fs::write(&broken, "broken = \"unfinished").expect("broken source");
    fs::write(&legacy, b"caf\xE9 = yes\n").expect("legacy-encoded source");
    fs::write(&localised, "l_english:\nkey:0 \"value\"\n").expect("localisation source");

    let input = frames([
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "workspaceFolders": [{"uri": root_uri, "name": "test"}],
                "capabilities": {"window": {"workDoneProgress": true}}
            }
        }),
        json!({"jsonrpc": "2.0", "method": "initialized", "params": {}}),
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "workspace/executeCommand",
            "params": {"command": "pdc/formatWorkspace", "arguments": []}
        }),
        json!({"jsonrpc": "2.0", "id": 3, "method": "shutdown", "params": {}}),
        json!({"jsonrpc": "2.0", "method": "exit"}),
    ]);
    let mut output = Vec::new();
    let mut server = eu4_server(InitializeOptions).expect("embedded rules");
    server
        .run_transport(input.as_slice(), &mut output)
        .expect("transport");
    let responses = decode_frames(&output);

    let initialize = responses
        .iter()
        .find(|value| value["id"] == 1)
        .expect("initialize response");
    assert_eq!(
        initialize["result"]["capabilities"]["executeCommandProvider"]["commands"],
        json!([
            "pdc/reindexWorkspace",
            "validateWorkspace",
            "pdc/formatWorkspace"
        ])
    );

    let format = responses
        .iter()
        .find(|value| value["id"] == 2)
        .expect("format response");
    assert_eq!(format["error"], Value::Null);
    assert_eq!(format["result"]["totalFiles"], 5);
    assert_eq!(format["result"]["formattedFiles"], 2);
    assert_eq!(format["result"]["unchangedFiles"], 1);
    assert_eq!(format["result"]["skippedUnsafeFiles"], 1);
    assert_eq!(format["result"]["skippedLegacyEncodingFiles"], 1);
    assert_eq!(format["result"]["failedFiles"], 0);

    assert_eq!(
        fs::read_to_string(&messy).expect("messy survives"),
        "ROOT = { child = yes }\n"
    );
    assert_eq!(
        fs::read_to_string(&paths).expect("asset-path file survives"),
        "sprite = { texturefile = \"gfx/interface/alert.dds\" }\n"
    );
    assert_eq!(
        fs::read_to_string(&canonical).expect("canonical survives"),
        "ROOT = { child = yes }\n"
    );
    assert_eq!(
        fs::read(&broken).expect("broken survives"),
        b"broken = \"unfinished"
    );
    assert_eq!(
        fs::read(&legacy).expect("legacy survives"),
        b"caf\xE9 = yes\n"
    );
    assert_eq!(
        fs::read_to_string(&localised).expect("localisation survives"),
        "l_english:\nkey:0 \"value\"\n"
    );

    // The whole pass shares one work-done-progress token: a begin frame at
    // spawn and an end frame when the worker completes.
    assert!(responses.iter().any(|value| {
        value["method"] == "$/progress"
            && value["params"]["token"]
                .as_str()
                .is_some_and(|token| token.starts_with("pdc-format-"))
            && value["params"]["value"]["kind"] == "begin"
    }));
    assert!(responses.iter().any(|value| {
        value["method"] == "$/progress"
            && value["params"]["token"]
                .as_str()
                .is_some_and(|token| token.starts_with("pdc-format-"))
            && value["params"]["value"]["kind"] == "end"
    }));
    fs::remove_dir_all(root).expect("cleanup");
}

/// An overlay document opened from the workspace still gets its disk bytes
/// rewritten: the client saves before invoking the command, so the on-disk
/// file is the authoritative text and the watcher pipeline re-syncs the index
/// afterwards.
#[test]
fn format_workspace_rewrites_files_backing_open_documents() {
    let (root, root_uri) = temp_workspace_dir();
    let events = root.join("events");
    fs::create_dir_all(&events).expect("events directory");
    let source = events.join("open-messy.txt");
    fs::write(&source, "root = {\r\n  child = yes\r\n}\r\n").expect("messy source");
    let uri = canonical_uri(&source);
    let input = frames([
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "workspaceFolders": [{"uri": root_uri, "name": "test"}],
                "capabilities": {}
            }
        }),
        json!({"jsonrpc": "2.0", "method": "initialized", "params": {}}),
        json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {"uri": uri, "languageId": "eu4", "version": 1,
                                 "text": "root = {\r\n  child = yes\r\n}\r\n"}
            }
        }),
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "workspace/executeCommand",
            "params": {"command": "pdc/formatWorkspace", "arguments": []}
        }),
        json!({"jsonrpc": "2.0", "id": 3, "method": "shutdown", "params": {}}),
        json!({"jsonrpc": "2.0", "method": "exit"}),
    ]);
    let mut output = Vec::new();
    let mut server = eu4_server(InitializeOptions).expect("embedded rules");
    server
        .run_transport(input.as_slice(), &mut output)
        .expect("transport");
    let responses = decode_frames(&output);
    let format = responses
        .iter()
        .find(|value| value["id"] == 2)
        .expect("format response");
    assert_eq!(format["error"], Value::Null);
    assert_eq!(format["result"]["formattedFiles"], 1);
    assert_eq!(
        fs::read_to_string(&source).expect("overlay-backed file survives"),
        "ROOT = { child = yes }\n"
    );
    fs::remove_dir_all(root).expect("cleanup");
}
