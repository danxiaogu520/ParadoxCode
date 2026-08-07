use std::fs;
use std::io::Cursor;

use pdx_rules::{RuleSet, RulesError, RulesModel};
use pdx_text::TextRange;
use serde_json::{Value, json};

use super::*;

#[test]
fn transport_framing_rejects_oversized_and_ambiguous_headers() {
    let oversized = format!(
        "Content-Length: {}\r\n\r\n",
        MAX_LSP_MESSAGE_BYTES.saturating_add(1)
    );
    assert!(matches!(
        read_message(&mut Cursor::new(oversized)),
        Err(LspError::Protocol(message)) if message.contains("safety limit")
    ));

    let duplicate = b"Content-Length: 2\r\nContent-Length: 2\r\n\r\n{}";
    assert!(matches!(
        read_message(&mut Cursor::new(duplicate)),
        Err(LspError::Protocol(message)) if message.contains("duplicate")
    ));

    let oversized_header = format!("X-Test: {}\r\n\r\n", "x".repeat(MAX_LSP_HEADER_BYTES));
    assert!(matches!(
        read_message(&mut Cursor::new(oversized_header)),
        Err(LspError::Protocol(message)) if message.contains("headers")
    ));
}

#[test]
fn document_changes_are_bounded_before_allocation() {
    assert_eq!(
        changed_document_len(0, None, MAX_DOCUMENT_BYTES).expect("boundary document"),
        MAX_DOCUMENT_BYTES
    );
    assert!(changed_document_len(0, None, MAX_DOCUMENT_BYTES + 1).is_err());
    assert!(
        changed_document_len(
            MAX_DOCUMENT_BYTES,
            Some(TextRange::new(0, 1).expect("range")),
            2,
        )
        .is_err()
    );
}

#[test]
fn ranked_result_limits_report_completion_truncation() {
    let (values, incomplete) = bounded_results(vec![0, 1, 2, 3], 3);
    assert_eq!(values, [0, 1, 2]);
    assert!(incomplete);
    let (values, incomplete) = bounded_results(vec![0, 1, 2], 3);
    assert_eq!(values, [0, 1, 2]);
    assert!(!incomplete);
    assert_eq!(diagnostic_result_counts(3, 3), (3, 0));
    assert_eq!(diagnostic_result_counts(4, 3), (2, 2));
}
#[test]
fn uri_round_trip_preserves_unicode_and_spaces() {
    let path = std::env::temp_dir().join("Paradox Code").join("汉.txt");
    let uri = path_to_uri(&path);
    assert!(uri.contains("%20"));
    assert_eq!(uri_to_path(&uri).expect("URI should decode"), path);
}

#[test]
fn selected_game_rejects_a_mismatched_rules_artifact() {
    let rules = RuleSet::from_model(RulesModel {
        game_id: "another-game".to_owned(),
        ..RulesModel::default()
    });

    let error = LspServer::try_new_with_rules(InitializeOptions, rules, pdx_game::eu4::profile())
        .expect_err("mismatched game must be rejected");
    assert!(matches!(
        error,
        LspError::Rules(RulesError::GameMismatch { expected, actual })
            if expected == "eu4" && actual == "another-game"
    ));
}

