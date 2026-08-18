use std::fs;
use std::io::Cursor;

use lsp_types::{
    CompletionItem, CompletionResponse, Diagnostic, DocumentSymbol, Hover, Location,
    PrepareRenameResponse, SymbolInformation, SymbolKind, WorkspaceEdit,
};
use pdx_text::{LineIndex, Position};
use serde_json::{Value, json};

use super::*;

#[test]
fn classify_paths_uses_profile_whitelist_and_diagnostic_parser_categories() {
    let (root, root_uri) = temp_workspace_dir();
    let input = frames([
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"rootUri":root_uri,"capabilities":{}}}),
        json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
        json!({"jsonrpc":"2.0","id":2,"method":"pdx/classifyPaths","params":{"paths":["events/test.txt","localisation/test_l_english.yml","ThirdPartyLicenses.txt","interface/test.gui"]}}),
        json!({"jsonrpc":"2.0","id":3,"method":"shutdown","params":{}}),
        json!({"jsonrpc":"2.0","method":"exit"}),
    ]);
    let mut output = Vec::new();
    let mut server = eu4_server(InitializeOptions).expect("embedded rules");
    server
        .run_transport(Cursor::new(input), &mut output)
        .expect("transport");
    let responses = decode_frames(&output);
    let classified = responses
        .iter()
        .find(|value| value["id"] == 2)
        .expect("classification response");
    assert_eq!(
        classified["result"],
        json!(["events/test.txt", "localisation/test_l_english.yml"])
    );
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn text_diagnostics_analyzes_caller_supplied_files_without_opening_overlays() {
    let (root, root_uri) = temp_workspace_dir();
    let input = frames([
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"rootUri":root_uri,"capabilities":{}}}),
        json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
        json!({"jsonrpc":"2.0","id":2,"method":"pdx/textDiagnostics","params":{"files":[
            {"path":"events/invalid.txt","text":"country_event = { id = text.1 scope = nowhere }\n"},
            {"path":"events/valid.txt","text":"country_event = { id = text.2 }\n"}
        ]}}),
        json!({"jsonrpc":"2.0","id":3,"method":"shutdown","params":{}}),
        json!({"jsonrpc":"2.0","method":"exit"}),
    ]);
    let mut output = Vec::new();
    let mut server = eu4_server(InitializeOptions).expect("embedded rules");
    server
        .run_transport(Cursor::new(input), &mut output)
        .expect("transport");
    let responses = decode_frames(&output);
    let response = responses
        .iter()
        .find(|value| value["id"] == 2)
        .expect("text diagnostics response");
    let files = response["result"].as_array().expect("file results");
    assert_eq!(files.len(), 2);
    assert_eq!(files[0]["path"], "events/invalid.txt");
    assert!(
        files[0]["diagnostics"]
            .as_array()
            .is_some_and(|items| items.iter().any(|item| item["code"] == "pdx-unknown-scope"))
    );
    assert_eq!(files[1]["path"], "events/valid.txt");
    assert!(files[1]["diagnostics"].is_array());
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn mission_preview_returns_renderer_ready_tree_data() {
    let (root, root_uri) = temp_workspace_dir();
    let input = frames([
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"rootUri":root_uri,"capabilities":{}}}),
        json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
        json!({"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":format!("{root_uri}/localisation/titles_l_english.yml"),"languageId":"eu4","version":1,"text":"l_english:\n a1_title:0 \"Alpha One\"\n a2_title:0 \"Alpha Two\"\n"}}}),
        json!({"jsonrpc":"2.0","id":2,"method":"pdx/missionPreview","params":{"path":"missions/test.txt","text":"main_tree = {\n\tslot = 1\n\ta1 = { position = 1 icon = mission_alpha }\n\ta2 = { position = 2 required_missions = { a1 } }\n}\n\nbranch_tree = {\n\tslot = 2\n\tb1 = { position = 1 required_missions = { external_id } }\n}\n"}}),
        json!({"jsonrpc":"2.0","id":3,"method":"shutdown","params":{}}),
        json!({"jsonrpc":"2.0","method":"exit"}),
    ]);
    let mut output = Vec::new();
    let mut server = eu4_server(InitializeOptions).expect("embedded rules");
    server
        .run_transport(Cursor::new(input), &mut output)
        .expect("transport");
    let responses = decode_frames(&output);
    let response = responses
        .iter()
        .find(|value| value["id"] == 2)
        .expect("mission preview response");
    let nodes = response["result"]["nodes"].as_array().expect("nodes");
    assert_eq!(nodes.len(), 3);
    // Without a game installation the texture table is empty but present, and
    // every node/arrow still carries its sprite identity for textured runs.
    assert!(
        response["result"]["textures"]
            .as_object()
            .is_some_and(|map| map.is_empty()),
        "textures must be an empty object without a game directory"
    );
    let arrows = response["result"]["arrows"].as_array().expect("arrows");
    assert!(
        arrows.iter().all(|arrow| arrow["texture"].is_string()),
        "every arrow must expose its sprite name"
    );
    let a1 = nodes
        .iter()
        .find(|node| node["id"] == "a1")
        .expect("a1 node");
    assert!(a1["isRoot"] == true);
    assert!(a1["hasError"] == false);
    assert_eq!(a1["icon"], "mission_alpha");
    assert_eq!(a1["x"], 16.0);
    assert_eq!(a1["y"], 56.0);
    let a2 = nodes
        .iter()
        .find(|node| node["id"] == "a2")
        .expect("a2 node");
    let start = a2["start"].as_u64().expect("a2 start");
    let end = a2["end"].as_u64().expect("a2 end");
    assert!(start < end, "mission block carries a real byte span");
    assert_eq!(a2["required"], json!(["a1"]), "edges reachable from ids");
    // Mission titles resolve from the open localisation overlay via
    // active workspace definitions; unknown keys fall back to the raw id.
    assert_eq!(a1["titleKey"], "a1_title");
    assert_eq!(a1["title"]["value"], "Alpha One");
    assert_eq!(a1["title"]["language"], "l_english");
    let a2_title = nodes
        .iter()
        .find(|node| node["id"] == "a2")
        .expect("a2 node");
    assert_eq!(a2_title["title"]["value"], "Alpha Two");
    let b1 = nodes
        .iter()
        .find(|node| node["id"] == "b1")
        .expect("b1 node");
    assert_eq!(b1["titleKey"], "b1_title");
    assert!(b1["title"].is_null(), "missing localisation key stays null");
    let arrows = response["result"]["arrows"].as_array().expect("arrows");
    assert_eq!(arrows.len(), 2, "a1->a2 vertical tiles plus end");
    assert!(arrows.iter().any(|a| a["glyph"] == "end"));
    let groups = response["result"]["groups"].as_array().expect("groups");
    assert_eq!(groups.len(), 2);
    // Cross-file prerequisite surfaces as an external stub, not a node.
    let external = response["result"]["external"].as_array().expect("external");
    assert_eq!(external.len(), 1);
    assert_eq!(external[0]["label"], "external_id");
    // The dangling cross-file reference is also reported as a diagnostic.
    let diagnostics = response["result"]["diagnostics"]
        .as_array()
        .expect("diagnostics");
    assert!(
        diagnostics
            .iter()
            .any(|d| d["mission"] == "b1" && d["severity"] == 1)
    );
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn workspace_diagnostics_batches_indexed_disk_files_without_opening_overlays() {
    let (root, root_uri) = temp_workspace_dir();
    let events = root.join("events");
    fs::create_dir_all(&events).expect("events directory");
    fs::write(
        events.join("a.txt"),
        "country_event = { id = batch.1 scope = nowhere }\n",
    )
    .expect("first source");
    fs::write(events.join("b.txt"), "country_event = { id = batch.2 }\n").expect("second source");
    let input = frames([
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"rootUri":root_uri,"capabilities":{}}}),
        json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
        json!({"jsonrpc":"2.0","id":2,"method":"pdx/workspaceDiagnostics","params":{"offset":0,"limit":1}}),
        json!({"jsonrpc":"2.0","id":3,"method":"pdx/workspaceDiagnostics","params":{"offset":1,"limit":1}}),
        json!({"jsonrpc":"2.0","id":4,"method":"shutdown","params":{}}),
        json!({"jsonrpc":"2.0","method":"exit"}),
    ]);
    let mut output = Vec::new();
    let mut server = eu4_server(InitializeOptions).expect("embedded rules");
    server
        .run_transport(Cursor::new(input), &mut output)
        .expect("transport");
    let responses = decode_frames(&output);
    let first = responses
        .iter()
        .find(|value| value["id"] == 2)
        .expect("first batch");
    assert_eq!(first["result"]["total"], 2);
    assert_eq!(first["result"]["nextOffset"], 1);
    assert_eq!(first["result"]["items"][0]["logicalPath"], "events/a.txt");
    assert!(
        first["result"]["items"][0]["diagnostics"]
            .as_array()
            .is_some_and(|items| items.iter().any(|item| item["code"] == "pdx-unknown-scope"))
    );
    let second = responses
        .iter()
        .find(|value| value["id"] == 3)
        .expect("second batch");
    assert!(second["result"]["nextOffset"].is_null());
    assert_eq!(second["result"]["items"][0]["logicalPath"], "events/b.txt");
    assert!(second["result"]["items"][0]["diagnostics"].is_array());
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn noncanonical_document_uri_preserves_rule_path_context() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let container = std::env::temp_dir().join(format!("pdx-lsp-path-context-{nonce}"));
    let root = container.join("workspace");
    let decrees = root.join("common/decrees");
    fs::create_dir_all(&decrees).expect("decrees directory");
    fs::create_dir_all(container.join("detour")).expect("detour directory");
    let file = decrees.join("test.txt");
    fs::write(&file, "my_decree = { cost = 50 }\n").expect("decree source");

    let aliased_root = container.join("detour/../workspace");
    let aliased_file = aliased_root.join("common/decrees/test.txt");
    let root_uri = path_to_uri(&aliased_root);
    let uri = path_to_uri(&aliased_file);
    let input = frames([
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"rootUri":root_uri,"capabilities":{}}}),
        json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
        json!({"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":uri,"languageId":"eu4","version":1,"text":"my_decree = { cost = 50 }\n"}}}),
        json!({"jsonrpc":"2.0","id":2,"method":"textDocument/hover","params":{"textDocument":{"uri":uri},"position":{"line":0,"character":16}}}),
        json!({"jsonrpc":"2.0","id":3,"method":"shutdown","params":{}}),
        json!({"jsonrpc":"2.0","method":"exit"}),
    ]);
    let mut output = Vec::new();
    let mut server = eu4_server(InitializeOptions).expect("embedded rules");
    server
        .run_transport(Cursor::new(input), &mut output)
        .expect("transport");
    let responses = decode_frames(&output);
    let hover = responses
        .iter()
        .find(|value| value["id"] == 2)
        .expect("hover response");
    assert!(
        hover["result"]["contents"]["value"]
            .as_str()
            .is_some_and(|contents| contents.contains("Cost in meritocracy of enacting")),
        "hover response={hover}"
    );
    fs::remove_dir_all(container).expect("cleanup");
}

