use std::fs;
use std::io::Cursor;

use rules::{RuleSet, RulesError, RulesModel};
use serde_json::{Value, json};
use text::TextRange;

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
    let uri = FileUri::from_path(&path).expect("absolute path has a file URI");
    assert!(uri.as_str().contains("%20"));
    assert_eq!(uri.to_path().expect("URI should decode"), path);
}

/// `pdcloc://` is the extension's decoded view over a real file: the path
/// must resolve to the same on-disk location the `file://` spelling does, so
/// a virtually opened document attaches to (and hides) its backing file.
#[test]
fn pdcloc_uris_resolve_to_the_backing_file_path() {
    // Drive-letter URIs only exist on Windows clients.  On POSIX the leading
    // `/` is part of the path, so `pdcloc:///C:/...` keeps its slash exactly
    // like `file:///C:/...` does.
    #[cfg(windows)]
    {
        assert_eq!(
            FileUri::parse("pdcloc:///C:/mods/edg/localisation/replace/edg_l_english.yml")
                .expect("pdcloc URI should parse")
                .to_path()
                .expect("pdcloc URI should decode"),
            std::path::PathBuf::from("C:/mods/edg/localisation/replace/edg_l_english.yml")
        );
        assert_eq!(
            FileUri::parse("pdcloc://localhost/C:/mods/edg/history/countries/CHI%20-%20Ming.txt")
                .expect("pdcloc URI with localhost authority should parse")
                .to_path()
                .expect("localhost authority should decode"),
            std::path::PathBuf::from("C:/mods/edg/history/countries/CHI - Ming.txt")
        );
    }
    assert_eq!(
        FileUri::parse("pdcloc:///tmp/edg/localisation/x.yml")
            .expect("scheme-swapped file URI should parse")
            .to_path()
            .expect("scheme-swapped file URI should decode"),
        // The Windows branch of `to_path` drops the leading `/` of a
        // POSIX-style path just like it does for `file://` URIs.
        std::path::PathBuf::from(if cfg!(windows) {
            "tmp/edg/localisation/x.yml"
        } else {
            "/tmp/edg/localisation/x.yml"
        })
    );
    // A non-local authority names a remote host: a UNC path on Windows, an
    // error where no filesystem spelling exists.
    #[cfg(windows)]
    {
        assert_eq!(
            FileUri::parse("pdcloc://remote/share/file.yml")
                .expect("remote authority should parse")
                .to_path()
                .expect("remote authority should decode"),
            std::path::PathBuf::from(r"\\remote\share\file.yml")
        );
    }
    #[cfg(not(windows))]
    {
        assert!(
            FileUri::parse("pdcloc://remote/share/file.yml")
                .and_then(|uri| uri.to_path())
                .is_err()
        );
    }
    assert!(FileUri::parse("untitled:Untitled-1").is_err());
}

#[test]
fn file_uris_decode_client_spelling_variants_identically() {
    // VS Code serializes drive letters percent-encoded and lowercased; both
    // spellings of the same document must decode.
    #[cfg(windows)]
    {
        for spelling in ["file:///c%3A/mods/x.txt", "file:///C:/mods/x.txt"] {
            assert_eq!(
                FileUri::parse(spelling)
                    .expect("drive-letter URI should parse")
                    .to_path()
                    .expect("drive-letter URI should decode"),
                std::path::PathBuf::from(
                    spelling.trim_start_matches("file:///").replace("%3A", ":")
                )
            );
        }
        // Scheme case is insignificant per RFC 3986.
        assert_eq!(
            FileUri::parse("FILE:///C:/mods/x.txt")
                .expect("uppercase scheme should parse")
                .to_path()
                .expect("uppercase scheme should decode"),
            std::path::PathBuf::from("C:/mods/x.txt")
        );
        // Query and fragment components never enter the path.
        assert_eq!(
            FileUri::parse("file:///C:/mods/x.txt?server=1#frag")
                .expect("URI with query should parse")
                .to_path()
                .expect("URI with query should decode"),
            std::path::PathBuf::from("C:/mods/x.txt")
        );
    }
}

