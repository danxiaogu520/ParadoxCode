//! File-level contracts for the rule authoring formatter.
use std::fs;
use std::process::Command;

#[test]
fn formatting_preflights_the_bundle_and_check_never_writes() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    fs::write(
        root.join("game.json"),
        r#"{"game_id":"test","source_format_version":13}"#,
    )
    .unwrap();
    let first = root.join("first.json");
    let original = r#"{"schemas":{"empty":{"open":false}}}"#;
    fs::write(&first, original).unwrap();
    fs::write(root.join("second.json"), "{").unwrap();
    let run = |options: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_rulec"))
            .arg("fmt")
            .arg(root)
            .args(options)
            .output()
            .unwrap()
            .status
    };
    assert!(!run(&[]).success());
    assert_eq!(fs::read_to_string(&first).unwrap(), original);
    fs::write(root.join("second.json"), "{}").unwrap();
    assert!(!run(&["--check"]).success());
    assert_eq!(fs::read_to_string(&first).unwrap(), original);
    assert!(run(&[]).success());
    assert!(run(&["--check"]).success());
    let compact = fs::read_to_string(&first).unwrap();
    assert!(!run(&["--expanded", "--check"]).success());
    assert_eq!(fs::read_to_string(&first).unwrap(), compact);
    assert!(run(&["--expanded"]).success());
    assert!(run(&["--expanded", "--check"]).success());
}