#[test]
fn memory_transport_hover_returns_semantic_value_and_null_for_unknown_text() {
    let (root, root_uri) = temp_workspace_dir();
    let directory = root.join("common/decrees");
    fs::create_dir_all(&directory).expect("decrees directory");
    let file = directory.join("test.txt");
    let uri = canonical_uri(&file);
    let text = "my_decree = { cost = 50 unknown = yes }\n";
    fs::write(&file, text).expect("decree source");
    let cost = text.find("cost").expect("cost") + 1;
    let unknown = text.find("unknown").expect("unknown") + 1;
    let input = frames([
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"rootUri":root_uri,"capabilities":{}}}),
        json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
        json!({"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":uri,"languageId":"eu4","version":1,"text":text}}}),
        json!({"jsonrpc":"2.0","id":2,"method":"textDocument/hover","params":{"textDocument":{"uri":uri},"position":{"line":0,"character":cost}}}),
        json!({"jsonrpc":"2.0","id":3,"method":"textDocument/hover","params":{"textDocument":{"uri":uri},"position":{"line":0,"character":unknown}}}),
        json!({"jsonrpc":"2.0","id":4,"method":"shutdown","params":{}}),
        json!({"jsonrpc":"2.0","method":"exit"}),
    ]);
    let mut output = Vec::new();
    let mut server = eu4_server(InitializeOptions).expect("embedded rules");
    server
        .run_transport(Cursor::new(input), &mut output)
        .expect("transport");
    let responses = decode_frames(&output);
    let semantic_hover = responses
        .iter()
        .find(|value| value["id"] == 2)
        .expect("semantic hover response");
    let contents = semantic_hover["result"]["contents"]["value"]
        .as_str()
        .expect("semantic hover markdown");
    assert!(contents.contains("PDX property `cost`"));
    assert!(contents.contains("- value:"));
    assert!(!contents.contains("Provenance"));
    let unknown_hover = responses
        .iter()
        .find(|value| value["id"] == 3)
        .expect("unknown hover response");
    assert_eq!(unknown_hover["result"], Value::Null);
    let _: Hover = typed_result(&responses, 2);
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn memory_transport_completes_macro_argument_values_from_body_constraints() {
    let (root_dir, root_uri) = temp_workspace_dir();
    let effects_dir = root_dir.join("common/scripted_effects");
    fs::create_dir_all(&effects_dir).expect("create scripted effects directory");
    fs::write(
        effects_dir.join("00_complete.txt"),
        "bool_macro = { set_primitive = $VALUE$ }\n",
    )
    .expect("scripted effect definition");
    let events_dir = root_dir.join("events");
    fs::create_dir_all(&events_dir).expect("create events directory");
    let file_path = events_dir.join("macro-completion.txt");
    fs::write(&file_path, "").expect("create placeholder file");
    let uri = canonical_uri(&file_path);
    let text = "country_event = { immediate = { bool_macro = { VALUE =  } } }\n";
    let position = text.find("=  }").expect("empty value") + 2;
    let input = frames([
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"rootUri":root_uri,"capabilities":{}}}),
        json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
        json!({"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":uri,"languageId":"eu4","version":1,"text":text}}}),
        json!({"jsonrpc":"2.0","id":2,"method":"textDocument/completion","params":{"textDocument":{"uri":uri},"position":{"line":0,"character":position}}}),
        json!({"jsonrpc":"2.0","id":3,"method":"shutdown","params":{}}),
        json!({"jsonrpc":"2.0","method":"exit"}),
    ]);
    let mut output = Vec::new();
    let mut server = eu4_server(InitializeOptions).expect("embedded rules");
    server
        .run_transport(Cursor::new(input), &mut output)
        .expect("transport");
    let responses = decode_frames(&output);
    let completion = responses
        .iter()
        .find(|value| value["id"] == 2)
        .expect("completion response");
    let items = completion["result"]["items"]
        .as_array()
        .expect("completion items");
    assert!(items.iter().any(|item| item["label"] == "yes"), "{items:?}");
    assert!(items.iter().any(|item| item["label"] == "no"), "{items:?}");
    let _: CompletionResponse = typed_result(&responses, 2);
    fs::remove_dir_all(root_dir).expect("cleanup");
}

