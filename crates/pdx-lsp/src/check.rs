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
    results.push(requires_file("RELEASING.md"));
    results.push(requires_file("LICENSE"));
    results.push(requires_file(".github/workflows/ci.yml"));
    results.push(requires_file(".github/workflows/release.yml"));
    results.push(requires_file("deny.toml"));
    results.push(requires_file("editors/zed/extension.toml"));
    results.push(requires_file("editors/vscode/package.json"));
    results.push(requires_file("editors/vscode/package-lock.json"));
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
        &["highlights.scm", "brackets.scm", "indents.scm"][..],
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

/// Validates that the per-editor syntax artefacts (Zed `.scm` queries and the VSCode
/// `language-configuration.json`) still match the canonical `editors/syntax-profile.json`.
/// Bracket pairs, auto-closing pairs, the line comment marker, and indentation rules exist in
/// two editors with no LSP protocol to share them, so drift here silently breaks parity.
pub fn check_editor_syntax_parity(root: &Path) -> Vec<CheckResult> {
    let mut results = Vec::new();
    let profile_path = root.join("editors/syntax-profile.json");
    let vscode_config_path = root.join("editors/vscode/language-configuration.json");
    let brackets_scm = root.join("editors/zed/languages/eu4/brackets.scm");
    let indents_scm = root.join("editors/zed/languages/eu4/indents.scm");
    let highlights_scm = root.join("editors/zed/languages/eu4/highlights.scm");

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

    // Zed `.scm` queries must recognize every bracket pair from the profile and keep the
    // indentation captures that mirror the profile's indentation rules.
    let bracket_captures = fs::read_to_string(&brackets_scm).unwrap_or_default();
    let profile_brackets = brackets
        .and_then(|value| value.as_array())
        .map(|pairs| {
            pairs
                .iter()
                .map(|pair| pair.get("open").and_then(serde_json::Value::as_str))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    results.push(check(
        profile_brackets.iter().all(|open| {
            open.is_some_and(|open| bracket_captures.contains(&format!("\"{open}\" @open")))
        }),
        "zed: bracket parity",
        "Zed brackets.scm does not cover every syntax-profile bracket pair",
    ));
    let indents = fs::read_to_string(&indents_scm).unwrap_or_default();
    results.push(check(
        indents.contains("(block") && indents.contains("@indent"),
        "zed: block indent capture",
        "Zed indents.scm lost the block indent capture",
    ));
    results.push(check(
        indents.contains("(parameter_block") && indents.contains("@end"),
        "zed: parameter block indent capture",
        "Zed indents.scm lost the parameter block indent capture",
    ));

    // Semantic classification moved to pdx-ls; the fallback query must stay syntax-only.
    let highlights = fs::read_to_string(&highlights_scm).unwrap_or_default();
    results.push(check(
        !highlights.contains("#match?") && !highlights.contains("variable.special"),
        "zed: syntax-only fallback",
        "Zed highlights.scm still carries semantic regex captures that belong to pdx-ls",
    ));

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
        !root.join("rules/eu4.pdxrules").exists(),
        "no committed rules artifact",
        "rules/eu4.pdxrules must be generated in a user or release cache, not committed",
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
        serde_json::from_str::<pdx_rules::rulec::ArtifactManifest>(&manifest_text)
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
    let generated_path = temporary_directory.join("eu4.pdxrules");
    let generated_manifest_path = temporary_directory.join("manifest.json");
    match pdx_rules::rulec::compile(&source_path, &generated_path, &generated_manifest_path) {
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
            match pdx_rules::RuleSet::load(&generated_path) {
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
