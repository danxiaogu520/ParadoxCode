//! Read-only GitHub evidence lookup. Missing, expired or mismatched evidence
//! falls back to full CI; publication never executes downloaded artifact code.
use super::*;

pub(super) fn gh(args: &[&str]) -> Result<String, String> {
    capture(process::command("gh").args(args))
}
pub(super) fn api(path: &str) -> Result<Value, String> {
    serde_json::from_str(&gh(&["api", path])?).map_err(|e| e.to_string())
}
pub(super) fn run(repository: &str, id: u64, workflow: &str) -> Result<Value, String> {
    let run = api(&format!("repos/{repository}/actions/runs/{id}"))?;
    validate_run(&run, repository, workflow)?;
    Ok(run)
}
pub(super) fn validate_run(run: &Value, repository: &str, workflow: &str) -> Result<(), String> {
    require(
        run["id"].as_u64().is_some_and(|n| n > 0)
            && run["run_attempt"].as_u64().is_some_and(|n| n > 0),
        "invalid workflow run identity",
    )?;
    require(
        run["repository"]["full_name"] == repository
            && run["head_repository"]["full_name"] == repository,
        "workflow run must belong to this repository, not a fork",
    )?;
    require(
        run["path"]
            .as_str()
            .is_some_and(|p| p.split('@').next() == Some(workflow)),
        "unexpected evidence workflow",
    )?;
    require(
        run["status"] == "completed" && run["conclusion"] == "success",
        "latest workflow attempt must be complete and successful",
    )?;
    require(
        run["head_sha"].as_str().is_some_and(|s| hex(s, 40)),
        "invalid workflow head SHA",
    )
}

pub(super) fn artifact(
    repository: &str,
    run: &Value,
    name: &str,
    destination: &Path,
    max_bytes: u64,
) -> Result<(), String> {
    let id = run["id"].as_u64().ok_or("missing run id")?;
    let response = api(&format!(
        "repos/{repository}/actions/runs/{id}/artifacts?per_page=100"
    ))?;
    let artifacts = response["artifacts"]
        .as_array()
        .ok_or("invalid artifact response")?;
    require(
        response["total_count"].as_u64() == Some(artifacts.len() as u64),
        "artifact listing is incomplete",
    )?;
    let matches = artifacts
        .iter()
        .filter(|a| a["name"] == name)
        .collect::<Vec<_>>();
    require(
        matches.len() == 1,
        "expected one evidence artifact for this run attempt",
    )?;
    let item = matches[0];
    require(
        item["expired"] == false
            && item["size_in_bytes"]
                .as_u64()
                .is_some_and(|n| n > 0 && n <= max_bytes),
        "evidence artifact is expired or exceeds its bound",
    )?;
    require(
        item["workflow_run"]["id"] == id && item["workflow_run"]["head_sha"] == run["head_sha"],
        "artifact producer differs from workflow run",
    )?;
    let artifact_id = item["id"].as_u64().ok_or("missing artifact id")?;
    let bytes = process::capture(process::command("gh").args([
        "api",
        &format!("repos/{repository}/actions/artifacts/{artifact_id}/zip"),
        "--allow-escape-sequences",
    ]))?
    .stdout;
    require(
        bytes.len() as u64 <= max_bytes
            && item["digest"] == format!("sha256:{}", report::hash(&bytes)),
        "downloaded artifact differs from GitHub digest",
    )?;
    unpack_flat(&bytes, destination, max_bytes)
}

