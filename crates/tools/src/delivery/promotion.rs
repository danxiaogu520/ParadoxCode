//! Candidate sealing is read-only toward GitHub. Promotion performs all local,
//! source and advisory checks before creating a formal annotated tag.
use super::*;

fn optional_api(path: &str) -> Result<Option<Value>, String> {
    let response = process::command("gh")
        .args(["api", path])
        .output()
        .map_err(|e| e.to_string())?;
    if response.status.success() {
        return serde_json::from_slice(&response.stdout)
            .map(Some)
            .map_err(|e| e.to_string());
    }
    let value = serde_json::from_slice::<Value>(&response.stdout).ok();
    if value
        .as_ref()
        .is_some_and(|v| v["status"] == "404" || v["status"] == 404)
    {
        return Ok(None);
    }
    Err(format!(
        "GitHub lookup failed (not an absence): {}",
        String::from_utf8_lossy(&response.stderr)
    ))
}

fn lookup_release(repository: &str, tag: &str) -> Result<Option<Value>, String> {
    if let Some(release) = optional_api(&format!("repos/{repository}/releases/tags/{tag}"))? {
        return Ok(Some(release));
    }
    // REST's by-tag endpoint finds published releases, not pending draft tags.
    // Resolve the draft ID with GraphQL, then use REST for complete asset digests.
    let (owner, name) = repository.split_once('/').ok_or("invalid repository")?;
    let query = "query($owner:String!,$name:String!,$tag:String!){repository(owner:$owner,name:$name){release(tagName:$tag){databaseId}}}";
    let value: Value = serde_json::from_str(&github::gh(&[
        "api",
        "graphql",
        "-f",
        &format!("query={query}"),
        "-f",
        &format!("owner={owner}"),
        "-f",
        &format!("name={name}"),
        "-f",
        &format!("tag={tag}"),
    ])?)
    .map_err(|e| e.to_string())?;
    require(
        value.get("errors").is_none() && value["data"]["repository"].is_object(),
        "draft release lookup failed",
    )?;
    let release = &value["data"]["repository"]["release"];
    if release.is_null() {
        return Ok(None);
    }
    let id = release["databaseId"]
        .as_u64()
        .filter(|id| *id > 0)
        .ok_or("invalid draft release ID")?;
    github::api(&format!("repos/{repository}/releases/{id}")).map(Some)
}

fn required_release(repository: &str, tag: &str) -> Result<Value, String> {
    lookup_release(repository, tag)?.ok_or_else(|| format!("expected release is absent: {tag}"))
}

fn tag_source(repository: &str, tag: &str) -> Result<Option<String>, String> {
    let Some(reference) = optional_api(&format!("repos/{repository}/git/ref/tags/{tag}"))? else {
        return Ok(None);
    };
    require(
        reference["object"]["type"] == "tag",
        "formal release tag must be annotated",
    )?;
    let object = reference["object"]["sha"]
        .as_str()
        .filter(|s| hex(s, 40))
        .ok_or("invalid tag object")?;
    let tag = github::api(&format!("repos/{repository}/git/tags/{object}"))?;
    require(
        tag["object"]["type"] == "commit",
        "release tag must point directly to a commit",
    )?;
    tag["object"]["sha"]
        .as_str()
        .filter(|s| hex(s, 40))
        .map(|s| Some(s.into()))
        .ok_or_else(|| "invalid tag source".into())
}

pub(super) fn available_version(
    repository: &str,
    version: &str,
    source: &str,
) -> Result<(), String> {
    let tag = format!("v{version}");
    if let Some(release) = lookup_release(repository, &tag)? {
        require(
            release["draft"] == true,
            "version is already publicly released",
        )?;
    }
    if let Some(existing) = tag_source(repository, &tag)? {
        require(
            existing == source,
            "version tag is already reserved for different source; choose a new version",
        )?;
    }
    Ok(())
}

