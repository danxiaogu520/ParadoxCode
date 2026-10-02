//! Rejected source must never create or overwrite a distributable arena.
use std::fs;
use std::process::Command;

#[test]
fn invalid_source_refuses_to_create_or_replace_artifacts() {
    let fixture = tempfile::tempdir().unwrap();
    let source = fixture.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(
        source.join("game.json"),
        r#"{"game_id":"test","source_format_version":13}"#,
    )
    .unwrap();
    fs::write(
        source.join("rules.json"),
        r#"{"files":{"script":{"path":"","root":"missing"}}}"#,
    )
    .unwrap();
    let manifest = fixture.path().join("manifest.json");
    let payload = fixture.path().join("arena.json");
    let run = || {
        Command::new(env!("CARGO_BIN_EXE_bake-ir"))
            .arg("build")
            .arg("--source")
            .arg(&source)
            .arg("--manifest")
            .arg(&manifest)
            .arg("--output")
            .arg(&payload)
            .output()
            .unwrap()
    };
    let first = run();
    assert!(!first.status.success());
    assert!(
        String::from_utf8_lossy(&first.stderr).contains("missing"),
        "{first:?}"
    );
    assert!(!manifest.exists());
    assert!(!payload.exists());
    fs::write(&manifest, "previous manifest").unwrap();
    fs::write(&payload, "previous arena").unwrap();
    assert!(!run().status.success());
    assert_eq!(fs::read_to_string(manifest).unwrap(), "previous manifest");
    assert_eq!(fs::read_to_string(payload).unwrap(), "previous arena");
}

#[test]
fn invalid_block_form_refuses_to_create_or_replace_artifacts() {
    let fixture = tempfile::tempdir().unwrap();
    let source = fixture.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(
        source.join("game.json"),
        r#"{"game_id":"test","source_format_version":13}"#,
    )
    .unwrap();
    fs::write(source.join("rules.json"), r#"{"files":{"script":{"path":"","root":"body"}},"schemas":{"body":{"forms":[{"fields":{"missing":"1"}}]}}}"#).unwrap();
    let manifest = fixture.path().join("manifest.json");
    let payload = fixture.path().join("arena.json");
    for existing in [false, true] {
        if existing {
            fs::write(&manifest, "previous manifest").unwrap();
            fs::write(&payload, "previous arena").unwrap();
        }
        let output = Command::new(env!("CARGO_BIN_EXE_bake-ir"))
            .args(["build", "--source"])
            .arg(&source)
            .arg("--manifest")
            .arg(&manifest)
            .arg("--output")
            .arg(&payload)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("/forms/0/fields/missing"),
            "{output:?}"
        );
        if existing {
            assert_eq!(fs::read_to_string(&manifest).unwrap(), "previous manifest");
            assert_eq!(fs::read_to_string(&payload).unwrap(), "previous arena");
        } else {
            assert!(!manifest.exists());
            assert!(!payload.exists());
        }
    }
}
