//! Quality-gate checks for the ParadoxCode repository.
//!
//! These checks replace the former Python scripts (`check-project-policy.py`,
//! `check-phase6a.py`, `check-release-version.py`). Run via `cargo test` or
//! `cargo run -p tools -- check (policy|release|all)`.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

const REPOSITORY: &str = "https://github.com/danxiaogu520/ParadoxCode";

/// A simple check that either passes or produces a message.
#[derive(Clone, Debug)]
pub struct CheckResult {
    pub name: String,
    pub outcome: CheckOutcome,
}

#[derive(Clone, Debug)]
pub enum CheckOutcome {
    Passed,
    Failed(String),
}

impl CheckResult {
    fn pass(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            outcome: CheckOutcome::Passed,
        }
    }

    fn fail(name: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            outcome: CheckOutcome::Failed(message.into()),
        }
    }
}

fn check(condition: bool, name: &str, message: impl Into<String>) -> CheckResult {
    if condition {
        CheckResult::pass(name)
    } else {
        CheckResult::fail(name, message)
    }
}

/// Runs all policy checks against the repository root.
pub fn check_project_policy(root: &Path) -> Vec<CheckResult> {
    let mut results = Vec::new();
    let requires_file = |path: &str| -> CheckResult {
        check(
            root.join(path).is_file(),
            &format!("file exists: {path}"),
            format!("missing required file: {path}"),
        )
    };
    let requires_dir = |path: &str| -> CheckResult {
        check(
            root.join(path).is_dir(),
            &format!("dir exists: {path}"),
            format!("missing required directory: {path}"),
        )
    };

    results.push(requires_file("Cargo.toml"));
    results.push(requires_file("Cargo.lock"));
    results.push(requires_file("README.md"));
    results.push(requires_file("GOVERNANCE.md"));
    results.push(requires_file("RELEASING.md"));
    results.push(requires_file("SECURITY.md"));
    results.push(requires_file("LICENSE"));
    results.push(requires_file(".github/actionlint.yaml"));
    results.push(requires_file(".github/workflows/ci.yml"));
    results.push(requires_file(".github/workflows/release.yml"));
    results.push(requires_file(".github/workflows/sweep.yml"));
    results.push(requires_file("docs/runner-recovery.md"));
    results.push(requires_file("deny.toml"));
    results.push(requires_file("editors/vscode/package.json"));
    results.push(requires_file("editors/vscode/package-lock.json"));
    results.push(requires_dir("fuzz"));

    let unpinned_actions = unpinned_workflow_actions(root);
    results.push(check(
        unpinned_actions.is_empty(),
        "workflow actions pinned",
        format!(
            "workflow actions must use full commit SHAs: {}",
            unpinned_actions.join(", ")
        ),
    ));

    if let Ok(release_workflow) = fs::read_to_string(root.join(".github/workflows/release.yml")) {
        results.push(check(
            release_workflow.contains("--draft")
                && release_workflow.contains("uses: ./.github/workflows/sweep.yml")
                && !release_workflow.contains("--clobber"),
            "immutable release workflow",
            "release workflow must gate through sweep, publish from a draft, and never clobber assets",
        ));
    }

    if let Ok(sweep_workflow) = fs::read_to_string(root.join(".github/workflows/sweep.yml")) {
        results.push(check(
            sweep_workflow.contains("github.workflow_ref")
                && sweep_workflow.contains("github.ref == 'refs/heads/main'")
                && sweep_workflow.contains("refs/tags/{0}")
                && sweep_workflow.contains("IsPathFullyQualified"),
            "trusted sweep authorization",
            "sweep must reject untrusted callers and refs and require absolute runner paths",
        ));
    }

    // README content checks.
    if let Ok(readme) = fs::read_to_string(root.join("README.md")) {
        results.push(check(
            readme.contains("not affiliated with or endorsed by Paradox Interactive"),
            "README disclaimer",
            "README disclaimer is missing",
        ));
        results.push(check(
            readme.contains("Latest release:"),
            "README release status",
            "README release status is missing",
        ));
    }

    // Retired crate checks.
    results.push(check(
        !root.join("crates/pdx-cwt").exists(),
        "retired pdx-cwt absent",
        "the retired CWT importer must not return",
    ));
    results.push(check(
        !root.join("crates/pdx-eu4").exists(),
        "retired pdx-eu4 absent",
        "the retired pdx-eu4 compatibility facade must not return",
    ));

    // CWT prohibition in rules.
    let rule_source = root.join("rules/eu4");
    if rule_source.is_dir() {
        let cwt_found = contains_extension_recursive(&rule_source, "cwt");
        results.push(check(
            !cwt_found,
            "no CWT in rules/eu4",
            "CWT files are prohibited in the authoritative rule source",
        ));
    } else {
        results.push(CheckResult::fail(
            "rules/eu4 directory",
            "first-party EU4 rule source is missing",
        ));
    }

    // Cargo metadata checks.
    match cargo_metadata(root) {
        Ok(packages) => {
            results.push(check(
                !packages.is_empty(),
                "cargo metadata",
                "Cargo workspace has no packages",
            ));
            let versions: BTreeSet<&str> = packages.iter().map(|p| p.version.as_str()).collect();
            results.push(check(
                versions.len() == 1,
                "workspace version agreement",
                format!("workspace package versions must agree: {versions:?}"),
            ));
            for package in &packages {
                let name_ok = !package.name.contains('-');
                results.push(check(
                    name_ok,
                    &format!("package name: {}", package.name),
                    format!(
                        "workspace package names are short and unprefixed (rust-analyzer style): {}",
                        package.name
                    ),
                ));
                let repo_ok = package.repository.as_deref() == Some(REPOSITORY);
                results.push(check(
                    repo_ok,
                    &format!("{}.repository", package.name),
                    format!(
                        "{name}: repository metadata is missing",
                        name = package.name
                    ),
                ));
                let license_ok = package.license.as_deref() == Some("MIT");
                results.push(check(
                    license_ok,
                    &format!("{}.license", package.name),
                    format!("{name}: expected MIT license metadata", name = package.name),
                ));
                let internal = [
                    "pdc",
                    "tools",
                    "vfs",
                    "index",
                    "hir",
                    "rules",
                    "game",
                    "parser",
                    "text",
                    "engine",
                    "ide",
                    "transcode",
                ];
                if internal.contains(&package.name.as_str()) {
                    let publish_disabled = package
                        .publish
                        .as_ref()
                        .is_some_and(|registries| registries.is_empty());
                    results.push(check(
                        publish_disabled,
                        &format!("{}.publish", package.name),
                        format!(
                            "{name}: internal workspace crates must not be publishable",
                            name = package.name
                        ),
                    ));
                }
            }
        }
        Err(error) => {
            results.push(CheckResult::fail("cargo metadata", error));
        }
    }

    // Version agreement across workspace and editor extension.
    if let Ok(workspace_version) = read_workspace_version(root) {
        if let Ok(text) = fs::read_to_string(root.join("editors/vscode/package.json"))
            && let Ok(package) = serde_json::from_str::<serde_json::Value>(&text)
        {
            let version = package.get("version").and_then(|value| value.as_str());
            results.push(check(
                version == Some(workspace_version.as_str()),
                "VS Code extension version",
                format!(
                    "VS Code extension version {} != workspace {workspace_version}",
                    version.unwrap_or("<missing>")
                ),
            ));
            results.push(check(
                package.get("publisher").and_then(|value| value.as_str()) == Some("paradoxcode"),
                "VS Code publisher id",
                "published VS Code publisher id must remain paradoxcode",
            ));
            results.push(check(
                package.get("name").and_then(|value| value.as_str()) == Some("paradoxcode-vscode"),
                "VS Code extension id",
                "published VS Code extension name must remain paradoxcode-vscode",
            ));
        }
        if let Ok(text) = fs::read_to_string(root.join("editors/vscode/package-lock.json"))
            && let Ok(package_lock) = serde_json::from_str::<serde_json::Value>(&text)
        {
            let version = package_lock.get("version").and_then(|value| value.as_str());
            results.push(check(
                version == Some(workspace_version.as_str()),
                "VS Code lockfile version",
                format!(
                    "VS Code lockfile version {} != workspace {workspace_version}",
                    version.unwrap_or("<missing>")
                ),
            ));
        }
    }

    // Server distribution contract.
    if root
        .join("editors/vscode/server-distribution.json")
        .is_file()
    {
        match crate::release::load_contract(root) {
            Ok((limits, artifacts)) => {
                results.push(check(
                    limits.checksum_bytes == 1024
                        && limits.archive_bytes == 64 * 1024 * 1024
                        && limits.executable_bytes == 128 * 1024 * 1024,
                    "server distribution limits",
                    "server distribution safety limits changed unexpectedly",
                ));
                let expected_count = 5;
                results.push(check(
                    artifacts.len() == expected_count,
                    "server distribution targets",
                    format!(
                        "server distribution target matrix is incomplete: {} vs {expected_count}",
                        artifacts.len()
                    ),
                ));
                let expected_binaries: BTreeSet<(&str, &str)> = [
                    ("tar.gz", "paradoxcode"),
                    ("tar.gz", "paradoxcode"),
                    ("tar.gz", "paradoxcode"),
                    ("tar.gz", "paradoxcode"),
                    ("zip", "paradoxcode.exe"),
                ]
                .into_iter()
                .collect();
                let actual_binaries: BTreeSet<(&str, &str)> = artifacts
                    .iter()
                    .map(|a| {
                        let kind = if a.archive_template.ends_with(".tar.gz") {
                            "tar.gz"
                        } else {
                            "zip"
                        };
                        (kind, a.binary.as_str())
                    })
                    .collect();
                results.push(check(
                    actual_binaries == expected_binaries,
                    "server distribution binaries",
                    "server distribution binary contract mismatch",
                ));
            }
            Err(error) => {
                results.push(CheckResult::fail(
                    "server distribution contract",
                    error.to_string(),
                ));
            }
        }
    }

    // Rule source metadata.
    let rules_manifest = root.join("rules/eu4/manifest.json");
    if rules_manifest.is_file()
        && let Ok(text) = fs::read_to_string(&rules_manifest)
        && let Ok(manifest) = serde_json::from_str::<serde_json::Value>(&text)
    {
        let game_id = manifest
            .get("game_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        results.push(check(
            game_id == "eu4",
            "rules game_id",
            format!("rules manifest game_id is not eu4: {game_id}"),
        ));
    }

    results
}

