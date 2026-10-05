//! Exercise the control CLI against a local fake GitHub service. These tests
//! observe tag/publication side effects; no real GitHub mutation is performed.
use super::*;
use std::{os::unix::fs::PermissionsExt, process::Command};

fn setup() -> (tempfile::TempDir, std::path::PathBuf) {
    let root = fixture();
    git(
        root.path(),
        &["remote", "add", "origin", root.path().to_str().unwrap()],
    )
    .unwrap();
    fs::create_dir_all(root.path().join("target/mock/bin")).unwrap();
    let mock = root.path().join("target/mock");
    let (mut candidate, envelope) = candidate(root.path());
    let proof = receipt(root.path());
    candidate.source_sha = proof.tested_sha.clone();
    candidate.tree = proof.tree.clone();
    candidate.workflow_sha = proof.tested_sha.clone();
    candidate.policy = proof.policy.clone();
    report::write(
        &envelope.join("candidate.json"),
        &serde_json::to_value(&candidate).unwrap(),
    )
    .unwrap();
    let mut proof = proof;
    proof.event = "workflow_dispatch".into();
    proof.vsix = candidate
        .assets
        .iter()
        .find(|asset| asset.name.ends_with(".vsix"))
        .unwrap()
        .clone();
    proof.vsix.name = "paradoxcode-vscode-contract.vsix".into();
    let ci_zip = zip_entries(&[("ci.json", serde_json::to_string(&proof).unwrap().as_bytes())]);
    fs::write(mock.join("ci.zip"), &ci_zip).unwrap();
    let files = report::files(&envelope).unwrap();
    let data = files
        .iter()
        .map(|p| {
            (
                p.file_name().unwrap().to_str().unwrap().to_owned(),
                fs::read(p).unwrap(),
            )
        })
        .collect::<Vec<_>>();
    let candidate_zip = zip_entries(
        &data
            .iter()
            .map(|(n, b)| (n.as_str(), b.as_slice()))
            .collect::<Vec<_>>(),
    );
    fs::write(mock.join("candidate.zip"), &candidate_zip).unwrap();
    let ci_run = json!({"id":7,"run_attempt":1,"repository":{"full_name":"owner/repo"},"head_repository":{"full_name":"owner/repo"},"path":CI_WORKFLOW,"status":"completed","conclusion":"success","event":"workflow_dispatch","head_branch":"main","head_sha":proof.head_sha});
    let candidate_run = json!({"id":20,"run_attempt":1,"repository":{"full_name":"owner/repo"},"head_repository":{"full_name":"owner/repo"},"path":CANDIDATE_WORKFLOW,"status":"completed","conclusion":"success","event":"workflow_dispatch","head_branch":"main","head_sha":candidate.workflow_sha});
    let artifacts = |id: u64, name: &str, artifact_id: u64, bytes: &[u8]| json!({"total_count":1,"artifacts":[{"id":artifact_id,"name":name,"expired":false,"size_in_bytes":bytes.len(),"digest":format!("sha256:{}",report::hash(bytes)),"workflow_run":{"id":id,"head_sha":candidate.source_sha}}]});
    report::write(&mock.join("model.json"), &json!({"sha":candidate.source_sha,"tree":candidate.tree,"candidate_run":candidate_run,"ci_run":ci_run,"candidate_artifacts":artifacts(20,"release-candidate-1",200,&candidate_zip),"ci_artifacts":artifacts(7,"ci-evidence-1",70,&ci_zip),"assets":candidate.assets,"notes":fs::read_to_string(envelope.join("release-notes.md")).unwrap()})).unwrap();
    let python =
        capture(process::command("python3").args(["-c", "import sys; print(sys.executable)"]))
            .unwrap();
    let script = format!(
        "#!{python}\n{}",
        r#"
import json, os, subprocess, sys
from pathlib import Path
root = Path(os.environ['MOCK_ROOT'])
model = json.loads((root / 'model.json').read_text())
state_path = root / 'state.json'
state = json.loads(state_path.read_text()) if state_path.exists() else {'draft': None, 'tag':None, 'assets': [], 'mutations': []}
args = sys.argv[1:]
def save(): state_path.write_text(json.dumps(state))
def emit(value): print(json.dumps(value)); sys.exit(0)
def absent(): print(json.dumps({'message':'Not Found','status':'404'})); sys.exit(1)
def release(): return {'id':100,'draft':state['draft'],'immutable':state['draft'] is False,'assets':state['assets'],'tag_name':state['tag'],'name':state['tag'],'prerelease':False,'body':model['notes'],'target_commitish':model['sha']}
if args[0] == 'api':
    endpoint = args[1]
    if endpoint == 'graphql':
        tag = next(a[4:] for a in args if a.startswith('tag='))
        emit({'data':{'repository':{'release':{'databaseId':100} if state['tag'] == tag else None}}})
    if '/actions/runs/20/artifacts' in endpoint: emit(model['candidate_artifacts'])
    if '/actions/runs/7/artifacts' in endpoint: emit(model['ci_artifacts'])
    if '/actions/runs/20' in endpoint: emit(model['candidate_run'])
    if '/actions/runs/7' in endpoint: emit(model['ci_run'])
    if '/actions/artifacts/200/zip' in endpoint: sys.stdout.buffer.write((root/'candidate.zip').read_bytes()); sys.exit(0)
    if '/actions/artifacts/70/zip' in endpoint: sys.stdout.buffer.write((root/'ci.zip').read_bytes()); sys.exit(0)
    if '/actions/workflows/ci.yml/runs?' in endpoint:
        assert 'event=workflow_dispatch' in endpoint and 'branch=main' in endpoint
        assert ('head_sha=' + model['sha']) in endpoint
        emit({'workflow_runs':[model['ci_run']]})
    if '/git/commits/' in endpoint: emit({'tree':{'sha':model['tree']}})
    if '/git/ref/tags/' in endpoint:
        result = subprocess.run(['git','rev-parse','refs/tags/v1.2.3'], cwd=os.environ['SOURCE_ROOT'], capture_output=True, text=True)
        if result.returncode: absent()
        emit({'object':{'type':'tag','sha':result.stdout.strip()}})
    if '/git/tags/' in endpoint: emit({'object':{'type':'commit','sha':model['sha']}})
    if '/releases/tags/' in endpoint:
        if state['draft'] is not False or state['tag'] != endpoint.rsplit('/',1)[1]: absent()
        emit(release())
    if endpoint.endswith('/releases/100'): emit(release())
    raise SystemExit('unmodeled API: ' + endpoint)
if args[:2] == ['release','create']:
    state['draft'] = True; state['tag'] = args[2]; state['mutations'].append('create'); save(); sys.exit(0)
if args[:2] == ['release','upload']:
    assert args[2] == state['tag']
    assert subprocess.run(['git','rev-parse','--verify','refs/tags/v1.2.3'],cwd=os.environ['SOURCE_ROOT'],capture_output=True).returncode != 0, 'formal tag created before all uploads completed'
    if os.environ.get('INTERRUPT_UPLOAD') == '1' and not state.get('interrupted'):
        state['interrupted'] = True; save(); raise SystemExit('simulated transport interruption')
    name = Path(args[3]).name
    asset = next(a for a in model['assets'] if a['name'] == name)
    state['assets'].append({'name':name,'size':asset['bytes'],'state':'uploaded','digest':'sha256:'+asset['sha256']})
    state['mutations'].append('upload:'+name); save()
    change = os.environ.get('CHANGE_AFTER_UPLOAD')
    if len(state['assets']) == len(model['assets']) and change:
        if change == 'ci_pending': model['ci_run']['status'] = 'in_progress'
        elif change == 'ci_failure': model['ci_run']['conclusion'] = 'failure'
        elif change == 'ci_attempt': model['ci_run']['run_attempt'] += 1
        elif change == 'candidate_pending': model['candidate_run']['status'] = 'in_progress'
        elif change == 'candidate_attempt': model['candidate_run']['run_attempt'] += 1
        else: raise SystemExit('unknown simulated evidence change: ' + change)
        (root/'model.json').write_text(json.dumps(model))
    sys.exit(0)
if args[:2] == ['release','edit']:
    assert len(state['assets']) == 11
    assert subprocess.run(['git','rev-parse','--verify','refs/tags/v1.2.3'],cwd=os.environ['SOURCE_ROOT'],capture_output=True).returncode == 0
    if os.environ.get('INTERRUPT_PUBLISH') == '1' and not state.get('publish_interrupted'):
        state['publish_interrupted'] = True; save(); raise SystemExit('simulated final publication interruption')
    state['tag'] = args[args.index('--tag')+1]; state['draft'] = False; state['mutations'].append('publish'); save(); sys.exit(0)
raise SystemExit('unmodeled command: ' + repr(args))
"#
    );
    fs::write(mock.join("bin/gh"), script).unwrap();
    fs::write(mock.join("bin/npm"), "#!/bin/sh\nif [ -n \"$GH_TOKEN\" ] || [ -n \"$GITHUB_TOKEN\" ]; then exit 9; fi\nif [ \"$AUDIT_FAIL\" = 1 ]; then echo 'simulated advisory or unavailable audit endpoint' >&2; exit 1; fi\nexit 0\n").unwrap();
    for name in ["gh", "npm"] {
        fs::set_permissions(
            mock.join("bin").join(name),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    (root, mock)
}
fn command(root: &Path, mock: &Path) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    // Invoke the same public dispatch as the control executable through a tiny
    // dedicated unit-test child, keeping per-test environment out of the parent.
    command
        .args([
            "delivery::tests::black_box::child_dispatch",
            "--exact",
            "--ignored",
            "--nocapture",
        ])
        .current_dir(root)
        .env("CHILD_DISPATCH", "1")
        .env("MOCK_ROOT", mock)
        .env("SOURCE_ROOT", root)
        .env(
            "PATH",
            format!(
                "{}:{}",
                mock.join("bin").display(),
                std::env::var("PATH").unwrap()
            ),
        )
        .env("GITHUB_REPOSITORY", "owner/repo")
        .env("GITHUB_EVENT_NAME", "workflow_dispatch")
        .env("GITHUB_REF", "refs/heads/main")
        .env("GH_TOKEN", "mock-publication-credential")
        .env("GITHUB_TOKEN", "mock-publication-credential");
    command
}
#[test]
#[ignore = "child process entry for environment-isolated control tests"]
fn child_dispatch() {
    assert_eq!(std::env::var("CHILD_DISPATCH").unwrap(), "1");
    let args = vec![
        "--root".into(),
        std::env::var("SOURCE_ROOT").unwrap(),
        "--candidate-run".into(),
        "20".into(),
    ];
    let args = Args::parse(&args, &["--root", "--candidate-run"], &[]).unwrap();
    let operation = std::env::var("CONTROL_COMMAND").unwrap_or_else(|_| "promote".into());
    match execute(&operation, &args) {
        Ok(_) => {}
        Err(error) => panic!("{error}"),
    }
}
#[test]
fn audit_failure_creates_no_tag_and_no_draft() {
    let (root, mock) = setup();
    let output = command(root.path(), &mock)
        .env("AUDIT_FAIL", "1")
        .output()
        .unwrap();
    assert!(!output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("simulated advisory or unavailable audit endpoint"),
        "{output:?}"
    );
    assert!(git(root.path(), &["tag", "--list"]).unwrap().is_empty());
    assert!(!mock.join("state.json").exists());
}
#[test]
fn interrupted_upload_resumes_identical_bytes_and_published_retry_is_idempotent() {
    let (root, mock) = setup();
    let output = command(root.path(), &mock)
        .env("INTERRUPT_UPLOAD", "1")
        .output()
        .unwrap();
    assert!(!output.status.success(), "{output:?}");
    assert!(
        git(root.path(), &["tag", "--list"]).unwrap().is_empty(),
        "{output:?}"
    );
    let output = command(root.path(), &mock).output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let state = report::json(&mock.join("state.json")).unwrap();
    assert_eq!(state["draft"], false);
    assert_eq!(state["assets"].as_array().unwrap().len(), 11);
    let mutations = state["mutations"].clone();
    let output = command(root.path(), &mock)
        .env("AUDIT_FAIL", "1")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        report::json(&mock.join("state.json")).unwrap()["mutations"],
        mutations
    );
}

