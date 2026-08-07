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
    assert!(contents.contains("rule:"));
    let unknown_hover = responses
        .iter()
        .find(|value| value["id"] == 3)
        .expect("unknown hover response");
    assert_eq!(unknown_hover["result"], Value::Null);
    let _: Hover = typed_result(&responses, 2);
    fs::remove_dir_all(root).expect("cleanup");
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
    assert_eq!(apply["insertText"], "apply = {\n    amount = $1\n    $0\n}");
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
    assert_eq!(apply["insertText"], "apply = {\n    amount = \n}");
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