pub(super) fn unpack_flat(bytes: &[u8], destination: &Path, max_bytes: u64) -> Result<(), String> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|e| e.to_string())?;
    require(
        !zip.is_empty() && zip.len() <= 32,
        "unexpected evidence archive entry count",
    )?;
    let mut names = BTreeSet::new();
    let mut total = 0u64;
    fs::create_dir_all(destination).map_err(|e| e.to_string())?;
    for index in 0..zip.len() {
        let mut file = zip.by_index(index).map_err(|e| e.to_string())?;
        require(
            plain_name(file.name()) && !file.is_dir() && !file.is_symlink(),
            "evidence archive must contain flat regular files",
        )?;
        require(
            names.insert(file.name().to_owned()),
            "duplicate evidence archive entry",
        )?;
        total = total
            .checked_add(file.size())
            .ok_or("evidence archive size overflow")?;
        require(
            total <= max_bytes,
            "expanded evidence archive exceeds its bound",
        )?;
        let mut data = Vec::new();
        file.read_to_end(&mut data).map_err(|e| e.to_string())?;
        require(data.len() as u64 == file.size(), "truncated evidence entry")?;
        release::atomic_write(&destination.join(file.name()), &data).map_err(|e| e.to_string())?;
    }
    let actual = report::files(destination)?
        .iter()
        .map(|p| {
            p.strip_prefix(destination)
                .map(|p| p.to_string_lossy().into_owned())
                .map_err(|e| e.to_string())
        })
        .collect::<Result<BTreeSet<_>, _>>()?;
    require(
        actual == names,
        "artifact destination contains stale or unexpected files",
    )?;
    Ok(())
}

pub(super) fn fetch_receipt(
    root: &Path,
    repository: &str,
    id: u64,
) -> Result<(CiReceipt, Value), String> {
    let run = run(repository, id, CI_WORKFLOW)?;
    require(
        matches!(run["event"].as_str(), Some("push" | "pull_request")),
        "unexpected CI event",
    )?;
    let attempt = run["run_attempt"].as_u64().ok_or("missing attempt")?;
    let directory = root.join(format!("target/ci/evidence/{id}-{attempt}"));
    artifact(
        repository,
        &run,
        &format!("ci-evidence-{attempt}"),
        &directory,
        1024 * 1024,
    )?;
    let receipt: CiReceipt = serde_json::from_value(report::json(&directory.join("ci.json"))?)
        .map_err(|e| e.to_string())?;
    require(
        receipt.run_id == id
            && receipt.run_attempt == attempt
            && receipt.head_sha == run["head_sha"]
            && receipt.event == run["event"],
        "CI receipt run identity differs",
    )?;
    let commit = api(&format!(
        "repos/{repository}/git/commits/{}",
        receipt.tested_sha
    ))?;
    require(
        commit["tree"]["sha"] == receipt.tree,
        "CI receipt differs from the actual tested Git tree",
    )?;
    receipt_matches(&receipt, root, repository)?;
    Ok((receipt, run))
}

fn find_reusable(root: &Path, repository: &str, sha: &str) -> Result<CiReceipt, String> {
    let response = api(&format!(
        "repos/{repository}/commits/{sha}/pulls?per_page=100"
    ))?;
    let prs = response
        .as_array()
        .ok_or("invalid associated PR response")?;
    for pr in prs {
        if pr["merged_at"].is_null()
            || pr["merge_commit_sha"] != sha
            || pr["base"]["ref"] != "main"
            || pr["head"]["repo"]["full_name"] != repository
            || pr["base"]["repo"]["full_name"] != repository
        {
            continue;
        }
        let head = pr["head"]["sha"]
            .as_str()
            .filter(|s| hex(s, 40))
            .ok_or("invalid PR head")?;
        let runs = api(&format!(
            "repos/{repository}/actions/workflows/ci.yml/runs?event=pull_request&head_sha={head}&per_page=10"
        ))?;
        // Only the latest run/attempt is eligible. Never resurrect an earlier green
        // run after a newer failed, cancelled or pending run.
        let latest = runs["workflow_runs"]
            .as_array()
            .and_then(|r| r.first())
            .ok_or("no successful PR CI evidence")?;
        let id = latest["id"].as_u64().ok_or("missing CI run id")?;
        let (receipt, _) = fetch_receipt(root, repository, id)?;
        require(
            receipt.origin_run == id && receipt.origin_attempt == receipt.run_attempt,
            "PR evidence must come from a complete CI run",
        )?;
        return Ok(receipt);
    }
    Err("no eligible same-repository merged PR".into())
}