pub(super) fn notes(changelog: &str, version: &str) -> Result<String, String> {
    let heading = format!("## [{version}]");
    let mut sections = changelog.match_indices(&heading);
    let (start, _) = sections
        .next()
        .ok_or("CHANGELOG lacks a dated release section")?;
    require(sections.next().is_none(), "duplicate release section")?;
    let tail = &changelog[start..];
    let line_end = tail.find('\n').ok_or("empty release section")?;
    let suffix = &tail[heading.len()..line_end];
    require(
        suffix.starts_with(" - ") && suffix.len() == 13,
        "release section must have YYYY-MM-DD date",
    )?;
    let body = &tail[line_end + 1..];
    let end = body.find("\n## [").unwrap_or(body.len());
    let body = body[..end].trim();
    require(!body.is_empty(), "release notes are empty")?;
    Ok(format!("{body}\n"))
}

fn previous_release(repository: &str) -> Result<Option<String>, String> {
    let releases = github::api(&format!("repos/{repository}/releases?per_page=100"))?;
    let releases = releases.as_array().ok_or("invalid release list")?;
    Ok(previous_tag(releases))
}

pub(super) fn previous_tag(releases: &[Value]) -> Option<String> {
    // API order follows creation time, which differs from publication time for
    // long-lived drafts. Failed tags never establish a comparison baseline.
    releases
        .iter()
        .filter(|r| r["draft"] == false && r["prerelease"] == false)
        .filter_map(|r| {
            let tag = r["tag_name"].as_str()?;
            stable_version(tag.strip_prefix('v')?).ok()?;
            Some((r["published_at"].as_str()?, tag))
        })
        .max_by_key(|(published, _)| *published)
        .map(|(_, tag)| tag.to_owned())
}

fn asset_inventory(root: &Path, directory: &Path, version: &str) -> Result<Vec<Asset>, String> {
    let server = directory.join("server");
    let (limits, artifacts) = release::load_contract(root).map_err(|e| e.to_string())?;
    release::verify_release_directory(version, &server, &artifacts, &limits)
        .map_err(|e| e.to_string())?;
    let vsix = directory
        .join("extension")
        .join(format!("paradoxcode-vscode-{version}.vsix"));
    verify_vsix(&vsix, version)?;
    let mut paths = report::files(&server)?;
    paths.push(vsix);
    let mut assets = paths
        .iter()
        .map(|p| Asset::from_file(p))
        .collect::<Result<Vec<_>, _>>()?;
    assets.sort_by(|a, b| a.name.cmp(&b.name));
    require(
        assets.len() == 2 * artifacts.len() + 1,
        "candidate inventory is incomplete",
    )?;
    Ok(assets)
}

pub(super) fn seal(root: &Path, args: &Args) -> Result<String, String> {
    let repository = env("GITHUB_REPOSITORY")?;
    require(
        env("GITHUB_EVENT_NAME")? == "workflow_dispatch" && env("GITHUB_REF")? == "refs/heads/main",
        "candidate sealing must use the main workflow",
    )?;
    let needs: Value = serde_json::from_str(&env("NEEDS_JSON")?).map_err(|e| e.to_string())?;
    for name in ["resolve", "audit", "build", "vscode"] {
        require(
            needs[name]["result"] == "success",
            &format!("candidate job did not pass: {name}"),
        )?;
    }
    let sha = git(root, &["rev-parse", "HEAD"])?;
    let ci = github::successful_ci(root, &repository, &sha)?;
    let version = workspace_version(root)?;
    require(
        policy(root)? == policy_at(root, &env("GITHUB_SHA")?)?,
        "sealing controls differ from the dispatched workflow",
    )?;
    let directory = args
        .path("--directory")
        .unwrap_or_else(|| root.join("target/dist"));
    let assets = asset_inventory(root, &directory, &version)?;
    verify_ci_vsix(&ci, &assets, &version)?;
    let staging = report::output(
        root,
        &args
            .path("--output")
            .unwrap_or_else(|| root.join("target/candidate")),
        "",
    )?;
    fs::create_dir_all(&staging).map_err(|e| e.to_string())?;
    let mut body = notes(
        &fs::read_to_string(root.join("CHANGELOG.md")).map_err(|e| e.to_string())?,
        &version,
    )?;
    if let Some(previous) = previous_release(&repository)? {
        body.push_str(&format!("\n**Full Changelog**: https://github.com/{repository}/compare/{previous}...v{version}\n"));
    }
    release::atomic_write(&staging.join("release-notes.md"), body.as_bytes())
        .map_err(|e| e.to_string())?;
    for asset in &assets {
        let source = directory
            .join(if asset.name.ends_with(".vsix") {
                "extension"
            } else {
                "server"
            })
            .join(&asset.name);
        fs::copy(source, staging.join(&asset.name)).map_err(|e| e.to_string())?;
    }
    let candidate = Candidate {
        schema: SCHEMA,
        repository,
        run_id: number("GITHUB_RUN_ID")?,
        run_attempt: number("GITHUB_RUN_ATTEMPT")?,
        workflow_sha: env("GITHUB_SHA")?,
        source_sha: sha.clone(),
        tree: git(root, &["rev-parse", "HEAD^{tree}"])?,
        policy: policy(root)?,
        version,
        ci_run: ci.origin_run,
        ci_attempt: ci.origin_attempt,
        sealed_at: now()?,
        assets,
        notes: Asset::from_file(&staging.join("release-notes.md"))?,
    };
    report::write(
        &staging.join("candidate.json"),
        &serde_json::to_value(&candidate).map_err(|e| e.to_string())?,
    )?;
    verify_candidate(root, &candidate, &staging)?;
    Ok(format!(
        "verified candidate sealed for {sha}; formal tag remains uncreated"
    ))
}