#[test]
fn memory_transport_delegates_phase5_requests_to_analysis() {
    let (root_dir, root_uri) = temp_workspace_dir();
    let events_dir = root_dir.join("events");
    fs::create_dir_all(&events_dir).expect("create events dir");
    let file_path = events_dir.join("phase5.txt");
    fs::write(&file_path, "").expect("create placeholder file");
    let uri = canonical_uri(&file_path);
    let text = "country_event = { id = test.1 }\nevent = test.1\nscope = nowhere\n";
    let input = frames([
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"rootUri":root_uri,"capabilities":{}}}),
        json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
        json!({"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":uri,"languageId":"eu4","version":1,"text":text}}}),
        json!({"jsonrpc":"2.0","id":2,"method":"textDocument/completion","params":{"textDocument":{"uri":uri},"position":{"line":0,"character":19}}}),
        json!({"jsonrpc":"2.0","id":3,"method":"textDocument/hover","params":{"textDocument":{"uri":uri},"position":{"line":1,"character":8}}}),
        json!({"jsonrpc":"2.0","id":4,"method":"textDocument/definition","params":{"textDocument":{"uri":uri},"position":{"line":1,"character":8}}}),
        json!({"jsonrpc":"2.0","id":5,"method":"textDocument/references","params":{"textDocument":{"uri":uri},"position":{"line":1,"character":8},"context":{"includeDeclaration":true}}}),
        json!({"jsonrpc":"2.0","id":6,"method":"textDocument/documentSymbol","params":{"textDocument":{"uri":uri}}}),
        json!({"jsonrpc":"2.0","id":7,"method":"workspace/symbol","params":{"query":"test"}}),
        json!({"jsonrpc":"2.0","id":9,"method":"textDocument/prepareRename","params":{"textDocument":{"uri":uri},"position":{"line":1,"character":8}}}),
        json!({"jsonrpc":"2.0","id":10,"method":"textDocument/rename","params":{"textDocument":{"uri":uri},"position":{"line":1,"character":8},"newName":"renamed.1"}}),
        json!({"jsonrpc":"2.0","id":8,"method":"shutdown","params":{}}),
        json!({"jsonrpc":"2.0","method":"exit"}),
    ]);
    let mut output = Vec::new();
    let mut server = eu4_server(InitializeOptions).expect("syntax-only server should initialize");
    server
        .run_transport(Cursor::new(input), &mut output)
        .expect("transport should finish");
    let responses = decode_frames(&output);
    let completion = responses
        .iter()
        .find(|value| value["id"] == 2)
        .expect("completion response");
    assert!(
        completion["result"]["items"]
            .as_array()
            .is_some_and(|items| !items.is_empty())
    );
    assert!(
        responses
            .iter()
            .find(|value| value["id"] == 3)
            .is_some_and(|value| value["result"]["contents"].is_object())
    );
    let definition = responses
        .iter()
        .find(|value| value["id"] == 4)
        .expect("definition");
    assert_eq!(definition["result"].as_array().map(Vec::len), Some(1));
    assert_eq!(definition["result"][0]["range"]["start"]["line"], 0);
    assert_eq!(definition["result"][0]["range"]["start"]["character"], 23);
    assert_eq!(definition["result"][0]["range"]["end"]["character"], 29);
    assert_eq!(
        responses
            .iter()
            .find(|value| value["id"] == 5)
            .expect("references")["result"]
            .as_array()
            .map(Vec::len),
        Some(2)
    );
    // With embedded EU4 rules, top-level keys (country_event, event, scope) all
    // produce document symbols — richer than the identity-only baseline.
    assert!(
        responses
            .iter()
            .find(|value| value["id"] == 6)
            .expect("document symbols")["result"]
            .as_array()
            .is_some_and(|symbols| !symbols.is_empty())
    );
    assert_eq!(
        responses
            .iter()
            .find(|value| value["id"] == 7)
            .expect("workspace symbols")["result"]
            .as_array()
            .map(Vec::len),
        Some(1)
    );
    let prepare = responses
        .iter()
        .find(|value| value["id"] == 9)
        .expect("prepare rename");
    assert_eq!(prepare["result"]["placeholder"], "test.1");
    let rename = responses
        .iter()
        .find(|value| value["id"] == 10)
        .expect("rename");
    assert_eq!(
        rename["result"]["changes"][uri.clone()]
            .as_array()
            .map(Vec::len),
        Some(2)
    );
    assert!(
        rename["result"]["changes"][uri]
            .as_array()
            .is_some_and(|edits| { edits.iter().all(|edit| edit["newText"] == "renamed.1") })
    );
    let diagnostics = responses
        .iter()
        .find(|value| value["method"] == "textDocument/publishDiagnostics")
        .expect("diagnostic notification");
    assert!(
        diagnostics["params"]["diagnostics"]
            .as_array()
            .is_some_and(|items| { items.iter().any(|item| item["code"] == "pdx-unknown-scope") })
    );

    let _: CompletionResponse = typed_result(&responses, 2);
    let _: Hover = typed_result(&responses, 3);
    let _: Vec<Location> = typed_result(&responses, 4);
    let _: Vec<Location> = typed_result(&responses, 5);
    let _: Vec<DocumentSymbol> = typed_result(&responses, 6);
    let _: Vec<SymbolInformation> = typed_result(&responses, 7);
    let _: PrepareRenameResponse = typed_result(&responses, 9);
    let _: WorkspaceEdit = typed_result(&responses, 10);
    let _: Vec<Diagnostic> = serde_json::from_value(diagnostics["params"]["diagnostics"].clone())
        .expect("diagnostic notification should use the standard LSP shape");
    fs::remove_dir_all(root_dir).expect("cleanup");
}