pub(super) fn ci_plan(root: &Path) -> Result<String, String> {
    let repository = env("GITHUB_REPOSITORY")?;
    let sha = git(root, &["rev-parse", "HEAD"])?;
    let reuse = if env("GITHUB_EVENT_NAME")? == "push" && env("GITHUB_REF")? == "refs/heads/main" {
        match find_reusable(root, &repository, &sha) {
            Ok(receipt) => Some(receipt),
            Err(error) => {
                eprintln!("Full CI required: {error}");
                None
            }
        }
    } else {
        None
    };
    output(&[
        ("full", reuse.is_none().to_string()),
        (
            "reuse_run",
            reuse.as_ref().map_or("0".into(), |r| r.run_id.to_string()),
        ),
    ])?;
    Ok(reuse.map_or("complete CI required".into(), |r| {
        format!("same tested tree and policy; reuse CI run {}", r.run_id)
    }))
}

pub(super) fn ci_receipt(root: &Path, args: &Args) -> Result<String, String> {
    let repository = env("GITHUB_REPOSITORY")?;
    let needs: Value = serde_json::from_str(&env("NEEDS_JSON")?).map_err(|e| e.to_string())?;
    require(needs["plan"]["result"] == "success", "CI plan must succeed")?;
    let reuse_id = args
        .get("--reuse-run")
        .unwrap_or("0")
        .parse::<u64>()
        .map_err(|e| e.to_string())?;
    require(
        needs["plan"]["outputs"]["reuse_run"]
            .as_str()
            .and_then(|s| s.parse::<u64>().ok())
            == Some(reuse_id)
            && needs["plan"]["outputs"]["full"] == if reuse_id == 0 { "true" } else { "false" },
        "CI receipt decision differs from its plan",
    )?;
    let (jobs, origin_run, origin_attempt, vsix) = if reuse_id > 0 {
        require(
            REQUIRED
                .iter()
                .all(|name| needs[*name]["result"] == "skipped"),
            "reuse cannot hide a partially executed or failed CI job",
        )?;
        let (receipt, _) = fetch_receipt(root, &repository, reuse_id)?;
        (
            receipt.jobs,
            receipt.origin_run,
            receipt.origin_attempt,
            receipt.vsix,
        )
    } else {
        let jobs = strict_jobs(&needs)?;
        let path = args.path("--vsix").ok_or("missing --vsix")?;
        let version = workspace_version(root)?;
        verify_vsix(&path, &version)?;
        (
            jobs,
            number("GITHUB_RUN_ID")?,
            number("GITHUB_RUN_ATTEMPT")?,
            Asset::from_file(&path)?,
        )
    };
    let receipt = CiReceipt {
        schema: SCHEMA,
        repository,
        run_id: number("GITHUB_RUN_ID")?,
        run_attempt: number("GITHUB_RUN_ATTEMPT")?,
        event: env("GITHUB_EVENT_NAME")?,
        head_sha: env("GITHUB_SHA")?,
        tested_sha: git(root, &["rev-parse", "HEAD"])?,
        tree: git(root, &["rev-parse", "HEAD^{tree}"])?,
        policy: policy(root)?,
        rustc: capture(process::command("rustc").arg("--version"))?,
        version: workspace_version(root)?,
        jobs,
        origin_run,
        origin_attempt,
        vsix,
    };
    // The Actions run's head_sha is the PR head (not necessarily checkout's
    // synthetic merge SHA). Bind both identities using the event payload.
    let mut receipt = receipt;
    if receipt.event == "pull_request" {
        let event = report::json(Path::new(&env("GITHUB_EVENT_PATH")?))?;
        receipt.head_sha = event["pull_request"]["head"]["sha"]
            .as_str()
            .ok_or("missing PR head SHA")?
            .into();
    }
    let destination = report::output(
        root,
        &args
            .path("--output")
            .unwrap_or_else(|| root.join("target/ci/ci.json")),
        "",
    )?;
    report::write(
        &destination,
        &serde_json::to_value(receipt).map_err(|e| e.to_string())?,
    )?;
    Ok("required CI evidence recorded".into())
}

