use super::*;
use serde_json::json;
#[cfg(unix)]
mod black_box;

fn fixture() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    for folder in ["crates/demo", "fuzz", "editors/vscode", ".github/workflows"] {
        fs::create_dir_all(root.path().join(folder)).unwrap();
    }
    fs::write(
        root.path().join("Cargo.toml"),
        "[workspace]\nmembers = [\"crates/demo\"]\n[workspace.package]\nversion = \"1.2.3\"\n",
    )
    .unwrap();
    fs::write(
        root.path().join("crates/demo/Cargo.toml"),
        "[package]\nname = \"demo\"\nversion.workspace = true\n",
    )
    .unwrap();
    fs::write(
        root.path().join("fuzz/Cargo.toml"),
        "[dependencies]\ndemo = {path = \"../crates/demo\"}\n",
    )
    .unwrap();
    for file in ["Cargo.lock", "fuzz/Cargo.lock"] {
        fs::write(
            root.path().join(file),
            "[[package]]\nname = \"demo\"\nversion = \"1.2.3\"\n",
        )
        .unwrap();
    }
    report::write(
        &root.path().join("editors/vscode/package.json"),
        &json!({"version":"1.2.3"}),
    )
    .unwrap();
    report::write(
        &root.path().join("editors/vscode/package-lock.json"),
        &json!({"version":"1.2.3", "packages":{"":{"version":"1.2.3"}}}),
    )
    .unwrap();
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    fs::copy(
        repo.join("editors/vscode/server-distribution.json"),
        root.path().join("editors/vscode/server-distribution.json"),
    )
    .unwrap();
    fs::write(root.path().join(CI_WORKFLOW), "owned CI definition\n").unwrap();
    git(root.path(), &["init", "-b", "main"]).unwrap();
    git(root.path(), &["config", "user.name", "Fixture"]).unwrap();
    git(
        root.path(),
        &["config", "user.email", "fixture@example.invalid"],
    )
    .unwrap();
    git(root.path(), &["add", "."]).unwrap();
    git(root.path(), &["commit", "-m", "owned fixture"]).unwrap();
    root
}