#[test]
fn publication_interruption_after_tag_resumes_without_reupload() {
    let (root, mock) = setup();
    let output = command(root.path(), &mock)
        .env("INTERRUPT_PUBLISH", "1")
        .output()
        .unwrap();
    assert!(!output.status.success(), "{output:?}");
    assert_eq!(git(root.path(), &["tag", "--list"]).unwrap(), "v1.2.3");
    let state = report::json(&mock.join("state.json")).unwrap();
    assert_eq!(state["draft"], true);
    assert_eq!(state["assets"].as_array().unwrap().len(), 11);
    let output = command(root.path(), &mock).output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let state = report::json(&mock.join("state.json")).unwrap();
    assert_eq!(state["draft"], false);
    assert_eq!(state["mutations"].as_array().unwrap().len(), 13);
}

#[test]
fn publication_rejects_quick_ci_non_main_and_non_manual_evidence() {
    for (field, value, error) in [
        (
            "path",
            ".github/workflows/quick-ci.yml",
            "unexpected evidence workflow",
        ),
        (
            "event",
            "push",
            "full CI evidence must be manually dispatched from main",
        ),
        (
            "event",
            "pull_request",
            "full CI evidence must be manually dispatched from main",
        ),
        (
            "event",
            "schedule",
            "full CI evidence must be manually dispatched from main",
        ),
        ("head_branch", "feature", "candidate CI must be a main run"),
        (
            "status",
            "in_progress",
            "latest workflow attempt must be complete and successful",
        ),
        (
            "conclusion",
            "failure",
            "latest workflow attempt must be complete and successful",
        ),
    ] {
        let (root, mock) = setup();
        let mut model = report::json(&mock.join("model.json")).unwrap();
        model["ci_run"][field] = json!(value);
        report::write(&mock.join("model.json"), &model).unwrap();
        let output = command(root.path(), &mock).output().unwrap();
        assert!(!output.status.success(), "{field}={value}: {output:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(error),
            "{field}={value}: {output:?}"
        );
        assert!(git(root.path(), &["tag", "--list"]).unwrap().is_empty());
        assert!(!mock.join("state.json").exists());
    }
}