fn contains_extension_recursive(root: &Path, extension: &str) -> bool {
    let Ok(entries) = fs::read_dir(root) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let path = entry.path();
        if path.is_dir() {
            contains_extension_recursive(&path, extension)
        } else {
            path.extension()
                .and_then(|value| value.to_str())
                .is_some_and(|value| value.eq_ignore_ascii_case(extension))
        }
    })
}

fn unpinned_workflow_actions(root: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(root.join(".github/workflows")) else {
        return vec![".github/workflows is unreadable".to_owned()];
    };
    let mut findings = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let is_yaml = path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| matches!(extension, "yml" | "yaml"));
        if !path.is_file() || !is_yaml {
            continue;
        }
        let Ok(contents) = fs::read_to_string(&path) else {
            findings.push(path.display().to_string());
            continue;
        };
        for (line_index, line) in contents.lines().enumerate() {
            let trimmed = line.trim_start();
            let trimmed = trimmed.strip_prefix("- ").unwrap_or(trimmed);
            let Some(value) = trimmed.strip_prefix("uses:") else {
                continue;
            };
            let action = value.split_whitespace().next().unwrap_or_default();
            if action.starts_with("./") {
                continue;
            }
            let pinned = action.rsplit_once('@').is_some_and(|(_, reference)| {
                reference.len() == 40 && reference.bytes().all(|byte| byte.is_ascii_hexdigit())
            });
            if !pinned {
                findings.push(format!("{}:{} ({action})", path.display(), line_index + 1));
            }
        }
    }
    findings
}

