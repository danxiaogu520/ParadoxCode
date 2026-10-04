//! Source-bound CI receipts and verified release candidates. Control commands are
//! available without the analysis feature; artifact data is never executable code.
mod github;
mod promotion;
mod smoke;
#[cfg(test)]
mod tests;

use crate::{args::Args, process, release, report};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeSet,
    fs,
    io::Read,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

const CI_WORKFLOW: &str = ".github/workflows/ci.yml";
const CANDIDATE_WORKFLOW: &str = ".github/workflows/release-candidate.yml";
const SCHEMA: u32 = 1;
const REQUIRED: &[&str] = &[
    "workflow-lint",
    "rustfmt",
    "clippy",
    "rustdoc",
    "rust",
    "msrv",
    "vscode",
    "fuzz",
    "release",
    "dependencies",
    "typo-check",
];

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Asset {
    pub name: String,
    pub bytes: u64,
    pub sha256: String,
}
impl Asset {
    fn from_file(path: &Path) -> Result<Self, String> {
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or("invalid artifact filename")?;
        if !plain_name(name) {
            return Err("artifact must have a plain filename".into());
        }
        let metadata = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
        require(metadata.is_file(), "artifact must be a regular file")?;
        let bytes = metadata.len();
        if bytes == 0 {
            return Err(format!("empty artifact: {name}"));
        }
        Ok(Self {
            name: name.into(),
            bytes,
            sha256: report::file_hash(path)?,
        })
    }
    fn verify(&self, root: &Path) -> Result<(), String> {
        if !plain_name(&self.name) || !hex(&self.sha256, 64) || self.bytes == 0 {
            return Err("invalid artifact identity".into());
        }
        if Self::from_file(&root.join(&self.name))? != *self {
            return Err(format!("artifact digest/size differs: {}", self.name));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CiReceipt {
    pub schema: u32,
    pub repository: String,
    pub run_id: u64,
    pub run_attempt: u64,
    pub event: String,
    pub head_sha: String,
    pub tested_sha: String,
    pub tree: String,
    pub policy: String,
    pub rustc: String,
    pub version: String,
    pub jobs: BTreeSet<String>,
    pub origin_run: u64,
    pub origin_attempt: u64,
    pub vsix: Asset,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Candidate {
    pub schema: u32,
    pub repository: String,
    pub run_id: u64,
    pub run_attempt: u64,
    pub workflow_sha: String,
    pub source_sha: String,
    pub tree: String,
    pub policy: String,
    pub version: String,
    pub ci_run: u64,
    pub ci_attempt: u64,
    pub sealed_at: u64,
    pub assets: Vec<Asset>,
    pub notes: Asset,
}

fn plain_name(s: &str) -> bool {
    !s.is_empty()
        && s != "."
        && s != ".."
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-._".contains(&b))
}
fn hex(s: &str, length: usize) -> bool {
    s.len() == length && s.bytes().all(|c| c.is_ascii_hexdigit())
}
fn require(condition: bool, message: &str) -> Result<(), String> {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}
fn env(key: &str) -> Result<String, String> {
    std::env::var(key)
        .ok()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("missing {key}"))
}
fn number(key: &str) -> Result<u64, String> {
    env(key)?
        .parse()
        .ok()
        .filter(|n| *n > 0)
        .ok_or_else(|| format!("invalid {key}"))
}
fn now() -> Result<u64, String> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .map_err(|e| e.to_string())
}
fn capture(command: &mut std::process::Command) -> Result<String, String> {
    String::from_utf8(process::capture(command)?.stdout)
        .map(|s| s.trim().to_owned())
        .map_err(|e| e.to_string())
}
fn git(root: &Path, args: &[&str]) -> Result<String, String> {
    capture(process::command("git").current_dir(root).args(args))
}
fn output(values: &[(&str, String)]) -> Result<(), String> {
    let path = env("GITHUB_OUTPUT")?;
    let mut text = String::new();
    for (key, value) in values {
        require(
            !value.contains(['\r', '\n']),
            "workflow output must be a single line",
        )?;
        text.push_str(&format!("{key}={value}\n"));
    }
    use std::io::Write;
    fs::OpenOptions::new()
        .append(true)
        .open(path)
        .map_err(|e| e.to_string())?
        .write_all(text.as_bytes())
        .map_err(|e| e.to_string())
}

fn policy(root: &Path) -> Result<String, String> {
    // Hash tracked definitions rather than commit IDs, so squash commits may reuse
    // the same result. The complete source tree is checked separately.
    let paths = git(
        root,
        &[
            "ls-files",
            "-z",
            ".github",
            "crates/tools",
            "Cargo.toml",
            "Cargo.lock",
            "fuzz/Cargo.lock",
            "editors/vscode/package.json",
            "editors/vscode/package-lock.json",
            "editors/vscode/server-distribution.json",
        ],
    )?;
    let mut bytes = Vec::new();
    for path in paths.split('\0').filter(|p| !p.is_empty()) {
        bytes.extend_from_slice(path.as_bytes());
        bytes.push(0);
        bytes.extend(fs::read(root.join(path)).map_err(|e| format!("{path}: {e}"))?);
        bytes.push(0);
    }
    Ok(report::hash(&bytes))
}
fn policy_at(root: &Path, revision: &str) -> Result<String, String> {
    require(hex(revision, 40), "invalid policy commit")?;
    let paths = git(
        root,
        &[
            "ls-tree",
            "-r",
            "--name-only",
            "-z",
            revision,
            ".github",
            "crates/tools",
            "Cargo.toml",
            "Cargo.lock",
            "fuzz/Cargo.lock",
            "editors/vscode/package.json",
            "editors/vscode/package-lock.json",
            "editors/vscode/server-distribution.json",
        ],
    )?;
    let mut bytes = Vec::new();
    for path in paths.split('\0').filter(|p| !p.is_empty()) {
        bytes.extend_from_slice(path.as_bytes());
        bytes.push(0);
        let object = process::capture(
            process::command("git")
                .current_dir(root)
                .args(["show", &format!("{revision}:{path}")]),
        )?;
        bytes.extend(object.stdout);
        bytes.push(0);
    }
    Ok(report::hash(&bytes))
}
fn read_toml(path: &Path) -> Result<toml::Value, String> {
    toml::from_str(&fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?)
        .map_err(|e| format!("{}: {e}", path.display()))
}
fn workspace_version(root: &Path) -> Result<String, String> {
    let cargo = read_toml(&root.join("Cargo.toml"))?;
    let workspace = cargo.get("workspace").ok_or("missing workspace")?;
    let version = workspace
        .get("package")
        .and_then(|p| p.get("version"))
        .and_then(toml::Value::as_str)
        .ok_or("missing workspace version")?
        .to_owned();
    stable_version(&version)?;
    for path in [
        "editors/vscode/package.json",
        "editors/vscode/package-lock.json",
    ] {
        let package = report::json(&root.join(path))?;
        require(
            package["version"] == version,
            "extension/workspace versions differ",
        )?;
        if path.ends_with("package-lock.json") {
            require(
                package["packages"][""]["version"] == version,
                "npm root package version differs",
            )?;
        }
    }
    let names = workspace
        .get("members")
        .and_then(toml::Value::as_array)
        .ok_or("missing workspace members")?
        .iter()
        .map(|p| {
            let path = p.as_str().ok_or("invalid member")?;
            let manifest = read_toml(&root.join(path).join("Cargo.toml"))?;
            manifest
                .get("package")
                .and_then(|p| p.get("name"))
                .and_then(toml::Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| "missing package name".into())
        })
        .collect::<Result<BTreeSet<String>, String>>()?;
    for path in ["Cargo.lock", "fuzz/Cargo.lock"] {
        let lock = read_toml(&root.join(path))?;
        let packages = lock
            .get("package")
            .and_then(toml::Value::as_array)
            .ok_or("missing locked packages")?;
        let mut found = BTreeSet::new();
        for package in packages {
            if let Some(name) = package
                .get("name")
                .and_then(toml::Value::as_str)
                .filter(|n| names.contains(*n) && package.get("source").is_none())
            {
                require(
                    package.get("version").and_then(toml::Value::as_str) == Some(&version),
                    "workspace lockfile versions differ",
                )?;
                found.insert(name.to_owned());
            }
        }
        require(
            !found.is_empty() && (path != "Cargo.lock" || found == names),
            "workspace lockfile is incomplete",
        )?;
        if path == "fuzz/Cargo.lock" {
            let fuzz = read_toml(&root.join("fuzz/Cargo.toml"))?;
            let direct = fuzz
                .get("dependencies")
                .and_then(toml::Value::as_table)
                .ok_or("missing fuzz dependencies")?
                .keys()
                .filter(|n| names.contains(*n))
                .cloned()
                .collect::<BTreeSet<_>>();
            require(
                direct.is_subset(&found),
                "fuzz lockfile lacks workspace dependencies",
            )?;
        }
    }
    Ok(version)
}
fn stable_version(s: &str) -> Result<(), String> {
    let version = semver::Version::parse(s).map_err(|e| e.to_string())?;
    require(
        version.pre.is_empty() && version.build.is_empty() && version.to_string() == s,
        "formal releases require normal stable SemVer",
    )
}

fn verify_vsix(path: &Path, version: &str) -> Result<(), String> {
    require(
        fs::metadata(path).map_err(|e| e.to_string())?.len() <= 64 * 1024 * 1024,
        "VSIX exceeds the size limit",
    )?;
    let mut zip = zip::ZipArchive::new(fs::File::open(path).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let mut file = zip
        .by_name("extension/package.json")
        .map_err(|e| e.to_string())?;
    require(file.size() <= 1024 * 1024, "VSIX manifest is too large")?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    let package: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    require(
        package["version"] == version
            && package["publisher"] == "paradoxcode"
            && package["name"] == "paradoxcode-vscode",
        "VSIX identity/version differs from release",
    )
}

fn strict_jobs(needs: &Value) -> Result<BTreeSet<String>, String> {
    let mut passed = BTreeSet::new();
    for name in REQUIRED {
        require(
            needs[*name]["result"] == "success",
            &format!("required CI job did not pass: {name}"),
        )?;
        passed.insert((*name).into());
    }
    Ok(passed)
}
fn receipt_matches(receipt: &CiReceipt, root: &Path, repository: &str) -> Result<(), String> {
    require(
        receipt.schema == SCHEMA && receipt.repository == repository,
        "CI receipt schema/repository differs",
    )?;
    require(
        hex(&receipt.tested_sha, 40) && hex(&receipt.tree, 40) && hex(&receipt.policy, 64),
        "invalid CI source identity",
    )?;
    require(
        receipt.jobs == REQUIRED.iter().map(|s| (*s).into()).collect(),
        "CI receipt lacks required checks",
    )?;
    require(
        receipt.tree == git(root, &["rev-parse", "HEAD^{tree}"])?
            && receipt.policy == policy(root)?
            && receipt.rustc == capture(process::command("rustc").arg("--version"))?,
        "CI source tree, policy or toolchain differs",
    )?;
    require(
        receipt.version == workspace_version(root)?,
        "CI version differs",
    )?;
    require(
        receipt.origin_run > 0 && receipt.origin_attempt > 0,
        "invalid CI origin",
    )
}

pub fn execute(command: &str, args: &Args) -> Result<String, String> {
    let root = args.root()?;
    match command {
        "plan" => github::ci_plan(&root),
        "receipt" => github::ci_receipt(&root, args),
        "candidate-plan" => github::candidate_plan(&root, args),
        "candidate-vsix" => github::candidate_vsix(&root, args),
        "seal-candidate" => promotion::seal(&root, args),
        "promote" => promotion::promote(&root, args),
        "candidate-smoke" => smoke::run(&root, args),
        "production-audit" => {
            production_audit(&root)?;
            Ok("production dependency audit passed".into())
        }
        _ => Err(format!("unknown delivery command: {command}")),
    }
}

fn production_audit(root: &Path) -> Result<(), String> {
    // Audit the lockfile without installing packages or executing lifecycle scripts.
    // Network failures remain failures; npm performs a bounded request retry.
    process::run(
        process::command("npm")
            .current_dir(root)
            .args([
                "--prefix",
                "editors/vscode",
                "audit",
                "--package-lock-only",
                "--ignore-scripts",
                "--omit=dev",
                "--audit-level=high",
                "--fetch-timeout=30000",
                "--fetch-retries=2",
                "--fetch-retry-mintimeout=1000",
                "--fetch-retry-maxtimeout=5000",
            ])
            .env_remove("GH_TOKEN")
            .env_remove("GITHUB_TOKEN"),
    )
}