#[test]
fn publication_rejects_receipts_from_a_different_source_or_reused_origin() {
    for field in ["head_sha", "tested_sha", "origin_run", "origin_attempt"] {
        let (root, mock) = setup();
        let mut proof = receipt(root.path());
        proof.event = "workflow_dispatch".into();
        match field {
            "head_sha" => proof.head_sha = "b".repeat(40),
            "tested_sha" => proof.tested_sha = "b".repeat(40),
            "origin_run" => proof.origin_run = 6,
            _ => proof.origin_attempt = 2,
        }
        let bytes = zip_entries(&[("ci.json", serde_json::to_string(&proof).unwrap().as_bytes())]);
        fs::write(mock.join("ci.zip"), &bytes).unwrap();
        let mut model = report::json(&mock.join("model.json")).unwrap();
        model["ci_artifacts"]["artifacts"][0]["size_in_bytes"] = json!(bytes.len());
        model["ci_artifacts"]["artifacts"][0]["digest"] =
            json!(format!("sha256:{}", report::hash(&bytes)));
        report::write(&mock.join("model.json"), &model).unwrap();
        let output = command(root.path(), &mock).output().unwrap();
        assert!(!output.status.success(), "{field}: {output:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("CI receipt run identity differs"),
            "{field}: {output:?}"
        );
        assert!(git(root.path(), &["tag", "--list"]).unwrap().is_empty());
        assert!(!mock.join("state.json").exists());
    }
}

