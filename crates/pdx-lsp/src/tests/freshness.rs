use std::collections::HashMap;
use std::fs;
use std::io::Cursor;

use pdx_engine::TextChange;
use pdx_text::TextRange;
use serde_json::json;

use super::*;

#[test]
fn stale_diagnostics_do_not_replace_newer_results() {
    let mut server = eu4_server(InitializeOptions).expect("syntax-only server should initialize");
    let uri = "file:///tmp/diagnostics.txt";
    let id = pdx_engine::DocumentId::new(uri);
    server
        .host
        .open_document(id.clone(), 1, "key = value".to_owned(), None)
        .expect("open should succeed");
    assert!(server.commit_diagnostics(uri, 1, json!([{"message":"old"}])));
    server
        .host
        .apply_document_changes(
            &id,
            2,
            &[TextChange::ranged(
                TextRange::new(0, 3).expect("range"),
                "new",
            )],
        )
        .expect("change should succeed");
    assert!(!server.commit_diagnostics(uri, 1, json!([{"message":"stale"}])));
    assert_eq!(
        server.diagnostics(uri).expect("old result remains")[0]["message"],
        "old"
    );
    assert!(server.commit_diagnostics(uri, 2, json!([{"message":"new"}])));
    assert_eq!(
        server.diagnostics(uri).expect("new result accepted")[0]["message"],
        "new"
    );
}

#[test]
fn rapid_changes_debounce_and_publish_only_the_latest_diagnostics() {
    let uri = "file:///tmp/debounced-diagnostics.txt";
    let input = frames([
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"workspaceFolders":[{"uri":"file:///tmp","name":"test"}],"capabilities":{}}}),
        json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
        json!({"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":uri,"languageId":"eu4","version":1,"text":"scope = nowhere\n"}}}),
        json!({"jsonrpc":"2.0","method":"textDocument/didChange","params":{"textDocument":{"uri":uri,"version":2},"contentChanges":[{"text":"scope = country\n"}]}}),
        json!({"jsonrpc":"2.0","id":2,"method":"shutdown","params":{}}),
        json!({"jsonrpc":"2.0","method":"exit"}),
    ]);
    let mut output = Vec::new();
    let mut server = eu4_server(InitializeOptions).expect("server");

    server
        .run_transport(Cursor::new(input), &mut output)
        .expect("transport");

    let responses = decode_frames(&output);
    let published = responses
        .iter()
        .filter(|value| {
            value["method"] == "textDocument/publishDiagnostics" && value["params"]["uri"] == uri
        })
        .collect::<Vec<_>>();
    assert_eq!(published.len(), 1);
    assert!(
        published[0]["params"]["diagnostics"]
            .as_array()
            .is_some_and(|items| items.iter().all(|item| item["code"] != "InvalidValue"))
    );
    let snapshot = server.snapshot();
    let document = snapshot
        .document(&DocumentId::new(uri))
        .expect("latest overlay");
    assert_eq!(document.version(), Some(2));
    assert_eq!(document.text(), "scope = country\n");
    assert!(document.parsed().is_some());
    assert!(document.hir().is_some());
}

