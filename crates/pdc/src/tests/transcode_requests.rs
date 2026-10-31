//! `pdc/transcodeDecode` / `pdc/transcodeEncode` request tests. Fixtures are built with
//! the `transcode` crate itself (its codec is pinned separately by the corpus and vector
//! suites); these tests pin the protocol layer — path eligibility, form dispatch,
//! passthrough/refusal shapes, hex framing, and the `transparentScriptGlobs` option.

use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use transcode::{EscapeSet, Profile, encode_file};

use super::*;

// 是 and 支 contain low 0x2F: their compensated '=' payload must not be
// misclassified as damage by the whole-file requests used by the editor.
const READABLE_SCRIPT: &str = "# 支持中文注释\nname = \"是否支持中文内容\"\n";
const READABLE_LOC: &str = "l_english:\n KEY:0 \"是否支持中文内容\"\n";

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn write_file(root: &Path, relative: &str, bytes: &[u8]) -> PathBuf {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().expect("file has a parent directory"))
        .expect("fixture directory");
    fs::write(&path, bytes).expect("fixture file");
    path
}

struct TransportRun {
    responses: Vec<Value>,
}

fn run_transcode_session(
    _root: &Path,
    root_uri: &str,
    initialization_options: Value,
    requests: Vec<Value>,
) -> TransportRun {
    let mut messages = vec![
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
            "workspaceFolders":[{"uri":root_uri,"name":"test"}],
            "capabilities":{},
            "initializationOptions":initialization_options,
        }}),
        json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
    ];
    let mut next_id = 2_i64;
    for request in requests {
        messages.push(json!({
            "jsonrpc":"2.0","id":next_id,"method":request["method"],
            "params":request.get("params").cloned().unwrap_or(Value::Null),
        }));
        next_id += 1;
    }
    messages.push(json!({"jsonrpc":"2.0","id":next_id,"method":"shutdown","params":{}}));
    messages.push(json!({"jsonrpc":"2.0","method":"exit"}));
    let input = frames(messages);
    let mut output = Vec::new();
    let mut server = eu4_server(InitializeOptions).expect("embedded rules");
    server
        .run_transport(Cursor::new(input), &mut output)
        .expect("transport");
    TransportRun {
        responses: decode_frames(&output),
    }
}

impl TransportRun {
    fn result(&self, id: i64) -> &Value {
        self.responses
            .iter()
            .find(|value| value["id"] == id)
            .unwrap_or_else(|| panic!("missing response {id}"))
            .get("result")
            .unwrap_or_else(|| panic!("response {id} failed"))
    }

    fn error(&self, id: i64) -> &Value {
        self.responses
            .iter()
            .find(|value| value["id"] == id)
            .unwrap_or_else(|| panic!("missing response {id}"))
            .get("error")
            .unwrap_or_else(|| panic!("response {id} succeeded"))
    }
}

fn decode_request(path: &Path) -> Value {
    json!({"method":"pdc/transcodeDecode","params":{"path":path.to_string_lossy()}})
}

fn decode_request_with_bytes(path: &Path, bytes: &[u8]) -> Value {
    json!({"method":"pdc/transcodeDecode","params":{
        "path":path.to_string_lossy(),
        "bytes":hex(bytes),
    }})
}

fn encode_request(path: &Path, bytes: &[u8]) -> Value {
    json!({"method":"pdc/transcodeEncode","params":{
        "path":path.to_string_lossy(),
        "bytes":hex(bytes),
    }})
}