#[test]
fn publication_rejects_a_full_run_for_another_commit_even_with_the_same_tree() {
    let (root, mock) = setup();
    let mut proof = receipt(root.path());
    proof.event = "workflow_dispatch".into();
    proof.head_sha = "b".repeat(40);
    proof.tested_sha = proof.head_sha.clone();
    let bytes = zip_entries(&[("ci.json", serde_json::to_string(&proof).unwrap().as_bytes())]);
    fs::write(mock.join("ci.zip"), &bytes).unwrap();
    let mut model = report::json(&mock.join("model.json")).unwrap();
    model["ci_run"]["head_sha"] = json!(proof.head_sha);
    model["ci_artifacts"]["artifacts"][0]["workflow_run"]["head_sha"] = json!(proof.head_sha);
    model["ci_artifacts"]["artifacts"][0]["size_in_bytes"] = json!(bytes.len());
    model["ci_artifacts"]["artifacts"][0]["digest"] =
        json!(format!("sha256:{}", report::hash(&bytes)));
    report::write(&mock.join("model.json"), &model).unwrap();
    let output = command(root.path(), &mock).output().unwrap();
    assert!(!output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("Full CI receipt is for a different source commit"),
        "{output:?}"
    );
    assert!(git(root.path(), &["tag", "--list"]).unwrap().is_empty());
    assert!(!mock.join("state.json").exists());
}

#[test]
fn superseded_evidence_during_staging_never_reserves_or_publishes_a_tag() {
    for (change, error) in [
        (
            "ci_pending",
            "latest workflow attempt must be complete and successful",
        ),
        (
            "ci_failure",
            "latest workflow attempt must be complete and successful",
        ),
        (
            "ci_attempt",
            "expected one evidence artifact for this run attempt",
        ),
        (
            "candidate_pending",
            "latest workflow attempt must be complete and successful",
        ),
        (
            "candidate_attempt",
            "candidate belongs to a different producer run",
        ),
    ] {
        let (root, mock) = setup();
        let output = command(root.path(), &mock)
            .env("CHANGE_AFTER_UPLOAD", change)
            .output()
            .unwrap();
        assert!(!output.status.success(), "{change}: {output:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(error),
            "{change}: {output:?}"
        );
        assert!(git(root.path(), &["tag", "--list"]).unwrap().is_empty());
        let state = report::json(&mock.join("state.json")).unwrap();
        assert_eq!(state["draft"], true);
        assert_eq!(state["tag"], "candidate-20-1");
        assert_eq!(state["assets"].as_array().unwrap().len(), 11);
        assert!(
            state["mutations"]
                .as_array()
                .unwrap()
                .iter()
                .all(|mutation| {
                    let mutation = mutation.as_str().unwrap();
                    mutation == "create" || mutation.starts_with("upload:")
                })
        );
        assert_eq!(
            git(root.path(), &["worktree", "list", "--porcelain"])
                .unwrap()
                .matches("worktree ")
                .count(),
            1
        );
    }
}