fn vsix(path: &Path, version: &str, publisher: &str) {
    use std::io::Write;
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    writer
        .start_file(
            "extension/package.json",
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
    writer
        .write_all(
            serde_json::to_string(
                &json!({"name":"paradoxcode-vscode","publisher":publisher,"version":version}),
            )
            .unwrap()
            .as_bytes(),
        )
        .unwrap();
    fs::write(path, writer.finish().unwrap().into_inner()).unwrap();
}

fn receipt(root: &Path) -> CiReceipt {
    CiReceipt {
        schema: SCHEMA,
        repository: "owner/repo".into(),
        run_id: 7,
        run_attempt: 1,
        event: "pull_request".into(),
        head_sha: git(root, &["rev-parse", "HEAD"]).unwrap(),
        tested_sha: git(root, &["rev-parse", "HEAD"]).unwrap(),
        tree: git(root, &["rev-parse", "HEAD^{tree}"]).unwrap(),
        policy: policy(root).unwrap(),
        rustc: capture(process::command("rustc").arg("--version")).unwrap(),
        version: "1.2.3".into(),
        jobs: REQUIRED.iter().map(|s| (*s).into()).collect(),
        origin_run: 7,
        origin_attempt: 1,
        vsix: Asset {
            name: "paradoxcode-vscode-contract.vsix".into(),
            bytes: 1,
            sha256: "a".repeat(64),
        },
    }
}
fn run() -> Value {
    json!({"id":7,"run_attempt":1,"repository":{"full_name":"owner/repo"},"head_repository":{"full_name":"owner/repo"},"path":CI_WORKFLOW,"status":"completed","conclusion":"success","event":"pull_request","head_sha":"a".repeat(40)})
}

#[test]
fn equivalent_tree_survives_a_new_commit_id() {
    let root = fixture();
    let proof = receipt(root.path());
    git(
        root.path(),
        &["commit", "--allow-empty", "-m", "squash identity"],
    )
    .unwrap();
    assert_ne!(
        proof.tested_sha,
        git(root.path(), &["rev-parse", "HEAD"]).unwrap()
    );
    assert!(receipt_matches(&proof, root.path(), "owner/repo").is_ok());
}
#[test]
fn changed_source_requires_new_ci() {
    let root = fixture();
    let proof = receipt(root.path());
    fs::write(root.path().join("owned-source.txt"), "different behavior").unwrap();
    git(root.path(), &["add", "."]).unwrap();
    git(root.path(), &["commit", "-m", "changed"]).unwrap();
    assert!(receipt_matches(&proof, root.path(), "owner/repo").is_err());
}
#[test]
fn receipt_rejects_policy_toolchain_version_repository_and_missing_jobs() {
    let root = fixture();
    let original = receipt(root.path());
    for field in [
        "policy",
        "rustc",
        "version",
        "repository",
        "jobs",
        "schema",
        "origin",
    ] {
        let mut proof = original.clone();
        match field {
            "policy" => proof.policy = "b".repeat(64),
            "rustc" => proof.rustc = "different compiler".into(),
            "version" => proof.version = "1.2.4".into(),
            "repository" => proof.repository = "other/repo".into(),
            "jobs" => {
                proof.jobs.remove("rust");
            }
            "schema" => proof.schema = 99,
            _ => proof.origin_run = 0,
        }
        assert!(
            receipt_matches(&proof, root.path(), "owner/repo").is_err(),
            "{field}"
        );
    }
}
#[test]
fn required_jobs_cannot_be_skipped_or_failed() {
    let mut needs = json!({});
    for name in REQUIRED {
        needs[*name] = json!({"result":"success"});
    }
    assert!(strict_jobs(&needs).is_ok());
    for state in ["skipped", "failure", "cancelled", "pending"] {
        needs["rust"]["result"] = json!(state);
        assert!(strict_jobs(&needs).is_err(), "{state}");
    }
}
#[test]
fn evidence_rejects_forks_wrong_workflows_and_incomplete_runs() {
    assert!(github::validate_run(&run(), "owner/repo", CI_WORKFLOW).is_ok());
    for field in ["fork", "workflow", "status", "conclusion", "attempt", "sha"] {
        let mut r = run();
        match field {
            "fork" => r["head_repository"]["full_name"] = json!("fork/repo"),
            "workflow" => r["path"] = json!("other.yml"),
            "status" => r["status"] = json!("in_progress"),
            "conclusion" => r["conclusion"] = json!("failure"),
            "attempt" => r["run_attempt"] = json!(0),
            _ => r["head_sha"] = json!("invalid"),
        }
        assert!(
            github::validate_run(&r, "owner/repo", CI_WORKFLOW).is_err(),
            "{field}"
        );
    }
}
#[test]
fn workspace_and_fuzz_versions_must_agree() {
    let root = fixture();
    assert_eq!(workspace_version(root.path()).unwrap(), "1.2.3");
    fs::write(
        root.path().join("fuzz/Cargo.lock"),
        "[[package]]\nname = \"demo\"\nversion = \"1.2.2\"\n",
    )
    .unwrap();
    assert!(workspace_version(root.path()).is_err());
    fs::write(
        root.path().join("fuzz/Cargo.lock"),
        "[[package]]\nname = \"unrelated\"\nversion = \"1.2.3\"\n",
    )
    .unwrap();
    assert!(workspace_version(root.path()).is_err());
}
#[test]
fn npm_root_lock_version_is_required() {
    let root = fixture();
    let path = root.path().join("editors/vscode/package-lock.json");
    report::write(
        &path,
        &json!({"version":"1.2.3","packages":{"":{"version":"1.2.2"}}}),
    )
    .unwrap();
    assert!(workspace_version(root.path()).is_err());
}
#[test]
fn formal_versions_reject_preview_build_metadata_and_invalid_spelling() {
    for version in ["1.2.3-rc.1", "1.2.3+build", "01.2.3", "v1.2.3", "1.2"] {
        assert!(stable_version(version).is_err(), "{version}");
    }
    assert!(stable_version("0.5.2").is_ok());
}
#[test]
fn vsix_must_match_identity_and_version() {
    let root = fixture();
    let file = root.path().join("owned.vsix");
    vsix(&file, "1.2.3", "paradoxcode");
    assert!(verify_vsix(&file, "1.2.3").is_ok());
    assert!(verify_vsix(&file, "1.2.4").is_err());
    vsix(&file, "1.2.3", "other");
    assert!(verify_vsix(&file, "1.2.3").is_err());
}

fn zip_entries(entries: &[(&str, &[u8])]) -> Vec<u8> {
    use std::io::Write;
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (name, data) in entries {
        zip.start_file(*name, zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(data).unwrap();
    }
    zip.finish().unwrap().into_inner()
}
#[test]
fn downloaded_evidence_rejects_traversal_nested_and_oversized_files() {
    let root = fixture();
    for name in [
        "../escape",
        "/absolute",
        "nested/file",
        "bad\\name",
        "C:escape",
    ] {
        assert!(
            github::unpack_flat(
                &zip_entries(&[(name, b"owned")]),
                &root.path().join("target/evidence"),
                1024
            )
            .is_err(),
            "{name}"
        );
    }
    assert!(
        github::unpack_flat(
            &zip_entries(&[("ci.json", b"long")]),
            &root.path().join("target/evidence"),
            2
        )
        .is_err()
    );
}
#[test]
fn downloaded_evidence_never_reuses_stale_missing_files() {
    let root = fixture();
    let dir = root.path().join("target/evidence");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("ci.json"), "old proof").unwrap();
    assert!(github::unpack_flat(&zip_entries(&[("other.json", b"new")]), &dir, 1024).is_err());
}
#[test]
fn release_notes_are_taken_from_the_selected_version_section() {
    let source = "# Changelog\n\n## [Unreleased]\nfuture\n\n## [1.2.3] - 2026-10-05\n\nactual features\n\n## [1.2.2] - 2026-10-01\nold\n";
    assert_eq!(
        promotion::notes(source, "1.2.3").unwrap(),
        "actual features\n"
    );
    assert!(promotion::notes(source, "1.2.4").is_err());
    assert!(promotion::notes("## [1.2.3]\nundated", "1.2.3").is_err());
}
#[test]
fn comparison_baseline_follows_publication_time_and_excludes_drafts_and_prereleases() {
    let releases = [
        json!({"tag_name":"v9.0.0", "published_at":"2026-10-05T12:00:00Z", "draft":true,"prerelease":false}),
        json!({"tag_name":"v2.0.0", "published_at":"2026-10-05T10:00:00Z", "draft":false,"prerelease":true}),
        json!({"tag_name":"v1.2.2", "published_at":"2026-10-01T10:00:00Z", "draft":false,"prerelease":false}),
        json!({"tag_name":"v1.2.3", "published_at":"2026-10-05T10:00:00Z", "draft":false,"prerelease":false}),
    ];
    assert_eq!(
        promotion::previous_tag(&releases).as_deref(),
        Some("v1.2.3")
    );
    assert_eq!(promotion::previous_tag(&releases[..2]), None);
}
#[test]
fn changed_draft_notes_are_rejected_without_overwrite() {
    let mut release =
        json!({"tag_name":"v1.2.3", "name":"v1.2.3", "prerelease":false,"body":"sealed notes\n"});
    assert!(promotion::remote_notes_match(&release, "v1.2.3", "sealed notes").is_ok());
    release["body"] = json!("changed notes");
    assert!(promotion::remote_notes_match(&release, "v1.2.3", "sealed notes").is_err());
}
fn candidate(root: &Path) -> (Candidate, std::path::PathBuf) {
    let dir = root.join("target/envelope");
    fs::create_dir_all(&dir).unwrap();
    let input = root.join("owned-native");
    fs::write(&input, b"owned native fixture").unwrap();
    let (limits, targets) = release::load_contract(root).unwrap();
    for target in targets {
        release::package_target("1.2.3", &target, &input, &dir, &limits).unwrap();
    }
    vsix(
        &dir.join("paradoxcode-vscode-1.2.3.vsix"),
        "1.2.3",
        "paradoxcode",
    );
    let mut assets = report::files(&dir)
        .unwrap()
        .iter()
        .map(|p| Asset::from_file(p).unwrap())
        .collect::<Vec<_>>();
    assets.sort_by(|a, b| a.name.cmp(&b.name));
    fs::write(dir.join("release-notes.md"), "owned notes").unwrap();
    let value = Candidate {
        schema: SCHEMA,
        repository: "owner/repo".into(),
        run_id: 20,
        run_attempt: 1,
        workflow_sha: "a".repeat(40),
        source_sha: "b".repeat(40),
        tree: "c".repeat(40),
        policy: "d".repeat(64),
        version: "1.2.3".into(),
        ci_run: 7,
        ci_attempt: 1,
        sealed_at: now().unwrap(),
        assets,
        notes: Asset::from_file(&dir.join("release-notes.md")).unwrap(),
    };
    report::write(
        &dir.join("candidate.json"),
        &serde_json::to_value(&value).unwrap(),
    )
    .unwrap();
    (value, dir)
}
#[test]
fn complete_candidate_is_verified_without_any_tag() {
    let root = fixture();
    let (candidate, dir) = candidate(root.path());
    assert!(promotion::verify_candidate(root.path(), &candidate, &dir).is_ok());
    assert!(git(root.path(), &["tag", "--list"]).unwrap().is_empty());
}
#[test]
fn modified_candidate_bytes_are_rejected() {
    let root = fixture();
    let (c, dir) = candidate(root.path());
    fs::write(dir.join(&c.assets[0].name), "different").unwrap();
    assert!(promotion::verify_candidate(root.path(), &c, &dir).is_err());
}
#[test]
fn incomplete_duplicate_and_extra_candidates_are_rejected() {
    let root = fixture();
    let (c, dir) = candidate(root.path());
    let mut bad = c.clone();
    bad.assets.pop();
    assert!(promotion::verify_candidate(root.path(), &bad, &dir).is_err());
    let mut bad = c.clone();
    bad.assets.push(bad.assets[0].clone());
    assert!(promotion::verify_candidate(root.path(), &bad, &dir).is_err());
    fs::write(dir.join("unexpected.txt"), "extra").unwrap();
    assert!(promotion::verify_candidate(root.path(), &c, &dir).is_err());
}
#[test]
fn candidate_producer_attempt_is_bound_to_the_manifest() {
    let root = fixture();
    let (c, _) = candidate(root.path());
    let mut producer = json!({"id":20,"run_attempt":1,"head_sha":"a".repeat(40),"event":"workflow_dispatch","head_branch":"main"});
    assert!(promotion::validate_candidate_run(&c, &producer, "owner/repo").is_ok());
    producer["run_attempt"] = json!(2);
    assert!(promotion::validate_candidate_run(&c, &producer, "owner/repo").is_err());
}
#[test]
fn draft_resume_accepts_only_matching_existing_assets() {
    let asset = Asset {
        name: "owned.zip".into(),
        bytes: 5,
        sha256: "a".repeat(64),
    };
    let mut r = json!({"assets":[{"name":"owned.zip","size":5,"state":"uploaded","digest":format!("sha256:{}",asset.sha256)}]});
    assert!(promotion::remote_assets_match(&r, std::slice::from_ref(&asset), true).is_ok());
    for key in ["digest", "size", "name", "state"] {
        let old = r["assets"][0][key].clone();
        r["assets"][0][key] = json!("different");
        assert!(
            promotion::remote_assets_match(&r, std::slice::from_ref(&asset), false).is_err(),
            "{key}"
        );
        r["assets"][0][key] = old;
    }
}
#[test]
fn incomplete_upload_can_resume_but_cannot_publish() {
    let asset = Asset {
        name: "owned.zip".into(),
        bytes: 5,
        sha256: "a".repeat(64),
    };
    let empty = json!({"assets":[]});
    assert!(promotion::remote_assets_match(&empty, std::slice::from_ref(&asset), false).is_ok());
    assert!(promotion::remote_assets_match(&empty, std::slice::from_ref(&asset), true).is_err());
}