/// Reads `{open, close}` object pairs from the canonical profile JSON.
fn syntax_bracket_pairs(value: Option<&serde_json::Value>) -> Option<Vec<(String, String)>> {
    object_bracket_pairs(value)
}

/// Reads bracket pairs that VSCode stores either as `[open, close]` arrays (its `brackets`
/// contribution) or as `{open, close}` objects (its `autoClosingPairs` contribution).
fn vscode_bracket_pairs(value: Option<&serde_json::Value>) -> Option<Vec<(String, String)>> {
    let array = value?.as_array()?;
    let mut pairs = Vec::with_capacity(array.len());
    for item in array {
        let pair = if let Some([open, close]) = item.as_array().map(Vec::as_slice) {
            (open.as_str()?.to_owned(), close.as_str()?.to_owned())
        } else {
            (
                item.get("open")?.as_str()?.to_owned(),
                item.get("close")?.as_str()?.to_owned(),
            )
        };
        pairs.push(pair);
    }
    Some(pairs)
}

fn object_bracket_pairs(value: Option<&serde_json::Value>) -> Option<Vec<(String, String)>> {
    let array = value?.as_array()?;
    let mut pairs = Vec::with_capacity(array.len());
    for item in array {
        pairs.push((
            item.get("open")?.as_str()?.to_owned(),
            item.get("close")?.as_str()?.to_owned(),
        ));
    }
    Some(pairs)
}

