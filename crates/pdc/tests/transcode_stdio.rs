use std::io::Write;
use std::process::{Command, Output, Stdio};

fn convert(profile: &str, direction: &str, input: &[u8]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_paradoxcode"))
        .args(["--transcode-stdio", profile, direction])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn binary_codec_matches_release_fixture_and_raw_script_bytes() {
    let master = include_bytes!("../../transcode/tests/corpus/cjk_pair_master.yml");
    let release = include_bytes!("../../transcode/tests/corpus/cjk_pair_release.yml");
    for (profile, readable, game) in [
        ("localisation", master.as_slice(), release.as_slice()),
        ("script", "中国".as_bytes(), &[16, 45, 78, 16, 253, 86][..]),
    ] {
        for (direction, input, expected) in [("encode", readable, game), ("decode", game, readable)]
        {
            let output = convert(profile, direction, input);
            assert!(output.status.success());
            assert_eq!(output.stdout, expected);
            assert!(output.stderr.is_empty());
        }
    }
}

#[test]
fn binary_codec_refuses_complete_operation_with_input_position() {
    let output = convert("script", "encode", "ok 😀".as_bytes());
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let refusal: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(refusal["points"][0]["byteIndex"], 3);
    assert_eq!(refusal["points"][0]["codePoint"], 0x1f600);
    let broken = convert("localisation", "decode", b"abc\x10");
    assert_eq!(broken.status.code(), Some(2));
    assert!(broken.stdout.is_empty());
    let refusal: serde_json::Value = serde_json::from_slice(&broken.stderr).unwrap();
    assert_eq!(refusal["offsets"][0], 3);
}
