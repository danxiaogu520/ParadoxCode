use std::fs;

use serde_json::{Value, json};

use super::*;

/// One whole `pdc/formatWorkspace` pass over a mixed Project workspace.
///
/// Formatting preserves each supported encoding and skips damaged input.
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
    let windows1252 = events.join("windows1252.txt");
    let utf8_bom = events.join("utf8-bom.txt");
    let utf16_le = events.join("utf16-le.txt");
    let utf16_be = events.join("utf16-be.txt");
    let invalid_encoding = events.join("invalid-encoding.txt");
    let localised = localisation.join("test_l_english.yml");
    let utf16 = |text: &str, little_endian: bool| {
        let units = std::iter::once(0xfeff).chain(text.encode_utf16());
        if little_endian {
            units.flat_map(u16::to_le_bytes).collect::<Vec<_>>()
        } else {
            units.flat_map(u16::to_be_bytes).collect::<Vec<_>>()
        }
    };
    let messy_unicode = "root = {\r\n  child=\"中文 🌍\"\r\n}\r\n";
    let formatted_unicode = "ROOT = { child = \"中文 🌍\" }\n";
    let invalid_bytes = b"\xef\xbb\xbfroot = { child = \"\xe9\" }\n";
    fs::write(&messy, "root = {\r\n  child = yes\r\n}\r\n").expect("messy source");
    fs::write(
        &paths,
        "sprite = {\r\n\ttexturefile = \"gfx\\\\interface\\\\alert.dds\"\r\n}\r\n",
    )
    .expect("asset-path source");
    fs::write(&canonical, "ROOT = { child = yes }\n").expect("canonical source");
    fs::write(&broken, "broken = \"unfinished").expect("broken source");
    fs::write(
        &windows1252,
        b"root = {\r\n  child=\"caf\xe9 \x80 \x93prix\x94\"\r\n}\r\n",
    )
    .expect("Windows-1252 source");
    fs::write(&utf8_bom, format!("\u{feff}{messy_unicode}")).expect("UTF-8 BOM source");
    fs::write(&utf16_le, utf16(messy_unicode, true)).expect("UTF-16LE source");
    fs::write(&utf16_be, utf16(messy_unicode, false)).expect("UTF-16BE source");
    fs::write(&invalid_encoding, invalid_bytes).expect("invalid UTF-8 BOM source");
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
    assert_eq!(format["result"]["totalFiles"], 9);
    assert_eq!(format["result"]["formattedFiles"], 6);
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
        fs::read(&windows1252).expect("Windows-1252 output"),
        b"ROOT = { child = \"caf\xe9 \x80 \x93prix\x94\" }\n"
    );
    assert_eq!(
        fs::read_to_string(&utf8_bom).unwrap(),
        format!("\u{feff}{formatted_unicode}")
    );
    assert_eq!(fs::read(&utf16_le).unwrap(), utf16(formatted_unicode, true));
    assert_eq!(
        fs::read(&utf16_be).unwrap(),
        utf16(formatted_unicode, false)
    );
    assert_eq!(fs::read(&invalid_encoding).unwrap(), invalid_bytes);
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
    let missions = root.join("missions");
    fs::create_dir_all(&missions).expect("missions directory");
    let source = missions.join("open-messy.txt");
    let original = "series = {\r\n  task = { required_missions={a b} }\r\n}\r\n";
    fs::write(&source, original).expect("messy source");
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
                                 "text": original}
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
        "series = {\n\ttask = {\n\t\trequired_missions = {\n\t\t\ta\n\t\t\tb\n\t\t}\n\t}\n}\n"
    );
    fs::remove_dir_all(root).expect("cleanup");
}