#[test]
fn transcode_complete_files_and_mixed_input_round_trip() {
    let (root, root_uri) = temp_workspace_dir();
    let script = write_file(&root, "events/plain.txt", READABLE_SCRIPT.as_bytes());
    let loc = write_file(
        &root,
        "localisation/test_l_english.yml",
        READABLE_LOC.as_bytes(),
    );
    let whole_script = encode_file(READABLE_SCRIPT, Profile::Script, EscapeSet::Paratranz).unwrap();
    let whole_loc = encode_file(READABLE_LOC, Profile::Localisation, EscapeSet::Paratranz).unwrap();
    let unquoted = encode_file("# 中\n", Profile::Script, EscapeSet::Paratranz).unwrap();
    let escaped_word = encode_file("中文", Profile::Script, EscapeSet::Paratranz).unwrap();
    let mixed = [
        "# 中文注释\nname = \"".as_bytes(),
        &escaped_word,
        "内容\"\n".as_bytes(),
    ]
    .concat();
    let run = run_transcode_session(
        &root,
        &root_uri,
        json!({}),
        vec![
            decode_request(&script),
            encode_request(&script, READABLE_SCRIPT.as_bytes()),
            decode_request_with_bytes(&script, &whole_script),
            encode_request(&script, &whole_script),
            decode_request_with_bytes(&script, &unquoted),
            decode_request_with_bytes(&script, &mixed),
            encode_request(&loc, READABLE_LOC.as_bytes()),
            decode_request_with_bytes(&loc, &whole_loc),
            decode_request_with_bytes(&script, "# 中文注释\nname = \"x\"\n".as_bytes()),
        ],
    );
    assert_eq!(run.result(2)["needsEncoding"], json!(true));
    assert_eq!(run.result(3)["bytes"], hex(&whole_script));
    assert_eq!(run.result(4)["bytes"], hex(READABLE_SCRIPT.as_bytes()));
    assert_eq!(run.result(5)["bytes"], hex(&whole_script));
    assert_eq!(run.result(6)["bytes"], hex("# 中\n".as_bytes()));
    assert_eq!(
        run.result(7)["bytes"],
        hex("# 中文注释\nname = \"中文内容\"\n".as_bytes())
    );
    assert_eq!(run.result(8)["bytes"], hex(&whole_loc));
    assert_eq!(run.result(9)["bytes"], hex(READABLE_LOC.as_bytes()));
    assert_eq!(
        run.result(10)["needsEncoding"],
        json!(true),
        "comment-only CJK participates"
    );
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn transcode_refuses_damage_anywhere_without_partial_output() {
    let (root, root_uri) = temp_workspace_dir();
    let script = write_file(&root, "events/plain.txt", b"name = \"x\"\n");
    let loc = write_file(&root, "localisation/plain.yml", b"l_english:\n");
    let run = run_transcode_session(
        &root,
        &root_uri,
        json!({}),
        vec![
            encode_request(&script, "# 😀\nname = \"x\"\n".as_bytes()),
            encode_request(&script, b"# \x10\n"),
            decode_request_with_bytes(&script, b"# \x10\n"),
            encode_request(&loc, b"# \xff\nname = \"x\"\n"),
            decode_request_with_bytes(&loc, b"# \xff\n"),
            json!({"method":"pdc/transcodeDecode","params":{
            "path":script.to_string_lossy(), "quotedOnly":true}}),
            json!({"method":"pdc/transcodeEncode","params":{
            "path":script.to_string_lossy(), "bytes":"", "preserveEscaped":true}}),
        ],
    );
    assert_eq!(run.result(2)["refused"], "unencodable");
    assert!(run.result(2).get("bytes").is_none());
    assert_eq!(run.result(3)["refused"], "brokenEscape");
    assert_eq!(run.result(3)["offsets"], json!([2]));
    assert!(run.result(3).get("bytes").is_none());
    assert_eq!(run.result(4)["form"], "damaged");
    assert_eq!(run.result(4)["refused"], "brokenEscape");
    assert_eq!(
        run.result(4)["bytes"],
        hex(b"# \x10\n"),
        "only original bytes are returned for repair"
    );
    assert_eq!(run.result(5)["refused"], "invalidUtf8");
    assert_eq!(run.result(6)["refused"], "invalidUtf8");
    assert_eq!(run.error(7)["code"], -32602, "removed options are rejected");
    assert_eq!(run.error(8)["code"], -32602);
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn transcode_buffer_precedence_and_path_eligibility_are_preserved() {
    let (root, root_uri) = temp_workspace_dir();
    let script = write_file(&root, "events/plain.txt", b"name = \"x\"\n");
    let markdown = write_file(&root, "docs/readme.md", b"# readme\n");
    let run = run_transcode_session(
        &root,
        &root_uri,
        json!({}),
        vec![
            decode_request(&script),
            decode_request_with_bytes(&script, READABLE_SCRIPT.as_bytes()),
            decode_request(&markdown),
            json!({"method":"pdc/transcodeDecode","params":{
            "path":script.to_string_lossy(), "bytes":"abc"}}),
        ],
    );
    assert_eq!(run.result(2)["needsEncoding"], false);
    assert_eq!(run.result(3)["bytes"], hex(READABLE_SCRIPT.as_bytes()));
    assert_eq!(run.result(3)["needsEncoding"], true);
    assert_eq!(run.result(4)["eligible"], false);
    assert_eq!(run.error(5)["code"], -32602);
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn transcode_encode_validates_hex_and_eligibility() {
    let (root, root_uri) = temp_workspace_dir();
    let eligible = write_file(&root, "events/draft.txt", b"");
    let markdown = write_file(&root, "docs/readme.md", b"");

    let run = run_transcode_session(
        &root,
        &root_uri,
        json!({}),
        vec![
            json!({"method":"pdc/transcodeEncode","params":{"path":eligible.to_string_lossy(),"bytes":"abc"}}),
            json!({"method":"pdc/transcodeEncode","params":{"path":eligible.to_string_lossy(),"bytes":"zz"}}),
            encode_request(&markdown, b"name = \"x\"\n"),
        ],
    );

    assert_eq!(run.error(2)["code"], json!(-32602));
    assert_eq!(run.error(3)["code"], json!(-32602));
    assert_eq!(run.error(4)["code"], json!(-32602));

    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn transparent_script_globs_option_scopes_eligibility() {
    let (root, root_uri) = temp_workspace_dir();
    let events = write_file(&root, "events/a.txt", b"name = \"x\"\n");
    let common = write_file(&root, "common/b.txt", b"name = \"x\"\n");
    let yml = write_file(&root, "localisation/a_l_english.yml", b"l_english:\n");

    let run = run_transcode_session(
        &root,
        &root_uri,
        json!({"transparentScriptGlobs": ["events/**"]}),
        vec![
            decode_request(&events),
            decode_request(&common),
            decode_request(&yml),
        ],
    );
    assert_eq!(run.result(2)["profile"], json!("script"));
    assert_eq!(run.result(3), &json!({"eligible": false}));
    // Localisation yml eligibility never depends on the script globs.
    assert_eq!(run.result(4)["profile"], json!("localisation"));

    let run = run_transcode_session(
        &root,
        &root_uri,
        json!({"transparentScriptGlobs": []}),
        vec![decode_request(&events)],
    );
    assert_eq!(run.result(2), &json!({"eligible": false}));

    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn transparent_script_globs_option_is_validated() {
    let (root, _root_uri) = temp_workspace_dir();
    let token = engine::WorkspaceScanToken::new();
    let resolved = resolve_source_roots(
        Some(&root),
        Some(json!({"transparentScriptGlobs": ["events/**"]})),
        &token,
    )
    .expect("valid globs resolve");
    assert_eq!(
        resolved.transparent_script_globs.patterns(),
        &["events/**".to_owned()]
    );
    // Absent globs fall back to the editor default.
    let resolved =
        resolve_source_roots(Some(&root), Some(json!({})), &token).expect("default globs resolve");
    assert_eq!(
        resolved.transparent_script_globs.patterns(),
        &["**/*.txt".to_owned()]
    );
    let rejected = resolve_source_roots(
        Some(&root),
        Some(json!({"transparentScriptGlobs": ["bad\0glob"]})),
        &token,
    )
    .expect_err("NUL inside a glob must be rejected");
    assert_eq!(rejected.code, INVALID_PARAMS);
    assert!(rejected.message.contains("transparentScriptGlobs"));
    fs::remove_dir_all(root).expect("cleanup");
}
