//! Production LSP requests use the IR without a legacy semantic corpus.
use super::*;
use serde_json::json;
use std::io::Cursor;

#[test]
fn production_factory_and_requests_use_ir() {
    let mut server = LspServer::try_new_production(
        InitializeOptions,
        rules::RuleSet::empty(),
        game::eu4::profile(),
    )
    .expect("production server");
    let snapshot = server.snapshot();
    assert!(!snapshot.ir().schemas.is_empty());
    assert_eq!(snapshot.rules().rule_hash(), snapshot.ir().rule_hash());
    let (root, root_uri) = temp_workspace_dir();
    let input = frames([
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"workspaceFolders":[{"uri":root_uri,"name":"test"}],"capabilities":{}}}),
        json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
        json!({"jsonrpc":"2.0","id":2,"method":"pdc/textDiagnostics","params":{"files":[{"path":"events/ir.txt","text":"country_event = { id = ir.1 is_triggered_only = maybe mystery = yes option = { name = ir.option } }"}]}}),
        json!({"jsonrpc":"2.0","id":3,"method":"pdc/ruleSearch","params":{"key":"add_prestige","limit":2}}),
        json!({"jsonrpc":"2.0","id":4,"method":"shutdown","params":{}}),
        json!({"jsonrpc":"2.0","method":"exit"}),
    ]);
    let mut output = Vec::new();
    server
        .run_transport(Cursor::new(input), &mut output)
        .expect("transport");
    let responses = decode_frames(&output);
    let diagnostics =
        responses.iter().find(|value| value["id"] == 2).unwrap()["result"][0]["diagnostics"]
            .as_array()
            .unwrap();
    assert!(
        diagnostics
            .iter()
            .any(|value| value["code"] == "InvalidValue"),
        "{diagnostics:?}"
    );
    assert!(
        diagnostics
            .iter()
            .any(|value| value["code"] == "UnknownKey"),
        "{diagnostics:?}"
    );
    let entries = responses.iter().find(|value| value["id"] == 3).unwrap()["result"]["rules"]
        .as_array()
        .unwrap();
    assert!(!entries.is_empty());
    assert!(
        entries
            .iter()
            .all(|value| value["id"].as_str().unwrap().starts_with("ir:")
                && value["source"]["pointer"].is_string()),
        "{entries:?}"
    );
    std::fs::remove_dir_all(root).unwrap();
}