pub(super) fn successful_ci(root: &Path, repository: &str, sha: &str) -> Result<CiReceipt, String> {
    let runs = api(&format!(
        "repos/{repository}/actions/workflows/ci.yml/runs?event=push&head_sha={sha}&per_page=10"
    ))?;
    let latest = runs["workflow_runs"]
        .as_array()
        .and_then(|r| r.first())
        .ok_or("source commit has no main CI receipt")?;
    require(
        latest["head_branch"] == "main",
        "candidate CI must be a main run",
    )?;
    let id = latest["id"].as_u64().ok_or("missing source CI run")?;
    let (receipt, _) = fetch_receipt(root, repository, id)?;
    require(
        receipt.tested_sha == sha,
        "main CI receipt is for a different commit",
    )?;
    Ok(receipt)
}

pub(super) fn candidate_plan(root: &Path, args: &Args) -> Result<String, String> {
    require(
        env("GITHUB_EVENT_NAME")? == "workflow_dispatch" && env("GITHUB_REF")? == "refs/heads/main",
        "candidate preparation must use the trusted main workflow",
    )?;
    let repository = env("GITHUB_REPOSITORY")?;
    let reference = args.get("--ref").unwrap_or("main");
    require(
        reference == "main" || hex(reference, 40),
        "candidate source must be main or an exact commit SHA",
    )?;
    git(root, &["fetch", "origin", "main"])?;
    let workflow_sha = env("GITHUB_SHA")?;
    let reference = if reference == "main" {
        workflow_sha.as_str()
    } else {
        reference
    };
    let sha = git(root, &["rev-parse", &format!("{reference}^{{commit}}")])?;
    require(hex(&sha, 40), "candidate must resolve to a commit")?;
    git(root, &["merge-base", "--is-ancestor", &sha, "origin/main"])?;
    // A detached worktree reads exactly the reviewed source without executing it.
    let temp = tempfile::tempdir_in(root.join("target")).map_err(|e| e.to_string())?;
    let source = temp.path().join("source");
    git(
        root,
        &[
            "worktree",
            "add",
            "--detach",
            source.to_str().ok_or("non-UTF8 source path")?,
            &sha,
        ],
    )?;
    let result = (|| {
        let version = workspace_version(&source)?;
        require(
            policy(&source)? == policy_at(root, &workflow_sha)?,
            "candidate controls differ from the dispatched main workflow",
        )?;
        let receipt = successful_ci(&source, &repository, &sha)?;
        promotion::available_version(&repository, &version, &sha)?;
        output(&[
            ("source_sha", sha.clone()),
            ("version", version),
            ("ci_run", receipt.origin_run.to_string()),
            ("ci_attempt", receipt.origin_attempt.to_string()),
        ])?;
        Ok("candidate source and prior CI verified; no tag created".into())
    })();
    let cleanup = git(
        root,
        &[
            "worktree",
            "remove",
            "--force",
            source.to_str().ok_or("non-UTF8 source path")?,
        ],
    );
    match (result, cleanup) {
        (Ok(value), Ok(_)) => Ok(value),
        (Err(error), _) | (_, Err(error)) => Err(error),
    }
}

pub(super) fn candidate_vsix(root: &Path, args: &Args) -> Result<String, String> {
    let repository = env("GITHUB_REPOSITORY")?;
    let id = args
        .required("--ci-run")?
        .parse::<u64>()
        .map_err(|e| e.to_string())?;
    let (receipt, run) = fetch_receipt(root, &repository, id)?;
    require(
        receipt.origin_run == id,
        "extension must come from the full CI origin",
    )?;
    let destination = report::output(
        root,
        &args
            .path("--directory")
            .unwrap_or_else(|| root.join("target/dist/extension")),
        "",
    )?;
    artifact(
        &repository,
        &run,
        &format!("ci-vsix-{}", receipt.run_attempt),
        &destination,
        64 * 1024 * 1024,
    )?;
    let files = report::files(&destination)?;
    require(
        files.len() == 1,
        "CI VSIX artifact must contain exactly one file",
    )?;
    receipt.vsix.verify(&destination)?;
    verify_vsix(&destination.join(&receipt.vsix.name), &receipt.version)?;
    let target = destination.join(format!("paradoxcode-vscode-{}.vsix", receipt.version));
    if target != destination.join(&receipt.vsix.name) {
        fs::rename(destination.join(&receipt.vsix.name), &target).map_err(|e| e.to_string())?;
    }
    Ok("CI-validated VSIX reused without rebuilding".into())
}