#[test]
fn memory_transport_preserves_hir_disambiguated_mixed_context_completion() {
    let (root_dir, root_uri) = temp_workspace_dir();
    let events_dir = root_dir.join("events");
    fs::create_dir_all(&events_dir).expect("create events dir");
    let file_path = events_dir.join("mixed-completion.txt");
    fs::write(&file_path, "").expect("create placeholder file");
    let uri = canonical_uri(&file_path);
    let text = concat!(
        "country_event = {\n",
        "  mean_time_to_happen = {\n",
        "    modifier = {\n",
        "      factor = 0.5\n",
        "      \n",
        "      always = maybe\n",
        "    }\n",
        "  }\n",
        "}\n",
    );
    let input = frames([
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"rootUri":root_uri,"capabilities":{}}}),
        json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
        json!({"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":uri,"languageId":"eu4","version":1,"text":text}}}),
        json!({"jsonrpc":"2.0","id":2,"method":"textDocument/completion","params":{"textDocument":{"uri":uri},"position":{"line":4,"character":6}}}),
        json!({"jsonrpc":"2.0","id":3,"method":"shutdown","params":{}}),
        json!({"jsonrpc":"2.0","method":"exit"}),
    ]);
    let mut output = Vec::new();
    let mut server = eu4_server(InitializeOptions).expect("embedded rules");
    server
        .run_transport(Cursor::new(input), &mut output)
        .expect("transport");
    let responses = decode_frames(&output);
    let completion = responses
        .iter()
        .find(|value| value["id"] == 2)
        .expect("completion response");
    let labels = completion["result"]["items"]
        .as_array()
        .expect("completion items")
        .iter()
        .filter_map(|item| item["label"].as_str())
        .collect::<Vec<_>>();
    assert!(
        labels.contains(&"factor"),
        "missing structural completion: {labels:?}"
    );
    assert!(
        labels.contains(&"always"),
        "missing trigger completion: {labels:?}"
    );

    let diagnostics = responses
        .iter()
        .find(|value| value["method"] == "textDocument/publishDiagnostics")
        .expect("diagnostic notification");
    let diagnostics = diagnostics["params"]["diagnostics"]
        .as_array()
        .expect("diagnostics");
    assert!(
        diagnostics
            .iter()
            .all(|item| item["code"] != "pdx-unknown-key"),
        "known mixed-context keys were rejected: {diagnostics:?}"
    );
    assert!(
        diagnostics
            .iter()
            .any(|item| item["code"] == "pdx-invalid-value"),
        "invalid trigger value was not diagnosed: {diagnostics:?}"
    );
    fs::remove_dir_all(root_dir).expect("cleanup");
}