/// Validates that the VS Code `language-configuration.json` still matches the canonical
/// `editors/syntax-profile.json`. Bracket pairs, auto-closing pairs, the line comment marker,
/// and indentation rules have no LSP protocol to share them, so drift here silently breaks
/// parity with the engine's syntax profile.
pub fn check_editor_syntax_parity(root: &Path) -> Vec<CheckResult> {
    let mut results = Vec::new();
    let profile_path = root.join("editors/syntax-profile.json");
    let vscode_config_path = root.join("editors/vscode/language-configuration.json");

    let Ok(profile) = fs::read_to_string(&profile_path) else {
        results.push(CheckResult::fail(
            "editor syntax profile",
            "editors/syntax-profile.json missing",
        ));
        return results;
    };
    let Ok(profile) = serde_json::from_str::<serde_json::Value>(&profile) else {
        results.push(CheckResult::fail(
            "editor syntax profile",
            "editors/syntax-profile.json is not valid JSON",
        ));
        return results;
    };
    let expect = |name: &'static str, value: Option<&serde_json::Value>| -> CheckResult {
        check(
            value.is_some(),
            name,
            format!("editors/syntax-profile.json is missing {name}"),
        )
    };
    let line_comment = profile.get("lineComment");
    let brackets = profile.get("brackets");
    let auto_closing_pairs = profile.get("autoClosingPairs");
    let indentation = profile.get("indentationRules");
    results.push(expect("lineComment", line_comment));
    results.push(expect("brackets", brackets));
    results.push(expect("autoClosingPairs", auto_closing_pairs));
    results.push(expect("indentationRules", indentation));

    // VSCode language-configuration.json must mirror the profile exactly.
    if let Ok(source) = fs::read_to_string(&vscode_config_path)
        && let Ok(vscode) = serde_json::from_str::<serde_json::Value>(&source)
    {
        results.push(check(
            vscode.pointer("/comments/lineComment") == line_comment,
            "vscode: line comment parity",
            "VSCode language-configuration line comment differs from the syntax profile",
        ));
        // Both files name the same pairs in different shapes: the profile and VSCode's
        // autoClosingPairs use `{open,close}` objects, VSCode's brackets use `[open,close]`.
        results.push(check(
            vscode_bracket_pairs(vscode.get("brackets")) == syntax_bracket_pairs(brackets),
            "vscode: bracket parity",
            "VSCode bracket pairs differ from the syntax profile",
        ));
        results.push(check(
            vscode_bracket_pairs(vscode.get("autoClosingPairs"))
                == syntax_bracket_pairs(auto_closing_pairs),
            "vscode: auto-closing parity",
            "VSCode autoClosingPairs differ from the syntax profile",
        ));
        results.push(check(
            vscode.get("indentationRules") == indentation,
            "vscode: indentation parity",
            "VSCode indentation rules differ from the syntax profile",
        ));
    } else {
        results.push(CheckResult::fail(
            "vscode: language configuration",
            "editors/vscode/language-configuration.json missing or invalid",
        ));
    }

    results
}

