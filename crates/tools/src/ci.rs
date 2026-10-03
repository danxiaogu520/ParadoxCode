//! CI process orchestration; workflow files retain runners, matrices and permissions.
use crate::{args::Args, process, report};

use serde_json::Value;

#[cfg(test)]
use serde_json::json;

use std::collections::BTreeSet;

use std::fs;

use std::io::Write;

use std::path::Path;

const HELP: &str = "tools ci conclusion|cancel-matrix-failed (NEEDS_JSON environment)\ntools ci autosync (GH_TOKEN/GITHUB_REPOSITORY/GITHUB_REPOSITORY_OWNER/PR_NUMBER environment)\ntools ci release-preflight|release-publish --tag vVERSION\ntools ci verify-extension-version --version VERSION\ntools ci typos|workflow-lint\ntools fuzz smoke [--runs N] [--seed N] [--target TARGET]";

fn env(name: &str) -> Result<String, String> {
    std::env::var(name)
        .ok()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("missing environment variable {name}"))
}

fn text(command: &mut std::process::Command) -> Result<String, String> {
    Ok(String::from_utf8_lossy(&process::capture(command)?.stdout)
        .trim()
        .to_owned())
}

fn gh(arguments: &[&str]) -> Result<String, String> {
    text(process::command("gh").args(arguments))
}

fn gh_json(arguments: &[&str]) -> Result<Value, String> {
    serde_json::from_str(&gh(arguments)?).map_err(|e| e.to_string())
}

pub fn conclusions(input: &Value) -> Result<(), String> {
    let jobs = input
        .as_object()
        .filter(|j| !j.is_empty())
        .ok_or("missing job results")?;

    let failed = jobs
        .iter()
        .filter(|(_, j)| !matches!(j["result"].as_str(), Some("success" | "skipped")))
        .map(|(name, j)| format!("{name}: {}", j["result"]))
        .collect::<Vec<_>>();

    if failed.is_empty() {
        Ok(())
    } else {
        Err(format!("failed CI jobs: {}", failed.join(", ")))
    }
}

fn autosync() -> Result<String, String> {
    env("GH_TOKEN")?;

    let repo = env("GITHUB_REPOSITORY")?;

    let owner = env("GITHUB_REPOSITORY_OWNER")?;

    gh(&["api", &format!("repos/{repo}"), "--jq", ".full_name"])?;

    let prs = if let Ok(number) = std::env::var("PR_NUMBER").and_then(|s| {
        if s.is_empty() {
            Err(std::env::VarError::NotPresent)
        } else {
            Ok(s)
        }
    }) {
        vec![number]
    } else {
        gh_json(&[
            "pr", "list", "--repo", &repo, "--state", "open", "--limit", "100", "--json", "number",
        ])?
        .as_array()
        .ok_or("invalid PR listing")?
        .iter()
        .map(|p| p["number"].to_string())
        .collect()
    };

    for number in prs {
        let metadata = match gh_json(&[
            "pr",
            "view",
            &number,
            "--repo",
            &repo,
            "--json",
            "author,isDraft,isCrossRepository,autoMergeRequest",
        ]) {
            Ok(value) => value,
            Err(error) => {
                eprintln!("PR #{number}: {error}");

                continue;
            }
        };

        if metadata["author"]["login"] != owner
            || metadata["isDraft"] == true
            || metadata["isCrossRepository"] == true
        {
            eprintln!("PR #{number}: skipped by owner/draft/fork policy");

            continue;
        }

        if metadata["autoMergeRequest"].is_null()
            && let Err(error) = gh(&[
                "pr", "merge", &number, "--repo", &repo, "--auto", "--squash",
            ])
        {
            eprintln!("PR #{number}: {error}");

            continue;
        }

        match gh(&[
            "api",
            "-X",
            "PUT",
            &format!("repos/{repo}/pulls/{number}/update-branch"),
        ]) {
            Ok(_) => eprintln!("PR #{number}: branch refresh requested"),
            Err(error) => eprintln!("PR #{number}: branch not refreshed: {error}"),
        }
    }

    Ok("PR autosync pass complete".into())
}