#[test]
fn snippet_placeholders_are_stripped_for_plain_text_fallbacks() {
    assert_eq!(
        strip_snippet_placeholders("name = {\n    $0\n}"),
        "name = {\n}"
    );
    assert_eq!(
        strip_snippet_placeholders("apply = {\n    amount = $1\n    optional = $2\n    $0\n}"),
        "apply = {\n    amount = \n    optional = \n}"
    );
    assert_eq!(strip_snippet_placeholders("plain"), "plain");
}

#[test]
fn memory_transport_negotiates_completion_snippet_support() {
    let (root_dir, root_uri) = temp_workspace_dir();
    let effects_dir = root_dir.join("common/scripted_effects");
    fs::create_dir_all(&effects_dir).expect("create scripted effects directory");
    fs::write(
        effects_dir.join("00_test.txt"),
        "apply = { value = $amount$ }\n",
    )
    .expect("scripted effect definition");
    let events_dir = root_dir.join("events");
    fs::create_dir_all(&events_dir).expect("create events dir");
    let file_path = events_dir.join("snippet-use.txt");
    fs::write(&file_path, "").expect("create placeholder file");
    let uri = canonical_uri(&file_path);
    let text = "country_event = { immediate = { ap";
    let run = |capabilities: Value| {
        let input = frames([
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"rootUri":root_uri.clone(),"capabilities":capabilities}}),
            json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
            json!({"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":uri.clone(),"languageId":"eu4","version":1,"text":text}}}),
            json!({"jsonrpc":"2.0","id":2,"method":"textDocument/completion","params":{"textDocument":{"uri":uri.clone()},"position":{"line":0,"character":33}}}),
            json!({"jsonrpc":"2.0","id":3,"method":"shutdown","params":{}}),
            json!({"jsonrpc":"2.0","method":"exit"}),
        ]);
        let mut output = Vec::new();
        let mut server = eu4_server(InitializeOptions).expect("embedded rules");
        server
            .run_transport(Cursor::new(input), &mut output)
            .expect("transport");
        decode_frames(&output)
    };
    let initialize =
        run(json!({"textDocument":{"completion":{"completionItem":{"snippetSupport":true}}}}));
    let capabilities = initialize
        .iter()
        .find(|value| value["id"] == 1)
        .expect("initialize response");
    assert_eq!(
        capabilities["result"]["capabilities"]["completionProvider"]["resolveProvider"],
        true
    );
    let snippet_items = initialize
        .iter()
        .find(|value| value["id"] == 2)
        .expect("snippet completion");
    let apply = snippet_items["result"]["items"]
        .as_array()
        .expect("completion items")
        .iter()
        .find(|item| item["label"] == "apply")
        .expect("scripted effect item");
    assert_eq!(apply["insertText"], "apply = {\n\tamount = $1\n\t$0\n}");
    assert_eq!(apply["insertTextFormat"], 2, "snippet format");

    let no_snippet = run(json!({}));
    let plain_items = no_snippet
        .iter()
        .find(|value| value["id"] == 2)
        .expect("plain completion");
    let apply = plain_items["result"]["items"]
        .as_array()
        .expect("completion items")
        .iter()
        .find(|item| item["label"] == "apply")
        .expect("scripted effect item");
    assert_eq!(apply["insertText"], "apply = {\n\tamount = \n}");
    assert_eq!(apply["insertTextFormat"], 1, "plain text fallback");
    assert!(
        apply["insertText"]
            .as_str()
            .is_some_and(|text| !text.contains('$')),
        "plain text fallback must not contain snippet placeholders"
    );
    fs::remove_dir_all(root_dir).expect("cleanup");
}

