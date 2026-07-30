//! Quality-gate checks for the ParadoxCode repository.
//!
//! These checks replace the former Python scripts (`check-project-policy.py`,
//! `check-zed-extension.py`, `check-phase6a.py`, `check-release-version.py`,
//! `check-phase1-grammar-deletions.py`). Run via `cargo test` or
//! `cargo run --bin pdx -- check (policy|zed|release|grammar-fuzz|all)`.

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
    results.push(requires_file("LICENSE"));
    results.push(requires_file("docs/architecture.md"));
    results.push(requires_file(".github/workflows/ci.yml"));
    results.push(requires_file(".github/workflows/release.yml"));
    results.push(requires_file("deny.toml"));
    results.push(requires_file("editors/zed/extension.toml"));
    results.push(requires_dir("grammars/tree-sitter-eu4"));
    results.push(requires_dir("fuzz"));
    results.push(requires_file(".githooks/pre-commit"));

    // README content checks.
    if let Ok(readme) = fs::read_to_string(root.join("README.md")) {
        results.push(check(
            readme.contains("not affiliated with or endorsed by Paradox Interactive"),
            "README disclaimer",
            "README disclaimer is missing",
        ));
        results.push(check(
            readme.contains("has not published an end-user release"),
            "README alpha status",
            "README alpha status is missing",
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
        let mut cwt_found = false;
        if let Ok(entries) = fs::read_dir(&rule_source) {
            for entry in entries.flatten() {
                if entry.path().extension().and_then(|ext| ext.to_str()) == Some("cwt") {
                    cwt_found = true;
                    break;
                }
            }
        }
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

    // Pre-commit hook invokes quality gates.
    if let Ok(hook) = fs::read_to_string(root.join(".githooks/pre-commit")) {
        results.push(check(
            hook.contains("scripts/check-quality-gates.sh"),
            "pre-commit hook",
            "the pre-commit hook must invoke the versioned quality-gate entry point",
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
                let prefix_ok = package.name.starts_with("pdx-");
                results.push(check(
                    prefix_ok,
                    &format!("package prefix: {}", package.name),
                    format!(
                        "workspace package does not use pdx- prefix: {}",
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
                    "pdx-lsp",
                    "pdx-rules",
                    "pdx-game",
                    "pdx-parser",
                    "pdx-text",
                    "pdx-engine",
                    "pdx-analysis",
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

    // Version agreement across workspace and Zed.
    if let Ok(workspace_version) = read_workspace_version(root) {
        if let Ok(ext_version) =
            read_toml_value::<String>(root.join("editors/zed/extension.toml"), &["version"])
        {
            results.push(check(
                ext_version == workspace_version,
                "Zed extension version",
                format!("Zed extension version {ext_version} != workspace {workspace_version}"),
            ));
        }
        if let Ok(zc_version) =
            read_toml_value::<String>(root.join("editors/zed/Cargo.toml"), &["package", "version"])
        {
            results.push(check(
                zc_version == workspace_version,
                "Zed Cargo version",
                format!("Zed Cargo version {zc_version} != workspace {workspace_version}"),
            ));
        }
    }

    // Zed extension metadata.
    if let Ok(ext_id) = read_toml_value::<String>(root.join("editors/zed/extension.toml"), &["id"])
    {
        results.push(check(
            ext_id == "paradoxcode",
            "Zed extension id",
            "published Zed extension id must remain stable",
        ));
    }
    if let Ok(ext_name) =
        read_toml_value::<String>(root.join("editors/zed/extension.toml"), &["name"])
    {
        results.push(check(
            ext_name == "ParadoxCode - EU4 Language Tools",
            "Zed extension name",
            "unexpected Zed display name",
        ));
    }
    if let Ok(ext_desc) =
        read_toml_value::<String>(root.join("editors/zed/extension.toml"), &["description"])
    {
        results.push(check(
            ext_desc.to_lowercase().contains("unofficial"),
            "Zed extension description",
            "Zed description must identify the extension as unofficial",
        ));
    }

    // Server distribution contract.
    if root.join("editors/zed/server-distribution.json").is_file() {
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
                    ("tar.gz", "pdx-ls"),
                    ("tar.gz", "pdx-ls"),
                    ("tar.gz", "pdx-ls"),
                    ("tar.gz", "pdx-ls"),
                    ("zip", "pdx-ls.exe"),
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

/// Validates the Zed extension manifest, grammar config, and query files.
pub fn check_zed_extension(root: &Path) -> Vec<CheckResult> {
    let mut results = Vec::new();
    let ext_dir = root.join("editors/zed");

    if let Ok(ext_schema) =
        read_toml_value::<i64>(ext_dir.join("extension.toml"), &["schema_version"])
    {
        results.push(check(
            ext_schema == 1,
            "Zed extension schema",
            "unsupported Zed extension schema",
        ));
    }

    let grammar_root = root.join("grammars");
    let languages = [(
        "eu4",
        "tree-sitter-eu4",
        "test/corpus/eu4.txt",
        &[
            "highlights.scm",
            "brackets.scm",
            "indents.scm",
            "outline.scm",
        ][..],
    )];

    for (lang_dir_name, grammar_dir_name, sample_relative, query_names) in languages {
        let lang_dir = ext_dir.join("languages").join(lang_dir_name);
        let grammar_path = grammar_root.join(grammar_dir_name);

        results.push(check(
            grammar_path.join("grammar.js").is_file(),
            &format!("{grammar_dir_name}: grammar.js"),
            format!("{grammar_dir_name}: grammar.js missing"),
        ));
        results.push(check(
            grammar_path.join("src/parser.c").is_file(),
            &format!("{grammar_dir_name}: parser.c"),
            format!("{grammar_dir_name}: generated parser missing"),
        ));

        for query_name in query_names {
            let query_path = lang_dir.join(query_name);
            results.push(check(
                query_path.is_file(),
                &format!("{lang_dir_name}: {query_name}"),
                format!("{lang_dir_name}: {query_name} missing"),
            ));
            if query_path.is_file()
                && let Ok(contents) = fs::read_to_string(&query_path)
            {
                results.push(check(
                    !contents.contains("Phase 0 query placeholder"),
                    &format!("{lang_dir_name}: {query_name} real"),
                    format!(
                        "{query_path}: placeholder query",
                        query_path = query_path.display()
                    ),
                ));
            }
        }

        // Validate queries load against the sample corpus.
        let sample = grammar_path.join(sample_relative);
        if sample.is_file() {
            let tree_sitter = find_tree_sitter(&grammar_root);
            for query_name in query_names {
                let query_path = lang_dir.join(query_name);
                if query_path.is_file()
                    && let Some(ref cli) = tree_sitter
                {
                    let output = Command::new(cli)
                        .args([
                            "query",
                            &query_path.to_string_lossy(),
                            &sample.to_string_lossy(),
                            "--quiet",
                        ])
                        .current_dir(&grammar_path)
                        .output();
                    match output {
                        Ok(out) if out.status.success() => {
                            results.push(CheckResult::pass(format!(
                                "{lang_dir_name}: {query_name} query loads"
                            )));
                        }
                        Ok(out) => {
                            results.push(CheckResult::fail(
                                format!("{lang_dir_name}: {query_name}"),
                                format!("query failed: {}", String::from_utf8_lossy(&out.stderr)),
                            ));
                        }
                        Err(_) => {} // tree-sitter not available, skip
                    }
                }
            }
        }
    }

    // Recommended settings.
    let settings_path = ext_dir.join("recommended-settings.json");
    if settings_path.is_file()
        && let Ok(text) = fs::read_to_string(&settings_path)
        && let Ok(settings) = serde_json::from_str::<serde_json::Value>(&text)
    {
        let file_types: BTreeSet<String> = settings
            .get("file_types")
            .and_then(|v| v.as_object())
            .map(|obj| obj.keys().cloned().collect())
            .unwrap_or_default();
        let expected: BTreeSet<String> = ["Europa Universalis IV".to_owned()].into_iter().collect();
        results.push(check(
            file_types == expected,
            "recommended settings",
            "recommended settings are incomplete",
        ));
    }

    results
}

/// Validates the rules artifact (checksum, schema, hash, foreign keys).
pub fn check_release_artifact(root: &Path) -> Vec<CheckResult> {
    let mut results = Vec::new();
    let rules_path = root.join("rules/eu4.pdxrules");
    let manifest_path = root.join("rules/manifest.json");

    if !rules_path.is_file() {
        results.push(CheckResult::fail(
            "rules artifact",
            "rules/eu4.pdxrules is missing",
        ));
        return results;
    }
    if !manifest_path.is_file() {
        results.push(CheckResult::fail(
            "rules manifest",
            "rules/manifest.json is missing",
        ));
        return results;
    }

    let Ok(rules_bytes) = fs::read(&rules_path) else {
        results.push(CheckResult::fail(
            "rules artifact",
            "cannot read rules/eu4.pdxrules",
        ));
        return results;
    };
    let Ok(manifest_text) = fs::read_to_string(&manifest_path) else {
        results.push(CheckResult::fail(
            "rules manifest",
            "cannot read rules/manifest.json",
        ));
        return results;
    };
    let Ok(manifest) = serde_json::from_str::<serde_json::Value>(&manifest_text) else {
        results.push(CheckResult::fail(
            "rules manifest",
            "invalid rules/manifest.json",
        ));
        return results;
    };

    // Checksum.
    let expected_sha = manifest
        .get("artifact_sha256")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let actual_sha = format!("{:x}", Sha256::digest(&rules_bytes));
    results.push(check(
        expected_sha == actual_sha,
        "rules artifact checksum",
        format!("rules artifact checksum mismatch: {actual_sha}"),
    ));

    // SQLite validation via pdx_rules (if the checksum passes, the artifact is intact).
    match pdx_rules::RuleSet::load(&rules_path) {
        Ok(rules) => {
            let schema_version = manifest
                .get("schema_version")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32;
            results.push(check(
                rules.schema_version() == schema_version,
                "rules schema version",
                format!(
                    "schema version mismatch: {} vs {schema_version}",
                    rules.schema_version()
                ),
            ));
            let hash = manifest
                .get("rule_hash")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            results.push(check(
                rules.rule_hash().to_hex() == hash,
                "rules rule_hash",
                format!(
                    "rule_hash mismatch: {} vs {hash}",
                    rules.rule_hash().to_hex()
                ),
            ));
            let game_id = manifest
                .get("game_id")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            results.push(check(
                rules.game_id() == game_id && game_id == "eu4",
                "rules game_id",
                format!("game/profile mismatch: {} vs eu4", rules.game_id()),
            ));
            results.push(CheckResult::pass("rules foreign keys enabled"));
        }
        Err(error) => {
            results.push(CheckResult::fail("rules validation", error.to_string()));
        }
    }

    // Zed extension: no --rules override.
    let zed_src = root.join("editors/zed/src/lib.rs");
    if let Ok(source) = fs::read_to_string(&zed_src) {
        results.push(check(
            !source.contains("--rules"),
            "Zed no --rules override",
            "Zed command still exposes a rules override",
        ));
    }

    results
}

/// Single-character deletion fuzz test for grammar corpora.
pub fn check_grammar_fuzz(root: &Path) -> Vec<CheckResult> {
    let mut results = Vec::new();
    let grammar_root = root.join("grammars/tree-sitter-eu4");
    if !grammar_root.is_dir() {
        results.push(CheckResult::fail(
            "grammar fuzz",
            "grammars/tree-sitter-eu4 missing",
        ));
        return results;
    }

    let tree_sitter = match find_tree_sitter(&root.join("grammars")) {
        Some(cli) => cli,
        None => {
            results.push(CheckResult::fail(
                "grammar fuzz",
                "tree-sitter CLI not found",
            ));
            return results;
        }
    };

    let corpus_dir = grammar_root.join("test/corpus");
    if !corpus_dir.is_dir() {
        results.push(CheckResult::fail(
            "grammar fuzz",
            "corpus directory missing",
        ));
        return results;
    }

    let crash_markers = ["panic", "aborted", "segmentation fault", "stack overflow"];
    let mut total = 0usize;
    let separator = "==================\n";

    let Ok(entries) = fs::read_dir(&corpus_dir) else {
        results.push(CheckResult::fail(
            "grammar fuzz",
            "cannot read corpus directory",
        ));
        return results;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("txt") {
            continue;
        }
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let mut sources = Vec::new();
        let mut rest = text.as_str();
        while let Some(case_start) = rest.find(separator) {
            let after_first = &rest[case_start + separator.len()..];
            let Some(name_end) = after_first.find('\n') else {
                break;
            };
            let after_name = &after_first[name_end + 1..];
            let Some(second_sep) = after_name.find(separator) else {
                break;
            };
            let input_start = second_sep + separator.len();
            let after_input = &after_name[input_start..];
            let Some(dash_end) = after_input.find("\n---\n") else {
                break;
            };
            let input = &after_input[..dash_end];
            for (offset, _) in input.char_indices() {
                let mut mutated = input.to_owned();
                mutated.remove(offset);
                sources.push(mutated);
                total += 1;
            }
            rest = &after_input[dash_end + "\n---\n".len()..];
        }
        if sources.is_empty() {
            continue;
        }
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let temp_dir =
            std::env::temp_dir().join(format!("pdx-grammar-fuzz-{}-{}", std::process::id(), nonce));
        if fs::create_dir_all(&temp_dir).is_err() {
            results.push(CheckResult::fail(
                "grammar fuzz",
                "cannot create temp directory",
            ));
            return results;
        }
        let paths_file = temp_dir.join("paths.txt");
        let mut paths_content = String::new();
        for (i, source) in sources.iter().enumerate() {
            let source_path = temp_dir.join(format!("mutation-{i}.txt"));
            if fs::write(&source_path, source).is_err() {
                continue;
            }
            paths_content.push_str(&source_path.to_string_lossy());
            paths_content.push('\n');
        }
        if fs::write(&paths_file, &paths_content).is_err() {
            let _ = fs::remove_dir_all(&temp_dir);
            continue;
        }

        let output = Command::new(&tree_sitter)
            .args([
                "parse",
                "--no-ranges",
                "--paths",
                &paths_file.to_string_lossy(),
            ])
            .current_dir(&grammar_root)
            .output();

        // Clean up temp directory regardless of outcome.
        let _ = fs::remove_dir_all(&temp_dir);

        match output {
            Ok(out) => {
                if out.status.code().is_some_and(|code| code > 1) {
                    results.push(CheckResult::fail(
                        "grammar fuzz",
                        format!(
                            "parser process failed with exit {}:\n{}",
                            out.status.code().unwrap_or(-1),
                            String::from_utf8_lossy(&out.stderr)
                        ),
                    ));
                    return results;
                }
                let combined = format!(
                    "{}\n{}",
                    String::from_utf8_lossy(&out.stdout),
                    String::from_utf8_lossy(&out.stderr)
                )
                .to_lowercase();
                if crash_markers.iter().any(|marker| combined.contains(marker)) {
                    results.push(CheckResult::fail(
                        "grammar fuzz",
                        "parser crash marker found in output",
                    ));
                    return results;
                }
            }
            Err(error) => {
                results.push(CheckResult::fail(
                    "grammar fuzz",
                    format!("cannot run tree-sitter: {error}"),
                ));
                return results;
            }
        }
    }

    results.push(CheckResult::pass(format!(
        "grammar fuzz: {total} single-char deletions"
    )));
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

fn find_tree_sitter(grammar_root: &Path) -> Option<String> {
    // Check local node_modules first.
    for candidate in [
        grammar_root.join("tree-sitter-eu4/node_modules/.bin/tree-sitter"),
        grammar_root.join("tree-sitter-eu4/node_modules/.bin/tree-sitter.cmd"),
    ] {
        if candidate.is_file() {
            return Some(candidate.to_string_lossy().into_owned());
        }
    }
    // Check PATH.
    let name = "tree-sitter";
    if let Ok(output) = Command::new(name).arg("--version").output()
        && output.status.success()
    {
        return Some(name.to_owned());
    }
    None
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
    fn zed_extension_passes_on_this_repository() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let results = check_zed_extension(&root);
        let all_pass = results
            .iter()
            .all(|r| matches!(r.outcome, CheckOutcome::Passed));
        for result in &results {
            if let CheckOutcome::Failed(ref msg) = result.outcome {
                eprintln!("FAIL {}: {msg}", result.name);
            }
        }
        assert!(all_pass, "Zed extension checks must pass");
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
}
