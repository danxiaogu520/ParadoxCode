//! CI process orchestration; workflow files retain runners, matrices and permissions.
use crate::{args::Args, process, report};

use serde_json::Value;

#[cfg(test)]
use serde_json::json;

use std::fs;

use std::path::Path;

const HELP: &str = "tools ci conclusion|cancel-matrix-failed (NEEDS_JSON environment)\ntools ci autosync (GH_TOKEN/GITHUB_REPOSITORY/GITHUB_REPOSITORY_OWNER/PR_NUMBER environment)\ntools ci plan|receipt|candidate-plan|candidate-smoke|candidate-vsix|seal-candidate|promote|production-audit\ntools ci verify-extension-version --version VERSION\ntools ci typos|workflow-lint\ntools fuzz smoke [--runs N] [--seed N] [--target TARGET]";

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

fn unpack_typos(bytes: &[u8], destination: &Path) -> Result<(), String> {
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(bytes));
    for entry in archive.entries().map_err(|error| error.to_string())? {
        let mut entry = entry.map_err(|error| error.to_string())?;
        let path = entry.path().map_err(|error| error.to_string())?;
        let mut components = path
            .components()
            .filter(|component| *component != std::path::Component::CurDir);
        let is_binary = components.next()
            == Some(std::path::Component::Normal(std::ffi::OsStr::new("typos")))
            && components.next().is_none()
            && entry.header().entry_type().is_file();
        if is_binary {
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent).map_err(|error| error.to_string())?;
            }
            entry
                .unpack(destination)
                .map_err(|error| error.to_string())?;
            return Ok(());
        }
    }
    Err("typos archive missing binary".into())
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
            "--ref",
            "--reuse-run",
            "--vsix",
            "--output",
            "--directory",
            "--ci-run",
            "--candidate-run",
            "--binary",
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
        "plan" | "receipt" | "candidate-plan" | "candidate-vsix" | "seal-candidate" | "promote"
        | "production-audit" | "candidate-smoke" => crate::delivery::execute(name, &args),
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

                unpack_typos(&archive.stdout, &binary)?;
            }

            process::run(process::command(binary).current_dir(&root))?;

            Ok("typo check passed".into())
        }

        _ => Err(format!("unknown CI command {name}\n{HELP}")),
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn typos_archive(path: &str, entry_type: tar::EntryType) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_size(4);
        header.set_mode(0o755);
        header.set_entry_type(entry_type);
        // Preserve the archive's leading ./ instead of normalizing the fixture path.
        header.as_mut_bytes()[..path.len()].copy_from_slice(path.as_bytes());
        if entry_type.is_symlink() {
            header.set_link_name("elsewhere").unwrap();
        }
        header.set_cksum();
        builder.append(&header, b"test".as_slice()).unwrap();
        let tar = builder.into_inner().unwrap();
        let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gzip.write_all(&tar).unwrap();
        gzip.finish().unwrap()
    }

    #[test]
    fn typos_installer_extracts_direct_and_dot_prefixed_binary() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("bin/typos");
        for path in ["typos", "./typos"] {
            unpack_typos(&typos_archive(path, tar::EntryType::Regular), &destination).unwrap();
            assert_eq!(fs::read(&destination).unwrap(), b"test");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_ne!(
                    fs::metadata(&destination).unwrap().permissions().mode() & 0o111,
                    0
                );
            }
            fs::remove_file(&destination).unwrap();
        }
    }

    #[test]
    fn typos_installer_rejects_nested_paths_and_symbolic_links() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("typos");
        for (path, kind) in [
            ("bin/typos", tar::EntryType::Regular),
            ("./typos", tar::EntryType::Symlink),
        ] {
            assert!(unpack_typos(&typos_archive(path, kind), &destination).is_err());
            assert!(!destination.exists());
        }
    }

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