#[test]
fn memory_transport_resolves_completion_items_by_data() {
    let (root_dir, root_uri) = temp_workspace_dir();
    let events_dir = root_dir.join("events");
    fs::create_dir_all(&events_dir).expect("create events dir");
    let file_path = events_dir.join("resolve-completion.txt");
    fs::write(&file_path, "").expect("create placeholder file");
    let uri = canonical_uri(&file_path);
    let text = concat!(
        "country_event = {\n",
        "  mean_time_to_happen = {\n",
        "    modifier = {\n",
        "      factor = 0.5\n",
        "      \n",
        "      always = maybe\n",
        "    }\n",
        "  }\n",
        "}\n",
    );
    let input = frames([
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"rootUri":root_uri,"capabilities":{}}}),
        json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
        json!({"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":uri,"languageId":"eu4","version":1,"text":text}}}),
        json!({"jsonrpc":"2.0","id":2,"method":"textDocument/completion","params":{"textDocument":{"uri":uri},"position":{"line":4,"character":6}}}),
        json!({"jsonrpc":"2.0","id":8,"method":"shutdown","params":{}}),
        json!({"jsonrpc":"2.0","method":"exit"}),
    ]);
    let mut output = Vec::new();
    let mut server = eu4_server(InitializeOptions).expect("embedded rules");
    server
        .run_transport(Cursor::new(input), &mut output)
        .expect("transport");
    let responses = decode_frames(&output);
    let completion = responses
        .iter()
        .find(|value| value["id"] == 2)
        .expect("completion response");
    let factor = completion["result"]["items"]
        .as_array()
        .expect("completion items")
        .iter()
        .find(|item| item["label"] == "factor")
        .expect("rule item");
    let data = factor["data"].as_str().expect("resolve data");
    assert!(
        data.starts_with("rule:"),
        "rule-backed items must carry a rule id: {data}"
    );

    let mut resolve_input = Vec::new();
    resolve_input.extend(frame(json!({
        "jsonrpc":"2.0","id":1,"method":"initialize","params":{"rootUri":root_uri,"capabilities":{}}
    })));
    resolve_input.extend(frame(
        json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
    ));
    resolve_input.extend(frame(json!({
        "jsonrpc":"2.0","id":3,"method":"completionItem/resolve",
        "params": factor.clone()
    })));
    resolve_input.extend(frame(
        json!({"jsonrpc":"2.0","id":9,"method":"shutdown","params":{}}),
    ));
    resolve_input.extend(frame(json!({"jsonrpc":"2.0","method":"exit"})));
    let mut resolve_output = Vec::new();
    let mut server = eu4_server(InitializeOptions).expect("embedded rules");
    server
        .run_transport(Cursor::new(resolve_input), &mut resolve_output)
        .expect("transport");
    let resolve_responses = decode_frames(&resolve_output);
    let resolved = resolve_responses
        .iter()
        .find(|value| value["id"] == 3)
        .expect("resolve response");
    assert_eq!(resolved["result"]["label"], "factor");
    let _: CompletionItem = serde_json::from_value(resolved["result"].clone())
        .expect("resolve response must be a standard CompletionItem");
    fs::remove_dir_all(root_dir).expect("cleanup");
}