#[test]
fn unc_paths_round_trip_through_authority_uris() {
    let path = std::path::Path::new(r"\\server\share\mod x\localisation\a.yml");
    let Ok(uri) = FileUri::from_path(path) else {
        return; // Non-Windows: UNC input is not an absolute local path.
    };
    assert_eq!(
        uri.as_str(),
        "file://server/share/mod%20x/localisation/a.yml"
    );
    assert_eq!(uri.to_path().expect("UNC URI should decode"), path);
}

#[test]
fn relative_paths_have_no_file_uri() {
    assert!(matches!(
        FileUri::from_path(std::path::Path::new("relative/x.txt")),
        Err(crate::UriError::NotAbsolute)
    ));
}

#[cfg(windows)]
#[test]
fn windows_file_uri_normalizes_extended_canonical_paths() {
    let path = std::path::Path::new(r"\\?\C:\Paradox Code\events\test.txt");
    let uri = FileUri::from_path(path).expect("verbatim drive path has a file URI");
    assert_eq!(uri.as_str(), "file:///C:/Paradox%20Code/events/test.txt");
    assert!(!uri.as_str().contains("%5C"));
    assert!(!uri.as_str().contains("%3F"));
}

#[cfg(windows)]
#[test]
fn windows_file_uri_normalizes_verbatim_unc_paths() {
    let path = std::path::Path::new(r"\\?\UNC\server\share\events\test.txt");
    let uri = FileUri::from_path(path).expect("verbatim UNC path has a file URI");
    assert_eq!(uri.as_str(), "file://server/share/events/test.txt");
    assert_eq!(
        uri.to_path().expect("UNC URI should decode"),
        std::path::Path::new(r"\\server\share\events\test.txt")
    );
}

#[test]
fn selected_game_rejects_a_mismatched_rules_artifact() {
    let rules = RuleSet::from_model(RulesModel {
        game_id: "another-game".to_owned(),
        ..RulesModel::default()
    });

    let error = LspServer::try_new_with_rules(InitializeOptions, rules, game::eu4::profile())
        .expect_err("mismatched game must be rejected");
    assert!(matches!(
        error,
        LspError::Rules(RulesError::GameMismatch { expected, actual })
            if expected == "eu4" && actual == "another-game"
    ));
}

#[test]
fn memory_transport_runs_real_json_rpc_lifecycle_and_sync() {
    let path = std::env::temp_dir().join(format!("pdc-{}.txt", std::process::id()));
    fs::write(&path, "disk").expect("write disk fixture");
    let uri = file_uri_string(&path);
    let input = frames([
        json!({"jsonrpc":"2.0","id":1,"method":"shutdown","params":{}}),
        json!({"jsonrpc":"2.0","id":2,"method":"initialize","params":{"workspaceFolders":[{"uri":uri,"name":"test"}],"capabilities":{}}}),
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
        .document(&engine::DocumentId::new(uri.clone()))
        .expect("close restores disk candidate");
    assert_eq!(document.text(), "disk");
    assert_eq!(document.version(), None);
    assert_eq!(server.state(), ServerState::Exited);
    fs::remove_file(path).expect("remove disk fixture");
}

#[test]
fn typed_protocol_rejects_malformed_params_without_corrupting_lifecycle() {
    let input = frames([
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"workspaceFolders":[{"uri":"file:///tmp","name":"test"}]}}),
        json!({"jsonrpc":"2.0","id":2,"method":"initialize","params":{"workspaceFolders":[{"uri":"file:///tmp","name":"test"}],"capabilities":{}}}),
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

#[test]
fn initialize_rejects_root_uri_only_clients() {
    let input = frames([
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"rootUri":"file:///tmp","capabilities":{}}}),
        json!({"jsonrpc":"2.0","id":2,"method":"initialize","params":{"workspaceFolders":[{"uri":"file:///tmp","name":"test"}],"capabilities":{}}}),
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
    let initialize = responses
        .iter()
        .find(|value| value["id"] == 1)
        .expect("initialize response");
    assert_eq!(initialize["error"]["code"], INVALID_PARAMS);
    assert_eq!(
        initialize["error"]["message"],
        "initialize requires at least one workspace folder; rootUri-only clients are not supported"
    );
    assert!(
        responses
            .iter()
            .find(|value| value["id"] == 2)
            .is_some_and(|value| value["result"]["capabilities"].is_object())
    );
    assert!(
        responses
            .iter()
            .any(|value| value["id"] == 3 && value["result"].is_null())
    );
    assert_eq!(server.state(), ServerState::Exited);
}