#[test]
fn initialization_can_filter_selected_diagnostic_codes() {
    let (root, root_uri) = temp_workspace_dir();
    let uri = format!("{root_uri}/events/filtered.txt");
    let input = frames([
        json!({
            "jsonrpc":"2.0",
            "id":1,
            "method":"initialize",
            "params":{
                "workspaceFolders":[{"uri":root_uri,"name":"test"}],
                "capabilities":{},
                "initializationOptions":{"ignoredErrorCodes":["InvalidValue"]}
            }
        }),
        json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
        json!({"jsonrpc":"2.0","method":"textDocument/didOpen","params":{
            "textDocument":{"uri":uri,"languageId":"eu4","version":1,"text":"scope = nowhere\n"}
        }}),
        json!({"jsonrpc":"2.0","id":2,"method":"shutdown","params":{}}),
        json!({"jsonrpc":"2.0","method":"exit"}),
    ]);
    let mut output = Vec::new();
    let mut server = eu4_server(InitializeOptions).expect("server");
    server
        .run_transport(Cursor::new(input), &mut output)
        .expect("transport");
    let responses = decode_frames(&output);
    let published = responses
        .iter()
        .find(|value| {
            value["method"] == "textDocument/publishDiagnostics" && value["params"]["uri"] == uri
        })
        .expect("diagnostic notification");
    assert!(
        published["params"]["diagnostics"]
            .as_array()
            .is_some_and(|items| items.iter().all(|item| item["code"] != "InvalidValue")),
        "ignored diagnostic code was published: {}",
        published["params"]["diagnostics"]
    );
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn inline_cwtools_ignore_filters_selected_diagnostic_codes() {
    let (root, root_uri) = temp_workspace_dir();
    let uri = format!("{root_uri}/events/inline-filtered.txt");
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
        json!({"jsonrpc":"2.0","method":"textDocument/didOpen","params":{
            "textDocument":{"uri":uri,"languageId":"eu4","version":1,"text":"scope = nowhere # cwtools-ignore InvalidValue\n"}
        }}),
        json!({"jsonrpc":"2.0","id":2,"method":"shutdown","params":{}}),
        json!({"jsonrpc":"2.0","method":"exit"}),
    ]);
    let mut output = Vec::new();
    let mut server = eu4_server(InitializeOptions).expect("server");
    server
        .run_transport(Cursor::new(input), &mut output)
        .expect("transport");
    let responses = decode_frames(&output);
    let published = responses
        .iter()
        .find(|value| {
            value["method"] == "textDocument/publishDiagnostics" && value["params"]["uri"] == uri
        })
        .expect("diagnostic notification");
    assert!(
        published["params"]["diagnostics"]
            .as_array()
            .is_some_and(|items| items.iter().all(|item| item["code"] != "InvalidValue")),
        "inline ignored diagnostic code was published: {}",
        published["params"]["diagnostics"]
    );
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn cancel_notification_marks_the_matching_in_flight_request() {
    let request_id = RequestId::String("active-query".to_owned());
    let cancellation = CancellationToken::new();
    let in_flight = HashMap::from([(
        request_id.clone(),
        InFlightRequest {
            cancellation: cancellation.clone(),
        },
    )]);

    cancel_request_from_notification(
        &json!({
            "jsonrpc": "2.0",
            "method": "$/cancelRequest",
            "params": {"id": "active-query"},
        }),
        &in_flight,
    );

    assert!(cancellation.is_cancelled());
    assert!(in_flight.contains_key(&request_id));
}

#[test]
fn initialize_cancellation_is_forwarded_and_a_retry_can_succeed() {
    let request_id = RequestId::Number(1);
    let scan_cancellation = pdx_engine::WorkspaceScanToken::new();
    let in_flight = InFlightInitialize {
        request_id: request_id.clone(),
        cancellation: scan_cancellation.clone(),
    };
    cancel_initialize_from_notification(
        &json!({
            "jsonrpc": "2.0",
            "method": "$/cancelRequest",
            "params": {"id": 1},
        }),
        Some(&in_flight),
    );
    assert!(scan_cancellation.is_cancelled());

    let input = frames([
        json!({"jsonrpc":"2.0","method":"$/cancelRequest","params":{"id":1}}),
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"workspaceFolders":[{"uri":"file:///tmp/cancelled","name":"test"}],"capabilities":{}}}),
        json!({"jsonrpc":"2.0","id":2,"method":"initialize","params":{"workspaceFolders":[{"uri":"file:///tmp/retry","name":"test"}],"capabilities":{}}}),
        json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
        json!({"jsonrpc":"2.0","id":3,"method":"shutdown","params":{}}),
        json!({"jsonrpc":"2.0","method":"exit"}),
    ]);
    let mut output = Vec::new();
    let mut server = eu4_server(InitializeOptions).expect("server");
    server
        .run_transport(Cursor::new(input), &mut output)
        .expect("transport");
    let responses = decode_frames(&output);

    assert_eq!(
        responses
            .iter()
            .find(|value| value["id"] == 1)
            .expect("cancelled initialize")["error"]["code"],
        super::REQUEST_CANCELLED
    );
    assert!(
        responses
            .iter()
            .find(|value| value["id"] == 2)
            .is_some_and(|value| value["result"]["capabilities"].is_object())
    );
    assert_eq!(server.state(), ServerState::Exited);
}