fn preflight(root: &Path, tag: &str) -> Result<String, String> {
    let version = tag
        .strip_prefix('v')
        .ok_or("release tag must start with v")?;

    crate::release::validate_release_version(version).map_err(|e| e.to_string())?;

    process::run(
        process::command("git")
            .current_dir(root)
            .args(["fetch", "--force", "origin", "main"])
            .arg(format!("refs/tags/{tag}:refs/tags/{tag}")),
    )?;

    let tag_ref = format!("refs/tags/{tag}");

    if text(
        process::command("git")
            .current_dir(root)
            .args(["cat-file", "-t", &tag_ref]),
    )? != "tag"
    {
        return Err("release tag must be annotated".into());
    }

    let sha = text(
        process::command("git")
            .current_dir(root)
            .args(["rev-list", "-n", "1", &tag_ref]),
    )?;

    process::run(process::command("git").current_dir(root).args([
        "merge-base",
        "--is-ancestor",
        &sha,
        "origin/main",
    ]))?;

    let repo = env("GITHUB_REPOSITORY")?;

    let checks = gh_json(&["api", &format!("repos/{repo}/commits/{sha}/check-runs")])?;

    let mut conclusions = checks["check_runs"]
        .as_array()
        .ok_or("missing check runs")?
        .iter()
        .filter(|c| c["name"] == "Conclusion")
        .collect::<Vec<_>>();

    conclusions.sort_by_key(|c| c["completed_at"].as_str().unwrap_or(""));

    if conclusions
        .last()
        .is_none_or(|c| c["conclusion"] != "success")
    {
        return Err("tagged commit must have a successful latest Conclusion check".into());
    }

    if gh(&["release", "view", tag, "--repo", &repo]).is_ok() {
        return Err("a release already exists; immutable releases are not overwritten".into());
    }

    if let Ok(path) = std::env::var("GITHUB_OUTPUT") {
        writeln!(
            fs::OpenOptions::new()
                .append(true)
                .open(path)
                .map_err(|e| e.to_string())?,
            "release_sha={sha}"
        )
        .map_err(|e| e.to_string())?;
    }

    Ok(sha)
}

fn publish(root: &Path, tag: &str) -> Result<String, String> {
    let version = tag.strip_prefix('v').ok_or("tag must start with v")?;

    crate::release::validate_release_version(version).map_err(|e| e.to_string())?;

    let server = root.join("target/dist/server");

    let (limits, artifacts) = crate::release::load_contract(root).map_err(|e| e.to_string())?;

    crate::release::verify_release_directory(version, &server, &artifacts, &limits)
        .map_err(|e| e.to_string())?;

    let extension = root
        .join("target/dist/extension")
        .join(format!("paradoxcode-vscode-{version}.vsix"));

    if !extension.is_file() || fs::metadata(&extension).map_err(|e| e.to_string())?.len() == 0 {
        return Err("release VSIX is missing or empty".into());
    }

    let repo = env("GITHUB_REPOSITORY")?;

    if gh(&["release", "view", tag, "--repo", &repo]).is_ok() {
        return Err("release already exists; refusing to overwrite".into());
    }

    let staging = root.join("target/dist/release");

    fs::create_dir_all(&staging).map_err(|e| e.to_string())?;

    let mut expected = BTreeSet::new();

    for path in report::files(&server)?
        .into_iter()
        .chain(std::iter::once(extension))
    {
        let name = path.file_name().ok_or("invalid release asset")?;

        if !expected.insert(name.to_string_lossy().into_owned()) {
            return Err("duplicate release asset".into());
        }

        fs::copy(&path, staging.join(name)).map_err(|e| e.to_string())?;
    }

    let stale = report::files(&staging)?
        .into_iter()
        .filter(|p| !expected.contains(p.file_name().unwrap().to_string_lossy().as_ref()))
        .collect::<Vec<_>>();

    if !stale.is_empty() {
        return Err("release staging contains stale assets".into());
    }

    let mut command = process::command("gh");

    command.args([
        "release",
        "create",
        tag,
        "--repo",
        &repo,
        "--draft",
        "--verify-tag",
        "--generate-notes",
        "--title",
        tag,
    ]);

    for name in &expected {
        command.arg(staging.join(name));
    }

    process::run(&mut command)?;

    let actual = gh_json(&["release", "view", tag, "--repo", &repo, "--json", "assets"])?;

    let actual = actual["assets"]
        .as_array()
        .ok_or("missing uploaded asset list")?
        .iter()
        .filter_map(|a| a["name"].as_str())
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();

    if actual != expected {
        return Err("draft release assets differ from the verified local payload; draft remains unpublished".into());
    }

    gh(&[
        "release",
        "edit",
        tag,
        "--repo",
        &repo,
        "--draft=false",
        "--latest",
    ])?;

    Ok(format!(
        "Release {tag} published; Marketplace publication is a separate step"
    ))
}