#[test]
fn memory_transport_exposes_parameters_as_document_local_symbols() {
    let (root_dir, root_uri) = temp_workspace_dir();
    let effects_dir = root_dir.join("common/scripted_effects");
    fs::create_dir_all(&effects_dir).expect("create scripted effects directory");
    let file_path = effects_dir.join("parameters.txt");
    fs::write(&file_path, "").expect("create placeholder file");
    let uri = canonical_uri(&file_path);
    let text = "apply = { value = $Amount$ again = $amount$ [[optional] enabled = yes ] }\n";
    let input = frames([
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"rootUri":root_uri,"capabilities":{}}}),
        json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
        json!({"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":uri,"languageId":"eu4","version":1,"text":text}}}),
        json!({"jsonrpc":"2.0","id":2,"method":"textDocument/documentSymbol","params":{"textDocument":{"uri":uri}}}),
        json!({"jsonrpc":"2.0","id":3,"method":"workspace/symbol","params":{"query":"amount"}}),
        json!({"jsonrpc":"2.0","id":4,"method":"shutdown","params":{}}),
        json!({"jsonrpc":"2.0","method":"exit"}),
    ]);
    let mut output = Vec::new();
    let mut server = eu4_server(InitializeOptions).expect("embedded rules");
    server
        .run_transport(Cursor::new(input), &mut output)
        .expect("transport");
    let responses = decode_frames(&output);

    let symbols: Vec<DocumentSymbol> = typed_result(&responses, 2);
    let amount = symbols
        .iter()
        .find(|symbol| symbol.name == "Amount")
        .expect("inferred parameter document symbol");
    assert_eq!(amount.kind, SymbolKind::VARIABLE);
    assert_eq!(
        amount.selection_range.end.character - amount.selection_range.start.character,
        u32::try_from("Amount".len()).expect("name length")
    );
    assert!(amount.range.start.character < amount.selection_range.start.character);
    assert!(amount.selection_range.end.character < amount.range.end.character);

    let workspace: Vec<SymbolInformation> = typed_result(&responses, 3);
    assert!(
        workspace
            .iter()
            .all(|symbol| !symbol.name.eq_ignore_ascii_case("amount"))
    );
    fs::remove_dir_all(root_dir).expect("cleanup");
}