/// Validates first-party source compilation and the generated rule manifest.
pub fn check_release_artifact(root: &Path) -> Vec<CheckResult> {
    let mut results = Vec::new();
    let source_path = root.join("rules/eu4");
    let manifest_path = root.join("rules/manifest.json");

    results.push(check(
        source_path.is_dir(),
        "rules source",
        "rules/eu4 source directory is missing",
    ));
    results.push(check(
        !root.join("rules/eu4.pdcrules").exists(),
        "no committed rules artifact",
        "rules/eu4.pdcrules must be generated in a user or release cache, not committed",
    ));
    if !source_path.is_dir() || !manifest_path.is_file() {
        results.push(CheckResult::fail(
            "rules manifest",
            "rules/manifest.json or rules/eu4 is missing",
        ));
        return results;
    }

    let Ok(manifest_text) = fs::read_to_string(&manifest_path) else {
        results.push(CheckResult::fail(
            "rules manifest",
            "cannot read rules/manifest.json",
        ));
        return results;
    };
    let Ok(expected_manifest) =
        serde_json::from_str::<rules::rulec::ArtifactManifest>(&manifest_text)
    else {
        results.push(CheckResult::fail(
            "rules manifest",
            "invalid rules/manifest.json",
        ));
        return results;
    };

    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let temporary_directory = std::env::temp_dir().join(format!(
        "paradoxcode-release-rules-{}-{nonce}",
        std::process::id()
    ));
    if let Err(error) = fs::create_dir_all(&temporary_directory) {
        results.push(CheckResult::fail(
            "rules source compilation",
            format!("cannot create temporary validation directory: {error}"),
        ));
        return results;
    }
    let generated_path = temporary_directory.join("eu4.pdcrules");
    let generated_manifest_path = temporary_directory.join("manifest.json");
    match rules::rulec::compile(&source_path, &generated_path, &generated_manifest_path) {
        Ok(generated_manifest) => {
            results.push(CheckResult::pass("rules source compilation"));
            results.push(check(
                generated_manifest == expected_manifest,
                "rules manifest reproducibility",
                format!(
                    "generated rule manifest differs from rules/manifest.json: generated hash {}",
                    generated_manifest.rule_hash
                ),
            ));
            let actual_sha: String = fs::read(&generated_path)
                .map(|bytes| {
                    Sha256::digest(&bytes)
                        .iter()
                        .map(|byte| format!("{byte:02x}"))
                        .collect()
                })
                .unwrap_or_default();
            results.push(check(
                actual_sha == generated_manifest.artifact_sha256,
                "rules artifact checksum",
                format!("generated rules artifact checksum mismatch: {actual_sha}"),
            ));
            match rules::RuleSet::load(&generated_path) {
                Ok(rules) => {
                    results.push(check(
                        rules.schema_version() == generated_manifest.schema_version,
                        "rules schema version",
                        format!(
                            "schema version mismatch: {} vs {}",
                            rules.schema_version(),
                            generated_manifest.schema_version
                        ),
                    ));
                    results.push(check(
                        rules.rule_hash().to_hex() == generated_manifest.rule_hash,
                        "rules rule_hash",
                        format!(
                            "rule_hash mismatch: {} vs {}",
                            rules.rule_hash().to_hex(),
                            generated_manifest.rule_hash
                        ),
                    ));
                    results.push(check(
                        rules.game_id() == generated_manifest.game_id && rules.game_id() == "eu4",
                        "rules game_id",
                        format!("game/profile mismatch: {} vs eu4", rules.game_id()),
                    ));
                    match game::eu4::first_party_rules() {
                        Ok(embedded) => results.push(check(
                            embedded == rules,
                            "embedded rules match source",
                            "embedded first-party JSON bundle differs from the compiled source",
                        )),
                        Err(error) => results.push(CheckResult::fail(
                            "embedded rules validation",
                            error.to_string(),
                        )),
                    }
                    results.push(CheckResult::pass("rules foreign keys enabled"));
                }
                Err(error) => {
                    results.push(CheckResult::fail("rules validation", error.to_string()));
                }
            }
        }
        Err(error) => {
            results.push(CheckResult::fail(
                "rules source compilation",
                error.to_string(),
            ));
        }
    }
    let _ = fs::remove_dir_all(&temporary_directory);

    results
}