#[test]
fn memory_transport_runs_real_json_rpc_lifecycle_and_sync() {
    let path = std::env::temp_dir().join(format!("pdx-lsp-{}.txt", std::process::id()));
    fs::write(&path, "disk").expect("write disk fixture");
    let uri = path_to_uri(&path);
    let input = frames([
        json!({"jsonrpc":"2.0","id":1,"method":"shutdown","params":{}}),
        json!({"jsonrpc":"2.0","id":2,"method":"initialize","params":{"rootUri":uri,"capabilities":{}}}),
        json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
        json!({
            "jsonrpc":"2.0",
            "method":"textDocument/didOpen",
            "params":{"textDocument":{"uri":uri,"languageId":"eu4","version":1,"text":"a\r\n汉😀e\u{301}\r\n"}}
        }),
        json!({
            "jsonrpc":"2.0",
            "method":"textDocument/didChange",
            "params":{"textDocument":{"uri":uri,"version":2},"contentChanges":[{"range":{"start":{"line":1,"character":1},"end":{"line":1,"character":3}},"text":"猫"}]}
        }),
        json!({
            "jsonrpc":"2.0",
            "method":"textDocument/didChange",
            "params":{"textDocument":{"uri":uri,"version":1},"contentChanges":[{"text":"stale"}]}
        }),
        json!({"jsonrpc":"2.0","method":"$/cancelRequest","params":{"id":99}}),
        json!({"jsonrpc":"2.0","id":99,"method":"textDocument/hover","params":{}}),
        json!({
            "jsonrpc":"2.0",
            "method":"textDocument/didChange",
            "params":{"textDocument":{"uri":uri,"version":3},"contentChanges":[{"text":"current"}]}
        }),
        json!({"jsonrpc":"2.0","method":"textDocument/didClose","params":{"textDocument":{"uri":uri}}}),
        json!({"jsonrpc":"2.0","id":4,"method":"shutdown","params":{}}),
        json!({"jsonrpc":"2.0","method":"exit"}),
    ]);
    let mut output = Vec::new();
    let mut server = eu4_server(InitializeOptions).expect("syntax-only server should initialize");
    server
        .run_transport(Cursor::new(input), &mut output)
        .expect("transport should finish");

    let responses = decode_frames(&output);
    let before_initialize = responses
        .iter()
        .find(|value| value["id"] == 1)
        .expect("pre-init response");
    assert_eq!(before_initialize["error"]["code"], -32002);
    let initialize = responses
        .iter()
        .find(|value| value["id"] == 2)
        .expect("initialize response");
    assert_eq!(
        initialize["result"]["capabilities"]["textDocumentSync"]["change"],
        2
    );
    assert_eq!(
        initialize["result"]["capabilities"]["renameProvider"]["prepareProvider"],
        true
    );
    assert_eq!(
        initialize["result"]["capabilities"]["documentFormattingProvider"],
        true
    );
    let cancelled = responses
        .iter()
        .find(|value| value["id"] == 99)
        .expect("cancelled response");
    assert_eq!(cancelled["error"]["code"], -32800);
    let shutdown = responses
        .iter()
        .find(|value| value["id"] == 4)
        .expect("shutdown response");
    assert_eq!(shutdown["result"], Value::Null);
    assert!(
        responses
            .iter()
            .any(|value| value["method"] == "textDocument/publishDiagnostics")
    );
    let snapshot = server.snapshot();
    let document = snapshot
        .document(&pdx_engine::DocumentId::new(uri.clone()))
        .expect("close restores disk candidate");
    assert_eq!(document.text(), "disk");
    assert_eq!(document.version(), None);
    assert_eq!(server.state(), ServerState::Exited);
    fs::remove_file(path).expect("remove disk fixture");
}

#[test]
fn typed_protocol_rejects_malformed_params_without_corrupting_lifecycle() {
    let input = frames([
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"rootUri":"file:///tmp"}}),
        json!({"jsonrpc":"2.0","id":2,"method":"initialize","params":{"rootUri":"file:///tmp","capabilities":{}}}),
        json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
        json!({"jsonrpc":"2.0","id":3,"method":"textDocument/hover","params":{}}),
        json!({"jsonrpc":"2.0","id":4,"method":"shutdown","params":{}}),
        json!({"jsonrpc":"2.0","method":"exit"}),
    ]);
    let mut output = Vec::new();
    let mut server = eu4_server(InitializeOptions).expect("server");

    server
        .run_transport(Cursor::new(input), &mut output)
        .expect("transport");

    let responses = decode_frames(&output);
    let malformed_initialize = responses
        .iter()
        .find(|value| value["id"] == 1)
        .expect("invalid initialize");
    assert_eq!(malformed_initialize["error"]["code"], INVALID_PARAMS);
    assert!(
        malformed_initialize["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("invalid initialize params"))
    );
    assert!(
        responses
            .iter()
            .find(|value| value["id"] == 2)
            .is_some_and(|value| value["result"]["capabilities"].is_object())
    );
    let malformed_hover = responses
        .iter()
        .find(|value| value["id"] == 3)
        .expect("invalid hover");
    assert_eq!(malformed_hover["error"]["code"], INVALID_PARAMS);
    assert_eq!(server.state(), ServerState::Exited);
}