#[test]
fn sealed_vsix_must_match_receipt_bytes_despite_normalized_filename() {
    let root = fixture();
    let (candidate, _) = candidate(root.path());
    let mut proof = receipt(root.path());
    proof.vsix = candidate
        .assets
        .iter()
        .find(|asset| asset.name.ends_with(".vsix"))
        .unwrap()
        .clone();
    proof.vsix.name = "paradoxcode-vscode-contract.vsix".into();
    assert!(promotion::verify_ci_vsix(&proof, &candidate.assets, &candidate.version).is_ok());
    let mut wrong_size = proof.clone();
    wrong_size.vsix.bytes += 1;
    assert!(promotion::verify_ci_vsix(&wrong_size, &candidate.assets, &candidate.version).is_err());
    proof.vsix.sha256 = "b".repeat(64);
    assert!(promotion::verify_ci_vsix(&proof, &candidate.assets, &candidate.version).is_err());
}

#[test]
fn publication_rejects_a_vsix_not_bound_to_the_selected_full_receipt() {
    let (root, mock) = setup();
    let mut proof = receipt(root.path());
    proof.event = "workflow_dispatch".into();
    let bytes = zip_entries(&[("ci.json", serde_json::to_string(&proof).unwrap().as_bytes())]);
    fs::write(mock.join("ci.zip"), &bytes).unwrap();
    let mut model = report::json(&mock.join("model.json")).unwrap();
    model["ci_artifacts"]["artifacts"][0]["size_in_bytes"] = json!(bytes.len());
    model["ci_artifacts"]["artifacts"][0]["digest"] =
        json!(format!("sha256:{}", report::hash(&bytes)));
    report::write(&mock.join("model.json"), &model).unwrap();
    let output = command(root.path(), &mock).output().unwrap();
    assert!(!output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("candidate VSIX differs from its Full CI receipt"),
        "{output:?}"
    );
    assert!(git(root.path(), &["tag", "--list"]).unwrap().is_empty());
    assert!(!mock.join("state.json").exists());
}

#[test]
fn sealing_rejects_vsix_bytes_from_a_different_full_receipt() {
    let (root, mock) = setup();
    let model = report::json(&mock.join("model.json")).unwrap();
    for asset in model["assets"].as_array().unwrap() {
        let name = asset["name"].as_str().unwrap();
        let folder = if name.ends_with(".vsix") {
            "extension"
        } else {
            "server"
        };
        let directory = root.path().join("target/dist").join(folder);
        fs::create_dir_all(&directory).unwrap();
        fs::copy(
            root.path().join("target/envelope").join(name),
            directory.join(name),
        )
        .unwrap();
    }
    // Keep a valid same-version VSIX, but change its bytes after CI validated it.
    let path = root
        .path()
        .join("target/dist/extension/paradoxcode-vscode-1.2.3.vsix");
    let mut bytes = fs::read(&path).unwrap();
    bytes.extend_from_slice(b"different archive bytes");
    fs::write(path, bytes).unwrap();
    let output = command(root.path(), &mock)
        .env("CONTROL_COMMAND", "seal-candidate")
        .env("GITHUB_SHA", model["sha"].as_str().unwrap())
        .env("GITHUB_RUN_ID", "20")
        .env("GITHUB_RUN_ATTEMPT", "1")
        .env(
            "NEEDS_JSON",
            serde_json::to_string(&json!({
                "resolve":{"result":"success"}, "audit":{"result":"success"},
                "build":{"result":"success"}, "vscode":{"result":"success"}
            }))
            .unwrap(),
        )
        .output()
        .unwrap();
    assert!(!output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("candidate VSIX differs from its Full CI receipt"),
        "{output:?}"
    );
    assert!(!root.path().join("target/candidate").exists());
    assert!(git(root.path(), &["tag", "--list"]).unwrap().is_empty());
    assert!(!mock.join("state.json").exists());
}