/// Prints results and returns true if all passed.
pub fn report(results: &[CheckResult]) -> bool {
    let mut passed = 0usize;
    let mut failed = 0usize;
    for result in results {
        match &result.outcome {
            CheckOutcome::Passed => {
                println!("  PASS  {}", result.name);
                passed += 1;
            }
            CheckOutcome::Failed(message) => {
                eprintln!("  FAIL  {}: {message}", result.name);
                failed += 1;
            }
        }
    }
    println!("{passed} passed, {failed} failed");
    failed == 0
}

#[derive(Clone, Debug)]
struct PackageInfo {
    name: String,
    version: String,
    repository: Option<String>,
    license: Option<String>,
    publish: Option<Vec<String>>,
}

fn cargo_metadata(root: &Path) -> Result<Vec<PackageInfo>, String> {
    let output = Command::new("cargo")
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .current_dir(root)
        .output()
        .map_err(|error| format!("cannot run cargo metadata: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "cargo metadata failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let doc: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("invalid cargo metadata: {error}"))?;
    let packages = doc
        .get("packages")
        .and_then(|v| v.as_array())
        .ok_or("cargo metadata has no packages")?;
    let mut result = Vec::new();
    for package in packages {
        result.push(PackageInfo {
            name: package
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_owned(),
            version: package
                .get("version")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_owned(),
            repository: package
                .get("repository")
                .and_then(|v| v.as_str())
                .map(str::to_owned),
            license: package
                .get("license")
                .and_then(|v| v.as_str())
                .map(str::to_owned),
            publish: package.get("publish").and_then(|v| {
                v.as_array().map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(str::to_owned))
                        .collect()
                })
            }),
        });
    }
    Ok(result)
}

fn read_workspace_version(root: &Path) -> Result<String, String> {
    read_toml_value(
        root.join("Cargo.toml"),
        &["workspace", "package", "version"],
    )
}

fn read_toml_value<T: for<'de> serde::Deserialize<'de>>(
    path: PathBuf,
    keys: &[&str],
) -> Result<T, String> {
    let text = fs::read_to_string(&path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    let mut value: toml::Value = toml::from_str(&text)
        .map_err(|error| format!("invalid TOML in {}: {error}", path.display()))?;
    for key in keys {
        value = value
            .get(key)
            .cloned()
            .ok_or_else(|| format!("missing key '{key}' in {}", path.display()))?;
    }
    T::deserialize(value).map_err(|error| {
        format!(
            "type mismatch for '{}' in {}: {error}",
            keys.last().unwrap_or(&""),
            path.display()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_policy_passes_on_this_repository() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let results = check_project_policy(&root);
        let all_pass = results
            .iter()
            .all(|r| matches!(r.outcome, CheckOutcome::Passed));
        for result in &results {
            if let CheckOutcome::Failed(ref msg) = result.outcome {
                eprintln!("FAIL {}: {msg}", result.name);
            }
        }
        assert!(all_pass, "project policy checks must pass");
    }

    #[test]
    fn release_artifact_passes_on_this_repository() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let results = check_release_artifact(&root);
        let all_pass = results
            .iter()
            .all(|r| matches!(r.outcome, CheckOutcome::Passed));
        for result in &results {
            if let CheckOutcome::Failed(ref msg) = result.outcome {
                eprintln!("FAIL {}: {msg}", result.name);
            }
        }
        assert!(all_pass, "release artifact checks must pass");
    }

    #[test]
    fn editor_syntax_parity_passes_on_this_repository() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let results = check_editor_syntax_parity(&root);
        let all_pass = results
            .iter()
            .all(|r| matches!(r.outcome, CheckOutcome::Passed));
        for result in &results {
            if let CheckOutcome::Failed(ref msg) = result.outcome {
                eprintln!("FAIL {}: {msg}", result.name);
            }
        }
        assert!(all_pass, "editor syntax parity checks must pass");
    }
}
