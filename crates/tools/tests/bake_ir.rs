//! Rejected source must never create or overwrite a distributable arena.
use std::fs;
use std::process::Command;

#[test]
fn bakes_only_the_ir_payload_under_target() {
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
        r#"{"files":{"script":{"path":"","root":"body"}},"schemas":{"body":{"fields":{}}}}"#,
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_tools"))
        .args(["rules", "bake", "--root"])
        .arg(fixture.path())
        .arg("--source")
        .arg(&source)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let payload = fixture.path().join("target/rules/compiled.ir.json");
    let ir = rules::ir::RulesIr::from_baked(&fs::read(&payload).unwrap()).unwrap();
    assert_eq!(ir.game_id(), "test");
    assert!(ir.schema_by_name("body").is_some());
    assert!(String::from_utf8_lossy(&output.stdout).contains(&ir.fingerprint()));
    assert_eq!(fs::read_dir(payload.parent().unwrap()).unwrap().count(), 1);
}

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
    let payload = fixture.path().join("target/arena.json");
    let run = || {
        Command::new(env!("CARGO_BIN_EXE_tools"))
            .args(["rules", "bake", "--root"])
            .arg(fixture.path())
            .arg("--source")
            .arg(&source)
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
    assert!(!payload.exists());
    fs::create_dir_all(payload.parent().unwrap()).unwrap();
    fs::write(&payload, "previous arena").unwrap();
    assert!(!run().status.success());
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
    let payload = fixture.path().join("target/arena.json");
    for existing in [false, true] {
        if existing {
            fs::create_dir_all(payload.parent().unwrap()).unwrap();
            fs::write(&payload, "previous arena").unwrap();
        }
        let output = Command::new(env!("CARGO_BIN_EXE_tools"))
            .args(["rules", "bake", "--root"])
            .arg(fixture.path())
            .arg("--source")
            .arg(&source)
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
            assert_eq!(fs::read_to_string(&payload).unwrap(), "previous arena");
        } else {
            assert!(!payload.exists());
        }
    }
}
