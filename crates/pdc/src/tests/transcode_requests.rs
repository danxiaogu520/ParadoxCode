//! `pdc/transcodeDecode` / `pdc/transcodeEncode` request tests. Fixtures are built with
//! the `transcode` crate itself (its codec is pinned separately by the corpus and vector
//! suites); these tests pin the protocol layer — path eligibility, form dispatch,
//! passthrough/refusal shapes, hex framing, and the `transparentScriptGlobs` option.

use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use transcode::{EscapeSet, Profile, encode_file, scoped_encode_file};

use super::*;

const READABLE_SCRIPT: &str = "# 注释保持可读\nname = \"中文内容\"\n";
const READABLE_LOC: &str = "l_english:\n KEY:0 \"中文内容\"\n";

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

fn encode_request(path: &Path, bytes: &[u8]) -> Value {
    json!({"method":"pdc/transcodeEncode","params":{
        "path":path.to_string_lossy(),
        "bytes":hex(bytes),
    }})
}

#[test]
fn transcode_decode_dispatches_plain_whole_and_scoped_forms() {
    let (root, root_uri) = temp_workspace_dir();
    let plain = write_file(&root, "events/plain.txt", READABLE_SCRIPT.as_bytes());
    let whole_bytes = encode_file(READABLE_LOC, Profile::Localisation, EscapeSet::Paratranz)
        .expect("whole-encoded fixture");
    let whole = write_file(&root, "localisation/test_l_english.yml", &whole_bytes);
    let scoped_bytes = scoped_encode_file(READABLE_SCRIPT, Profile::Script, EscapeSet::Paratranz)
        .expect("scoped-encoded fixture");
    let scoped = write_file(&root, "events/scoped.txt", &scoped_bytes);

    let run = run_transcode_session(
        &root,
        &root_uri,
        json!({}),
        vec![
            decode_request(&plain),
            decode_request(&whole),
            decode_request(&scoped),
        ],
    );

    // Plain form passes the bytes through and flags the quoted CJK a save would encode.
    let result = run.result(2);
    assert_eq!(result["eligible"], json!(true));
    assert_eq!(result["profile"], json!("script"));
    assert_eq!(result["form"], json!("plain"));
    assert_eq!(result["bytes"], json!(hex(READABLE_SCRIPT.as_bytes())));
    assert_eq!(result["broken"], json!(0));
    assert_eq!(result["quotedCjk"], json!(true));

    // Whole form decodes back to the readable localisation text.
    let result = run.result(3);
    assert_eq!(result["profile"], json!("localisation"));
    assert_eq!(result["form"], json!("whole"));
    assert_eq!(result["bytes"], json!(hex(READABLE_LOC.as_bytes())));
    assert_eq!(result["quotedCjk"], json!(false));

    // Scoped form (readable CJK comment outside the spans) resolves only inside strings.
    let result = run.result(4);
    assert_eq!(result["profile"], json!("script"));
    assert_eq!(result["form"], json!("scoped"));
    assert_eq!(result["bytes"], json!(hex(READABLE_SCRIPT.as_bytes())));

    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn transcode_decode_reports_damaged_content_and_ineligibility() {
    let (root, root_uri) = temp_workspace_dir();
    // A marker byte (0x10..=0x13) outside every quoted span marks damaged content.
    let damaged = b"# comment\nmarker \x11 here\n".to_vec();
    let damaged_path = write_file(&root, "events/damaged.txt", &damaged);
    let markdown = write_file(&root, "docs/readme.md", b"# readme\n");
    let outside = {
        let path = std::env::temp_dir().join("pdc-transcode-outside.txt");
        fs::write(&path, b"name = \"x\"\n").expect("outside fixture");
        dunce::canonicalize(&path).expect("canonical outside fixture")
    };

    let run = run_transcode_session(
        &root,
        &root_uri,
        json!({}),
        vec![
            decode_request(&damaged_path),
            decode_request(&markdown),
            decode_request(&outside),
        ],
    );

    let result = run.result(2);
    assert_eq!(result["form"], json!("damaged"));
    assert_eq!(result["bytes"], json!(hex(&damaged)));
    assert_eq!(
        result["damagedAt"],
        json!(damaged.iter().position(|b| *b == 0x11))
    );

    assert_eq!(run.result(3), &json!({"eligible": false}));
    assert_eq!(run.result(4), &json!({"eligible": false}));

    fs::remove_file(&outside).expect("cleanup outside fixture");
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn transcode_decode_accepts_pdcloc_uris_and_rejects_relative_paths() {
    let (root, root_uri) = temp_workspace_dir();
    let plain = write_file(&root, "events/plain.txt", READABLE_SCRIPT.as_bytes());
    let pdcloc_uri = FileUri::parse(&file_uri_string(&plain).replacen("file:", "pdcloc:", 1))
        .expect("pdcloc URI parses")
        .as_str()
        .to_owned();

    let run = run_transcode_session(
        &root,
        &root_uri,
        json!({}),
        vec![
            json!({"method":"pdc/transcodeDecode","params":{"path":pdcloc_uri}}),
            json!({"method":"pdc/transcodeDecode","params":{"path":"events/plain.txt"}}),
        ],
    );

    let result = run.result(2);
    assert_eq!(result["form"], json!("plain"));
    assert_eq!(result["bytes"], json!(hex(READABLE_SCRIPT.as_bytes())));
    assert_eq!(run.error(3)["code"], json!(-32602));

    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn transcode_encode_round_trips_and_refuses_the_three_guards() {
    let (root, root_uri) = temp_workspace_dir();
    let file = write_file(&root, "events/draft.txt", b"");
    let loc_file = write_file(&root, "localisation/draft_l_english.yml", b"");

    let run = run_transcode_session(
        &root,
        &root_uri,
        json!({}),
        vec![
            encode_request(&file, READABLE_SCRIPT.as_bytes()),
            // Iron rule ②: an escape marker inside a quoted string would double-encode.
            encode_request(&file, b"name = \"\x10abc\"\n"),
            // Beyond-BMP character inside a quoted string cannot round-trip.
            encode_request(&file, "name = \"😀\"\n".as_bytes()),
            // The editor buffer must be valid UTF-8.
            encode_request(&file, b"\xff\xfe"),
            // Localisation profile takes the whole-file encoder.
            encode_request(&loc_file, READABLE_LOC.as_bytes()),
        ],
    );

    let encoded = hex(
        &scoped_encode_file(READABLE_SCRIPT, Profile::Script, EscapeSet::Paratranz)
            .expect("expected scoped bytes"),
    );
    assert_eq!(run.result(2), &json!({"bytes": encoded}));

    let refused = run.result(3);
    assert_eq!(refused["refused"], json!("alreadyEscaped"));
    assert_eq!(refused["offsets"], json!([8]));

    let refused = run.result(4);
    assert_eq!(refused["refused"], json!("unencodable"));
    let points = refused["points"].as_array().expect("unencodable points");
    assert_eq!(points.len(), 1);
    assert_eq!(points[0]["codePoint"], json!(0x1F600));

    assert_eq!(run.result(5), &json!({"refused": "invalidUtf8"}));

    let encoded_loc = hex(
        &encode_file(READABLE_LOC, Profile::Localisation, EscapeSet::Paratranz)
            .expect("expected whole bytes"),
    );
    assert_eq!(run.result(6), &json!({"bytes": encoded_loc}));

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