#[test]
fn memory_transport_formats_safe_text_and_refuses_recovered_syntax() {
    let valid_uri = "file:///tmp/format-valid.txt";
    let unsafe_uri = "file:///tmp/format-unsafe.txt";
    let input = frames([
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"rootUri":"file:///tmp","capabilities":{}}}),
        json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
        json!({"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":valid_uri,"languageId":"eu4","version":1,"text":"root={name=\"汉😀\" other=yes}"}}}),
        json!({"jsonrpc":"2.0","id":2,"method":"textDocument/formatting","params":{"textDocument":{"uri":valid_uri},"options":{"tabSize":2,"insertSpaces":true}}}),
        json!({"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":unsafe_uri,"languageId":"eu4","version":1,"text":"country_event = {"}}}),
        json!({"jsonrpc":"2.0","id":3,"method":"textDocument/formatting","params":{"textDocument":{"uri":unsafe_uri},"options":{"tabSize":4,"insertSpaces":true}}}),
        json!({"jsonrpc":"2.0","id":4,"method":"shutdown","params":{}}),
        json!({"jsonrpc":"2.0","method":"exit"}),
    ]);
    let mut output = Vec::new();
    let mut server = eu4_server(InitializeOptions).expect("server");
    server
        .run_transport(Cursor::new(input), &mut output)
        .expect("transport");
    let responses = decode_frames(&output);

    let edits = typed_result::<Vec<lsp_types::TextEdit>>(&responses, 2);
    let source = "root={name=\"汉😀\" other=yes}";
    let line_index = LineIndex::new(source);
    let mut formatted = source.to_owned();
    for edit in edits.iter().rev() {
        let start = line_index
            .offset(
                source,
                Position::new(edit.range.start.line, edit.range.start.character),
            )
            .expect("format edit start");
        let end = line_index
            .offset(
                source,
                Position::new(edit.range.end.line, edit.range.end.character),
            )
            .expect("format edit end");
        formatted.replace_range(start as usize..end as usize, &edit.new_text);
    }
    assert_eq!(formatted, "root = {\n\tname = \"汉😀\"\n\tother = yes\n}\n");
    let unsafe_edits = typed_result::<Vec<lsp_types::TextEdit>>(&responses, 3);
    assert!(unsafe_edits.is_empty());
}

#[test]
fn memory_transport_rename_covers_current_mod_disk_references() {
    let nonce = std::process::id();
    let root = std::env::temp_dir().join(format!("pdx-lsp-rename-{nonce}"));
    let target_path = root.join("common/events/target.txt");
    let references_path = root.join("common/events/references.txt");
    fs::create_dir_all(target_path.parent().expect("target parent")).expect("directories");
    fs::write(&target_path, "country_event = { id = cross.1 }\n").expect("target");
    fs::write(&references_path, "event = cross.1\n").expect("reference");
    let target_uri = canonical_uri(&target_path);
    let references_uri = canonical_uri(&references_path);
    let root_uri = canonical_uri(&fs::canonicalize(&root).expect("canonical root"));
    let input = frames([
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"rootUri":root_uri,"capabilities":{}}}),
        json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
        json!({"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":target_uri,"languageId":"eu4","version":1,"text":"country_event = { id = cross.1 }\n"}}}),
        json!({"jsonrpc":"2.0","id":2,"method":"textDocument/rename","params":{"textDocument":{"uri":target_uri},"position":{"line":0,"character":25},"newName":"renamed.1"}}),
        json!({"jsonrpc":"2.0","id":3,"method":"shutdown","params":{}}),
        json!({"jsonrpc":"2.0","method":"exit"}),
    ]);
    let mut output = Vec::new();
    let mut server = eu4_server(InitializeOptions).expect("bundled rules should load");
    server
        .run_transport(Cursor::new(input), &mut output)
        .expect("transport should finish");
    let responses = decode_frames(&output);
    let rename = responses
        .iter()
        .find(|value| value["id"] == 2)
        .expect("rename response");
    assert!(rename["error"].is_null(), "rename response={rename}");
    let changes = rename["result"]["changes"]
        .as_object()
        .expect("workspace changes");
    assert_eq!(
        changes
            .get(&target_uri)
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(1)
    );
    assert_eq!(
        changes
            .get(&references_uri)
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(1)
    );
    assert!(changes.values().all(|edits| {
        edits
            .as_array()
            .is_some_and(|edits| edits.iter().all(|edit| edit["newText"] == "renamed.1"))
    }));
    fs::remove_dir_all(root).expect("cleanup");
}
