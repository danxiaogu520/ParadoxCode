use std::io::Cursor;

use serde_json::{Value, json};

use super::*;

/// Runs a full initialize→shutdown session over the framed transport and
/// returns every written frame.
fn run_session(messages: Vec<Value>) -> (LspServer, Vec<Value>) {
    let (dir, uri) = temp_workspace_dir();
    let _ = dir;
    let script: Vec<Value> = std::iter::once(json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "workspaceFolders": [{"uri": uri, "name": "test"}],
            "capabilities": {},
        }
    }))
    .chain(messages)
    .chain([
        json!({"jsonrpc": "2.0", "id": 2, "method": "shutdown", "params": {}}),
        json!({"jsonrpc": "2.0", "method": "exit"}),
    ])
    .collect();
    let input = frames(script);
    let mut output = Vec::new();
    let mut server = eu4_server(InitializeOptions).expect("server");
    server
        .run_transport(Cursor::new(input), &mut output)
        .expect("transport");
    let responses = decode_frames(&output);
    (server, responses)
}

fn trace_messages(responses: &[Value]) -> Vec<&str> {
    responses
        .iter()
        .filter(|value| value["method"] == "pdc/trace")
        .filter_map(|value| value["params"]["message"].as_str())
        .collect()
}

#[test]
fn settrace_notification_is_silent_and_enables_decision_trace() {
    let (_, responses) = run_session(vec![
        json!({"jsonrpc": "2.0", "method": "initialized", "params": {}}),
        json!({"jsonrpc": "2.0", "method": "$/setTrace", "params": {"value": "verbose"}}),
        json!({"jsonrpc": "2.0", "method": "$/cancelRequest", "params": {"id": 99}}),
    ]);

    // `$/setTrace` is a notification: it must never produce an error response,
    // and the initialize/shutdown requests both succeed.
    assert!(
        responses.iter().all(|value| value.get("error").is_none()),
        "unexpected error responses: {responses:?}"
    );

    let traces = trace_messages(&responses);
    assert!(
        traces
            .iter()
            .any(|message| message.contains("cancel requested")),
        "expected a cancel decision trace, got {traces:?}"
    );
    // `$/setTrace` is replayed after the initialize worker finishes, so the
    // initialize completion itself predates verbose mode and must not appear.
    assert!(
        !traces
            .iter()
            .any(|message| message.contains("initialize finished")),
        "initialize finished before verbose was enabled; it must not be traced: {traces:?}"
    );
}

#[test]
fn decision_trace_stays_silent_without_verbose_client() {
    let (_, responses) = run_session(vec![
        json!({"jsonrpc": "2.0", "method": "initialized", "params": {}}),
        json!({"jsonrpc": "2.0", "method": "$/cancelRequest", "params": {"id": 99}}),
    ]);

    // The default experience must stay byte-identical: no decision frames at
    // all, so verbose gating can never regress into always-on noise.
    assert!(
        responses.iter().all(|value| value["method"] != "pdc/trace"),
        "unexpected pdc/trace frames without verbose tracing: {responses:?}"
    );
}

#[test]
fn initialize_trace_parameter_enables_decision_trace() {
    let (dir, uri) = temp_workspace_dir();
    let _ = dir;
    let input = frames([
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "workspaceFolders": [{"uri": uri, "name": "test"}],
                "capabilities": {},
                "trace": "verbose",
            }
        }),
        json!({"jsonrpc": "2.0", "method": "initialized", "params": {}}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "shutdown", "params": {}}),
        json!({"jsonrpc": "2.0", "method": "exit"}),
    ]);
    let mut output = Vec::new();
    let mut server = eu4_server(InitializeOptions).expect("server");
    server
        .run_transport(Cursor::new(input), &mut output)
        .expect("transport");
    let responses = decode_frames(&output);

    let traces = trace_messages(&responses);
    assert!(
        traces
            .iter()
            .any(|message| message.contains("scan committed")),
        "expected the background scan commit trace, got {traces:?}"
    );
    // Decision lines carry the request id so they correlate with frame ids.
    assert!(
        traces
            .iter()
            .any(|message| message.contains("accepted initialize id=1 (worker)")),
        "expected an id-tagged accepted trace, got {traces:?}"
    );
}

#[test]
fn settrace_without_value_keeps_previous_level() {
    let (_, responses) = run_session(vec![
        json!({"jsonrpc": "2.0", "method": "$/setTrace", "params": {}}),
        json!({"jsonrpc": "2.0", "method": "$/cancelRequest", "params": {"id": 7}}),
    ]);

    // A malformed $/setTrace is ignored (spec: servers may ignore it), so the
    // level stays "off" and no decision frames appear.
    assert!(
        responses.iter().all(|value| value["method"] != "pdc/trace"),
        "malformed setTrace must not enable tracing: {responses:?}"
    );
}

#[test]
fn document_diagnostics_lifecycle_is_traced() {
    let (dir, root_uri) = temp_workspace_dir();
    let _ = dir;
    let uri = format!("{root_uri}/events/lifecycle.txt");
    let script: Vec<Value> = vec![
        json!({"jsonrpc": "2.0", "method": "initialized", "params": {}}),
        json!({"jsonrpc": "2.0", "method": "$/setTrace", "params": {"value": "verbose"}}),
        json!({"jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {
            "textDocument": {
                "uri": uri,
                "languageId": "eu4",
                "version": 1,
                "text": "scope = nowhere\n",
            }
        }}),
    ];
    let input = frames(
        std::iter::once(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "workspaceFolders": [{"uri": root_uri, "name": "test"}],
                "capabilities": {},
            }
        }))
        .chain(script)
        .chain([
            json!({"jsonrpc": "2.0", "id": 2, "method": "shutdown", "params": {}}),
            json!({"jsonrpc": "2.0", "method": "exit"}),
        ]),
    );
    let mut output = Vec::new();
    let mut server = eu4_server(InitializeOptions).expect("server");
    server
        .run_transport(Cursor::new(input), &mut output)
        .expect("transport");
    let responses = decode_frames(&output);

    // The per-document round must be visible from spawn to publication: the
    // 0.3.5 burn was invisible precisely because this lifecycle had no lines.
    let traces = trace_messages(&responses);
    assert!(
        traces
            .iter()
            .any(|message| message.contains("document diagnostics started lifecycle.txt v1")),
        "expected a started decision trace, got {traces:?}"
    );
    assert!(
        traces
            .iter()
            .any(|message| message.contains("document diagnostics published lifecycle.txt v1")),
        "expected a published decision trace, got {traces:?}"
    );
}