pub fn execute(group: &str, arguments: &[String]) -> Result<String, String> {
    let Some((name, arguments)) = arguments.split_first() else {
        return Ok(HELP.into());
    };

    let args = Args::parse(
        arguments,
        &[
            "--root",
            "--repo",
            "--tag",
            "--version",
            "--runs",
            "--seed",
            "--target",
        ],
        &[],
    )?;

    if args.help() || ["--help", "help", "-h"].contains(&name.as_str()) {
        return Ok(HELP.into());
    }

    let root = args.root()?;

    if group == "fuzz" {
        if name != "smoke" {
            return Err("fuzz command must be smoke".into());
        }

        let target = args.get("--target").unwrap_or("x86_64-unknown-linux-gnu");

        process::cargo(
            &root,
            &[
                "metadata",
                "--locked",
                "--manifest-path",
                "fuzz/Cargo.toml",
                "--no-deps",
                "--format-version",
                "1",
            ],
        )?;

        process::cargo(
            &root,
            &["+nightly", "fuzz", "build", "--dev", "--target", target],
        )?;

        let targets = text(
            process::command("cargo")
                .current_dir(&root)
                .args(["+nightly", "fuzz", "list"]),
        )?;

        if targets.trim().is_empty() {
            return Err("cargo fuzz list returned no targets".into());
        }

        let runs = args.number("--runs", 200)?.to_string();

        let seed = args.number("--seed", 1)?.to_string();

        for name in targets.split_whitespace() {
            let artifacts = root.join("target/fuzz/artifacts").join(name);

            fs::create_dir_all(&artifacts).map_err(|e| e.to_string())?;

            process::run(
                process::command("cargo")
                    .current_dir(&root)
                    .args([
                        "+nightly", "fuzz", "run", "--dev", "--target", target, name, "--",
                    ])
                    .arg(format!("-artifact_prefix={}/", artifacts.display()))
                    .arg(format!("-runs={runs}"))
                    .arg(format!("-seed={seed}")),
            )?;
        }

        return Ok("all fuzz targets passed bounded invariant smoke".into());
    }

    match name.as_str() {
        "conclusion" => {
            let results: Value =
                serde_json::from_str(&env("NEEDS_JSON")?).map_err(|e| e.to_string())?;

            conclusions(&results)?;

            Ok("all required CI jobs passed or were skipped".into())
        }

        "cancel-matrix-failed" => {
            let results: Value =
                serde_json::from_str(&env("NEEDS_JSON")?).map_err(|e| e.to_string())?;

            if conclusions(&results).is_err() {
                gh(&[
                    "api",
                    "-X",
                    "POST",
                    &format!(
                        "repos/{}/actions/runs/{}/cancel",
                        env("GITHUB_REPOSITORY")?,
                        env("GITHUB_RUN_ID")?
                    ),
                ])?;
            }

            Ok("matrix cancellation policy evaluated".into())
        }

        "autosync" => autosync(),
        "release-preflight" => preflight(
            &root,
            args.get("--tag")
                .map(str::to_owned)
                .or_else(|| std::env::var("RELEASE_TAG").ok())
                .ok_or("missing --tag or RELEASE_TAG")?
                .as_str(),
        ),
        "release-publish" => publish(
            &root,
            args.get("--tag")
                .map(str::to_owned)
                .or_else(|| std::env::var("RELEASE_TAG").ok())
                .ok_or("missing --tag or RELEASE_TAG")?
                .as_str(),
        ),
        "verify-extension-version" => {
            let version = args.required("--version")?;

            for file in ["package.json", "package-lock.json"] {
                if report::json(&root.join("editors/vscode").join(file))?["version"] != version {
                    return Err(format!("extension {file} version differs from {version}"));
                }
            }

            Ok("extension versions match release tag".into())
        }

        "workflow-lint" => {
            let gopath = text(process::command("go").args(["env", "GOPATH"]))?;

            process::run(
                process::command(Path::new(&gopath).join("bin/actionlint")).current_dir(&root),
            )?;

            Ok("workflow lint passed".into())
        }

        "typos" => {
            let binary = root.join("target/ci/bin/typos");

            if !binary.is_file() {
                if !cfg!(target_os = "linux") || std::env::var("CI").is_err() {
                    return Err(
                        "typos CI installer supports Linux CI; run installed typos locally".into(),
                    );
                }

                let version = std::env::var("TYPOS_VERSION").unwrap_or_else(|_| "v1.50.0".into());

                if !version
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '.')
                {
                    return Err("invalid typos version".into());
                }

                let archive=process::capture(process::command("curl").args(["-LsSf",&format!("https://github.com/crate-ci/typos/releases/download/{version}/typos-{version}-x86_64-unknown-linux-musl.tar.gz")]))?;

                let mut archive =
                    tar::Archive::new(flate2::read::GzDecoder::new(archive.stdout.as_slice()));

                let mut found = false;

                for entry in archive.entries().map_err(|e| e.to_string())? {
                    let mut entry = entry.map_err(|e| e.to_string())?;

                    if entry.path().map_err(|e| e.to_string())?.as_ref() == Path::new("typos") {
                        if let Some(parent) = binary.parent() {
                            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                        }

                        entry.unpack(&binary).map_err(|e| e.to_string())?;

                        found = true;

                        break;
                    }
                }

                if !found {
                    return Err("typos archive missing binary".into());
                }
            }

            process::run(process::command(binary).current_dir(&root))?;

            Ok("typo check passed".into())
        }

        _ => Err(format!("unknown CI command {name}\n{HELP}")),
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn failure_cancelled_and_missing_states_are_not_success() {
        assert!(
            conclusions(&json!({
            "a":{
            "result":"success"}
            ,"b":{
            "result":"skipped"}
            }
            ))
            .is_ok()
        );

        for result in ["failure", "cancelled", "", "pending"] {
            assert!(
                conclusions(&json!({
                "a":{
                "result":result}
                }
                ))
                .is_err()
            );
        }

        assert!(conclusions(&json!({})).is_err());
    }
}