fn expected_assets(root: &Path, version: &str) -> Result<BTreeSet<String>, String> {
    let (_, targets) = release::load_contract(root).map_err(|e| e.to_string())?;
    let mut names = BTreeSet::from([format!("paradoxcode-vscode-{version}.vsix")]);
    for target in targets {
        names.insert(target.archive_name(version));
        names.insert(target.checksum_name(version));
    }
    Ok(names)
}
pub(super) fn verify_candidate(
    root: &Path,
    candidate: &Candidate,
    directory: &Path,
) -> Result<(), String> {
    require(
        candidate.schema == SCHEMA
            && hex(&candidate.source_sha, 40)
            && hex(&candidate.tree, 40)
            && hex(&candidate.policy, 64),
        "invalid candidate source identity",
    )?;
    stable_version(&candidate.version)?;
    require(
        candidate.ci_run > 0
            && candidate.ci_attempt > 0
            && candidate.run_id > 0
            && candidate.run_attempt > 0
            && candidate.sealed_at <= now()?
            && candidate.sealed_at > 0,
        "invalid candidate evidence identity",
    )?;
    let expected = expected_assets(root, &candidate.version)?;
    let actual = candidate
        .assets
        .iter()
        .map(|a| a.name.clone())
        .collect::<BTreeSet<_>>();
    require(
        actual == expected && actual.len() == candidate.assets.len(),
        "candidate assets differ from distribution contract",
    )?;
    require(
        candidate.notes.name == "release-notes.md",
        "unexpected release notes file",
    )?;
    let mut envelope = actual.clone();
    envelope.extend(["candidate.json".into(), "release-notes.md".into()]);
    let files = report::files(directory)?
        .iter()
        .map(|p| {
            p.strip_prefix(directory)
                .map(|p| p.to_string_lossy().into_owned())
                .map_err(|e| e.to_string())
        })
        .collect::<Result<BTreeSet<_>, _>>()?;
    require(
        files == envelope,
        "candidate envelope contains missing or extra files",
    )?;
    for asset in candidate
        .assets
        .iter()
        .chain(std::iter::once(&candidate.notes))
    {
        asset.verify(directory)?;
    }
    verify_vsix(
        &directory.join(format!("paradoxcode-vscode-{}.vsix", candidate.version)),
        &candidate.version,
    )?;
    let (limits, targets) = release::load_contract(root).map_err(|e| e.to_string())?;
    for target in targets {
        release::verify_archive(
            &directory.join(target.archive_name(&candidate.version)),
            &directory.join(target.checksum_name(&candidate.version)),
            &target,
            &limits,
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub(super) fn remote_assets_match(
    release: &Value,
    assets: &[Asset],
    complete: bool,
) -> Result<BTreeSet<String>, String> {
    let expected = assets
        .iter()
        .map(|a| (a.name.as_str(), a))
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut seen = BTreeSet::new();
    for item in release["assets"]
        .as_array()
        .ok_or("invalid uploaded asset list")?
    {
        let name = item["name"].as_str().ok_or("missing uploaded asset name")?;
        let asset = expected
            .get(name)
            .ok_or("release contains an unexpected asset")?;
        require(
            seen.insert(name.to_owned())
                && item["state"] == "uploaded"
                && item["size"] == asset.bytes
                && item["digest"] == format!("sha256:{}", asset.sha256),
            "existing uploaded asset differs from verified candidate; it will not be replaced",
        )?;
    }
    if complete {
        require(
            seen.len() == expected.len(),
            "uploaded release inventory is incomplete",
        )?;
    }
    Ok(seen)
}

pub(super) fn validate_candidate_run(
    candidate: &Candidate,
    run: &Value,
    repository: &str,
) -> Result<(), String> {
    require(
        candidate.repository == repository
            && candidate.run_id == run["id"]
            && candidate.run_attempt == run["run_attempt"]
            && candidate.workflow_sha == run["head_sha"],
        "candidate belongs to a different producer run",
    )?;
    require(
        run["event"] == "workflow_dispatch" && run["head_branch"] == "main",
        "candidate producer must be the reviewed main workflow",
    )
}

pub(super) fn remote_notes_match(release: &Value, tag: &str, notes: &str) -> Result<(), String> {
    require(
        release["tag_name"] == tag
            && release["name"] == tag
            && release["prerelease"] == false
            && release["body"]
                .as_str()
                .is_some_and(|body| body.trim_end() == notes.trim_end()),
        "release metadata differs from the sealed candidate; it will not be replaced",
    )
}

// candidate-vsix normalizes the package filename, but never its bytes.
pub(super) fn verify_ci_vsix(
    ci: &CiReceipt,
    assets: &[Asset],
    version: &str,
) -> Result<(), String> {
    let name = format!("paradoxcode-vscode-{version}.vsix");
    let vsix = assets
        .iter()
        .find(|asset| asset.name == name)
        .ok_or("candidate lacks the CI-validated VSIX")?;
    require(
        vsix.bytes == ci.vsix.bytes && vsix.sha256 == ci.vsix.sha256,
        "candidate VSIX differs from its Full CI receipt",
    )
}

fn verify_candidate_ci(ci: &CiReceipt, candidate: &Candidate) -> Result<(), String> {
    require(
        ci.origin_run == candidate.ci_run && ci.origin_attempt == candidate.ci_attempt,
        "candidate CI origin changed",
    )?;
    verify_ci_vsix(ci, &candidate.assets, &candidate.version)
}

fn refresh_promotion_evidence(
    root: &Path,
    repository: &str,
    candidate: &Candidate,
) -> Result<(), String> {
    let temporary = tempfile::tempdir_in(root.join("target")).map_err(|e| e.to_string())?;
    let source = temporary.path().join("source");
    let source_path = source.to_str().ok_or("non-UTF8 source")?;
    git(
        root,
        &[
            "worktree",
            "add",
            "--detach",
            source_path,
            &candidate.source_sha,
        ],
    )?;
    let checks = (|| {
        let ci = github::successful_ci(&source, repository, &candidate.source_sha)?;
        verify_candidate_ci(&ci, candidate)?;
        let run = github::run(repository, candidate.run_id, CANDIDATE_WORKFLOW)?;
        validate_candidate_run(candidate, &run, repository)
    })();
    let cleanup = git(root, &["worktree", "remove", "--force", source_path]);
    checks?;
    cleanup?;
    Ok(())
}

pub(super) fn promote(root: &Path, args: &Args) -> Result<String, String> {
    require(
        env("GITHUB_EVENT_NAME")? == "workflow_dispatch" && env("GITHUB_REF")? == "refs/heads/main",
        "promotion must execute the trusted main workflow",
    )?;
    let repository = env("GITHUB_REPOSITORY")?;
    let id = args
        .required("--candidate-run")?
        .parse::<u64>()
        .ok()
        .filter(|n| *n > 0)
        .ok_or("invalid candidate run")?;
    let run = github::run(&repository, id, CANDIDATE_WORKFLOW)?;
    let attempt = run["run_attempt"]
        .as_u64()
        .ok_or("missing candidate attempt")?;
    let destination = root.join(format!("target/promotion/{id}-{attempt}"));
    github::artifact(
        &repository,
        &run,
        &format!("release-candidate-{attempt}"),
        &destination,
        512 * 1024 * 1024,
    )?;
    let candidate: Candidate =
        serde_json::from_value(report::json(&destination.join("candidate.json"))?)
            .map_err(|e| e.to_string())?;
    validate_candidate_run(&candidate, &run, &repository)?;
    verify_candidate(root, &candidate, &destination)?;
    require(
        candidate.policy == policy(root)?,
        "release control definitions changed since candidate preparation; prepare a new candidate",
    )?;
    let commit = github::api(&format!(
        "repos/{repository}/git/commits/{}",
        candidate.source_sha
    ))?;
    require(
        commit["tree"]["sha"] == candidate.tree,
        "candidate Git source differs",
    )?;
    git(root, &["fetch", "origin", "main"])?;
    git(
        root,
        &[
            "merge-base",
            "--is-ancestor",
            &candidate.source_sha,
            "origin/main",
        ],
    )?;
    let tag = format!("v{}", candidate.version);
    let release_path = format!("repos/{repository}/releases/tags/{tag}");
    let notes =
        fs::read_to_string(destination.join("release-notes.md")).map_err(|e| e.to_string())?;
    let existing_release = lookup_release(&repository, &tag)?;
    let existing_tag = tag_source(&repository, &tag)?;
    if let Some(existing) = &existing_tag {
        require(
            existing == &candidate.source_sha,
            "existing tag belongs to different source",
        )?;
    }
    if let Some(existing) = &existing_release {
        remote_notes_match(existing, &tag, &notes)?;
        remote_assets_match(existing, &candidate.assets, existing["draft"] == false)?;
        if existing["draft"] == false {
            require(
                existing["immutable"] == true && existing_tag.is_some(),
                "published release provenance/immutability differs",
            )?;
            // Verification-only retries do not depend on an advisory endpoint;
            // the identical immutable version is already publicly available.
            return Ok(format!(
                "{tag} is already published with the identical verified payload"
            ));
        }
    }
    let temporary = tempfile::tempdir_in(root.join("target")).map_err(|e| e.to_string())?;
    let source = temporary.path().join("source");
    git(
        root,
        &[
            "worktree",
            "add",
            "--detach",
            source.to_str().ok_or("non-UTF8 source")?,
            &candidate.source_sha,
        ],
    )?;
    let checks = (|| {
        require(
            workspace_version(&source)? == candidate.version,
            "candidate workspace version differs",
        )?;
        let ci = github::successful_ci(&source, &repository, &candidate.source_sha)?;
        verify_candidate_ci(&ci, &candidate)?;
        // This performs no install or package lifecycle scripts and receives no
        // publication credential. A failed endpoint never becomes a green audit.
        production_audit(&source)
    })();
    let cleanup = git(
        root,
        &[
            "worktree",
            "remove",
            "--force",
            source.to_str().ok_or("non-UTF8 source")?,
        ],
    );
    checks?;
    cleanup?;
    // Uploads are still reversible: the staging name is outside the protected
    // formal v* namespace and is never publicly published.
    let staging_tag = format!("candidate-{id}-{attempt}");
    let draft_tag = if existing_release.is_some() {
        &tag
    } else {
        &staging_tag
    };
    let draft = if existing_release.is_some() {
        existing_release
    } else {
        lookup_release(&repository, draft_tag)?
    };
    if let Some(draft) = &draft {
        require(
            draft["draft"] == true && draft["target_commitish"] == candidate.source_sha,
            "staging draft belongs to different source or is public",
        )?;
        remote_notes_match(draft, draft_tag, &notes)?;
        remote_assets_match(draft, &candidate.assets, false)?;
    } else {
        github::gh(&[
            "release",
            "create",
            draft_tag,
            "--repo",
            &repository,
            "--draft",
            "--target",
            &candidate.source_sha,
            "--title",
            draft_tag,
            "--notes-file",
            destination
                .join("release-notes.md")
                .to_str()
                .ok_or("non-UTF8 notes")?,
        ])?;
    }
    let current = required_release(&repository, draft_tag)?;
    require(
        current["draft"] == true && current["target_commitish"] == candidate.source_sha,
        "release changed while promotion was in progress",
    )?;
    remote_notes_match(&current, draft_tag, &notes)?;
    let present = remote_assets_match(&current, &candidate.assets, false)?;
    for asset in &candidate.assets {
        if !present.contains(&asset.name) {
            github::gh(&[
                "release",
                "upload",
                draft_tag,
                destination
                    .join(&asset.name)
                    .to_str()
                    .ok_or("non-UTF8 asset")?,
                "--repo",
                &repository,
            ])?;
        }
    }
    let uploaded = required_release(&repository, draft_tag)?;
    require(
        uploaded["draft"] == true && uploaded["target_commitish"] == candidate.source_sha,
        "release changed during upload",
    )?;
    remote_notes_match(&uploaded, draft_tag, &notes)?;
    remote_assets_match(&uploaded, &candidate.assets, true)?;
    // Staging can take long enough for CI or candidate reruns to supersede the
    // evidence checked above. Refresh both successful attempts before reserving
    // the formal tag, including when resuming an already reserved draft.
    refresh_promotion_evidence(root, &repository, &candidate)?;
    // Every build, test, audit and remote upload verification has now succeeded.
    // GitHub has no atomic evidence-check-and-tag operation; only formal tag
    // reservation and immutable publication remain after this final refresh.
    if existing_tag.is_none() {
        let local = process::command("git")
            .current_dir(root)
            .args([
                "show-ref",
                "--verify",
                "--quiet",
                &format!("refs/tags/{tag}"),
            ])
            .status()
            .map_err(|e| e.to_string())?;
        if local.success() {
            require(
                git(root, &["cat-file", "-t", &format!("refs/tags/{tag}")])? == "tag"
                    && git(root, &["rev-parse", &format!("refs/tags/{tag}^{{commit}}")])?
                        == candidate.source_sha,
                "local tag differs from the candidate; it will not be replaced",
            )?;
        } else {
            require(local.code() == Some(1), "local tag lookup failed")?;
            git(
                root,
                &[
                    "-c",
                    "user.name=github-actions[bot]",
                    "-c",
                    "user.email=41898282+github-actions[bot]@users.noreply.github.com",
                    "tag",
                    "-a",
                    &tag,
                    &candidate.source_sha,
                    "-m",
                    &format!("ParadoxCode {}", candidate.version),
                ],
            )?;
        }
        git(root, &["push", "origin", &tag])?;
    }
    require(
        tag_source(&repository, &tag)?.as_deref() == Some(&candidate.source_sha),
        "formal tag source was not confirmed",
    )?;
    github::gh(&[
        "release",
        "edit",
        draft_tag,
        "--repo",
        &repository,
        "--tag",
        &tag,
        "--title",
        &tag,
        "--target",
        &candidate.source_sha,
        "--verify-tag",
        "--draft=false",
        "--latest",
    ])?;
    let published = github::api(&release_path)?;
    require(
        published["draft"] == false && published["immutable"] == true,
        "publication completed but immutable public state is not confirmed; inspect before retrying",
    )?;
    remote_notes_match(&published, &tag, &notes)?;
    remote_assets_match(&published, &candidate.assets, true)?;
    Ok(format!(
        "{tag} published from candidate run {id}; all artifact bytes reused unchanged"
    ))
}
