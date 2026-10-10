//! Extension contracts and JavaScript behavior-test/build orchestration.
use crate::{args::Args, process, report};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
pub mod i18n;
const HELP: &str = "tools editor check|compile|package [--root REPO] [--output VSIX]\ntools editor test [unit|contract|ci|host] [--root REPO]\ntools rules check|fmt SOURCE [--check] [--expanded]\ntools rules schema [--output PATH]";
fn require(condition: bool, message: impl Into<String>, errors: &mut Vec<String>) {
    if !condition {
        errors.push(message.into());
    }
}
fn strings(value: &Value) -> BTreeSet<&str> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect()
}
pub fn check(root: &Path) -> Result<Vec<String>, String> {
    let ext = root.join("editors/vscode");
    let package = report::json(&ext.join("package.json"))?;
    let en = report::json(&ext.join("package.nls.json"))?;
    let zh = report::json(&ext.join("package.nls.zh-cn.json"))?;
    let contract = crate::editor_contract::data();
    let mut errors = Vec::new();
    require(
        strings(&package["files"]).contains("node_modules/**"),
        "VSIX must include production node_modules",
        &mut errors,
    );
    require(
        package["l10n"] == "./l10n",
        "VS Code must load runtime translations from ./l10n",
        &mut errors,
    );
    require(
        strings(&package["files"]).contains("l10n/**"),
        "VSIX must include runtime translation bundles",
        &mut errors,
    );
    require(
        package["dependencies"]["vscode-languageclient"].is_string(),
        "vscode-languageclient must remain a runtime dependency",
        &mut errors,
    );
    let languages = package["contributes"]["languages"]
        .as_array()
        .ok_or("missing language contributions")?;
    require(
        languages.len() == 2,
        "exactly EU4 and Localisation must be contributed",
        &mut errors,
    );
    let eu4 = languages
        .iter()
        .find(|v| v["id"] == "eu4")
        .ok_or("missing EU4 language")?;
    let loc = languages
        .iter()
        .find(|v| v["id"] == "localisation")
        .ok_or("missing Localisation language")?;
    let expected = strings(&contract["patterns"]);
    let actual = strings(&eu4["filenamePatterns"]);
    for pattern in &expected {
        require(
            actual.contains(pattern),
            format!("missing language pattern {pattern}"),
            &mut errors,
        );
    }
    for pattern in actual {
        require(
            !pattern.contains("/**")
                && !["**/common/*.txt", "**/common/**/*.txt"].contains(&pattern),
            format!("recursive/broad script language pattern {pattern}"),
            &mut errors,
        );
        require(
            !pattern.starts_with("**/common/") || expected.contains(pattern),
            format!("unexpected common pattern {pattern}"),
            &mut errors,
        );
        require(
            ![".yml", ".yaml", ".asset", ".sfx"]
                .iter()
                .any(|s| pattern.to_lowercase().contains(s)),
            format!("script language claims another parser zone: {pattern}"),
            &mut errors,
        );
    }
    require(
        strings(&loc["filenamePatterns"]).contains("**/localisation/**/*"),
        "localisation must claim its recursive tree",
        &mut errors,
    );
    let activation = strings(&package["activationEvents"]);
    for event in [
        "onLanguage:eu4",
        "onLanguage:localisation",
        "workspaceContains:**/localisation/**/*",
    ] {
        require(
            activation.contains(event),
            format!("missing activation {event}"),
            &mut errors,
        );
    }
    require(
        !activation.contains("onStartupFinished"),
        "extension must not start in unrelated workspaces",
        &mut errors,
    );
    for event in activation {
        require(
            !event.contains("/**") || event.ends_with("/localisation/**/*"),
            format!("recursive script activation {event}"),
            &mut errors,
        );
        if let Some(pattern) = event
            .strip_prefix("workspaceContains:")
            .filter(|p| p.starts_with("**/common/"))
        {
            require(
                expected.contains(pattern),
                format!("unexpected common activation {event}"),
                &mut errors,
            );
        }
    }
    require(
        package["capabilities"]["untrustedWorkspaces"]["supported"] == false,
        "downloads and executable startup require a trusted workspace",
        &mut errors,
    );
    let properties = &package["contributes"]["configuration"]["properties"];
    for name in ["paradoxcode.serverVersion", "paradoxcode.serverRepository"] {
        require(
            properties.get(name).is_none(),
            format!("workspace must not redirect downloads through {name}"),
            &mut errors,
        );
    }
    for name in strings(&contract["settings"]) {
        let setting = &properties[name];
        let key = setting["markdownDescription"]
            .as_str()
            .and_then(|s| s.strip_prefix('%'))
            .and_then(|s| s.strip_suffix('%'));
        require(
            key.is_some_and(|k| en[k].is_string() && zh[k].is_string()),
            format!("missing setting or bilingual description: {name}"),
            &mut errors,
        );
    }
    for (name, default) in [
        ("completion.iconPreview", json!(true)),
        ("localisation.transparentEncoding", json!(false)),
        ("localisation.autoOpen", json!("needsTranscode")),
        ("preview.gameFonts", json!(true)),
        ("preview.chineseFontMod", json!("")),
        ("hover.texturePreview", json!(true)),
        ("hover.missionCard", json!(true)),
        ("hover.eventCard", json!(true)),
        ("workspaceWideDiagnostics", json!(false)),
    ] {
        require(
            properties[format!("paradoxcode.{name}")]["default"] == default,
            format!("incorrect default for {name}"),
            &mut errors,
        );
    }
    require(
        properties["paradoxcode.localisation.autoOpen"]["enum"]
            == json!(["needsTranscode", "always", "off"]),
        "autoOpen alternatives drifted",
        &mut errors,
    );
    let commands = package["contributes"]["commands"]
        .as_array()
        .ok_or("missing commands")?;
    for name in [
        "showMissionPreview",
        "installServer",
        "selectServer",
        "selectGameDirectory",
        "reloadServer",
        "openOutput",
        "exportDiagnostics",
        "openMissionIconPicker",
        "addDependency",
        "removeDependency",
        "openDependencySettings",
        "updateIndexCaches",
    ] {
        let command = commands
            .iter()
            .find(|c| c["command"] == format!("paradoxcode.{name}"));
        require(
            command.is_some(),
            format!("missing command {name}"),
            &mut errors,
        );
        if let Some(command) = command {
            let key = command["title"]
                .as_str()
                .and_then(|s| s.strip_prefix('%'))
                .and_then(|s| s.strip_suffix('%'));
            require(
                key.is_some_and(|k| en[k].is_string() && zh[k].is_string()),
                format!("missing command translations {name}"),
                &mut errors,
            );
        }
    }
    require(
        package["contributes"]["views"]["explorer"]
            .as_array()
            .is_some_and(|a| a.iter().any(|v| v["id"] == "paradoxcode.loadedFiles")),
        "missing loaded-files Explorer view",
        &mut errors,
    );
    let walkthrough = package["contributes"]["walkthroughs"]
        .as_array()
        .and_then(|a| a.iter().find(|w| w["id"] == "paradoxcode.gettingStarted"));
    require(
        walkthrough.is_some_and(|w| {
            w["steps"].as_array().is_some_and(|a| {
                a.len() >= 6
                    && a.iter()
                        .all(|s| s["media"]["markdown"] == "media/getting-started.md")
            })
        }),
        "getting-started walkthrough drifted",
        &mut errors,
    );
    for (language, scope, path) in [
        ("eu4", "source.eu4", "eu4.tmLanguage.json"),
        (
            "localisation",
            "source.localisation",
            "localisation.tmLanguage.json",
        ),
    ] {
        let grammar = report::json(&ext.join("syntaxes").join(path))?;
        require(
            grammar["scopeName"] == scope
                && grammar["patterns"].as_array().is_some_and(|a| a.len() >= 5),
            format!("invalid {language} grammar"),
            &mut errors,
        );
        require(
            package["contributes"]["grammars"]
                .as_array()
                .is_some_and(|a| {
                    a.iter().any(|g| {
                        g["language"] == language && g["path"] == format!("./syntaxes/{path}")
                    })
                }),
            format!("missing grammar contribution {language}"),
            &mut errors,
        );
        require(
            package["contributes"]["configurationDefaults"][format!("[{language}]")]["editor.semanticHighlighting.enabled"]
                == true,
            format!("semantic highlighting disabled for {language}"),
            &mut errors,
        );
        if language == "localisation" {
            require(
                ["yml", "yaml"]
                    .iter()
                    .all(|s| strings(&grammar["fileTypes"]).contains(s)),
                "localisation grammar must include YAML extensions",
                &mut errors,
            );
        }
    }
    require(
        report::json(&ext.join("localisation-language-configuration.json"))?["comments"]["lineComment"]
            == "#",
        "localisation must recognize # comments",
        &mut errors,
    );
    require(
        package["contributes"]["menus"]["editor/context"]
            .as_array()
            .is_some_and(|a| {
                a.iter().any(|m| {
                    m["command"] == "paradoxcode.openMissionIconPicker"
                        && m["when"] == "paradoxcodeMissionFile"
                })
            }),
        "mission icon picker context menu entry missing",
        &mut errors,
    );
    for key in [
        "extension.displayName",
        "walkthrough.gettingStarted.title",
        "walkthrough.vanillaData.description",
        "commands.showMissionPreview.title",
        "configuration.serverPath.description",
    ] {
        require(
            en[key].is_string() && zh[key].is_string(),
            format!("missing package translation {key}"),
            &mut errors,
        );
    }
    for (file, markers) in contract["markers"]
        .as_object()
        .ok_or("invalid source markers")?
    {
        let text = fs::read_to_string(ext.join(file)).map_err(|e| e.to_string())?;
        for marker in strings(markers) {
            require(
                text.contains(marker),
                format!("{file}: missing behavior marker {marker}"),
                &mut errors,
            );
        }
    }
    let extension = fs::read_to_string(ext.join("src/extension.ts")).map_err(|e| e.to_string())?;
    require(extension.contains("options.workspaceWideDiagnostics = config.get<boolean>('workspaceWideDiagnostics', false)"),"workspace diagnostics default must be forwarded",&mut errors);
    for pattern in regex::Regex::new(r"\{ pattern: '(\*\*/common/[^']+)' \}")
        .unwrap()
        .captures_iter(&extension)
    {
        require(
            expected.contains(&pattern[1]),
            format!("unexpected language-client pattern {}", &pattern[1]),
            &mut errors,
        );
    }
    let installer =
        fs::read_to_string(ext.join("src/serverInstaller.ts")).map_err(|e| e.to_string())?;
    for (key, min) in [("DOWNLOAD_TIMEOUT_MS", 60000), ("MAX_DOWNLOAD_ATTEMPTS", 2)] {
        let pattern =
            regex::Regex::new(&format!("const {key} = ([0-9_]+)")).map_err(|e| e.to_string())?;
        let value = pattern
            .captures(&installer)
            .and_then(|c| c[1].replace('_', "").parse::<u64>().ok());
        require(
            value.is_some_and(|v| v >= min),
            format!("installer {key} below minimum {min}"),
            &mut errors,
        );
    }
    for file in [
        "README.md",
        "package.nls.json",
        "package.nls.zh-cn.json",
        "media/getting-started.md",
        "LICENSE",
    ] {
        require(
            ext.join(file).is_file(),
            format!("packaged asset missing: {file}"),
            &mut errors,
        );
    }
    require(
        package["engines"]["vscode"] == "^1.99.0"
            && package["devDependencies"]["@types/vscode"] == "1.99.0",
        "language-model tool API version contract drifted",
        &mut errors,
    );
    let tools = package["contributes"]["languageModelTools"]
        .as_array()
        .ok_or("missing agent tools")?;
    require(
        tools.len() == strings(&contract["tools"]).len(),
        "agent tool count drifted",
        &mut errors,
    );
    let mut references = BTreeSet::new();
    for name in strings(&contract["tools"]) {
        let tool = tools
            .iter()
            .find(|t| t["name"] == name)
            .ok_or_else(|| format!("missing agent tool {name}"))?;
        require(
            tool["modelDescription"]
                .as_str()
                .is_some_and(|s| s.len() >= 40)
                && tool["inputSchema"]["type"] == "object"
                && tool["canBeReferencedInPrompt"] == true
                && tool["when"] == "paradoxcodeServerRunning",
            format!("invalid agent declaration {name}"),
            &mut errors,
        );
        require(
            tool["toolReferenceName"]
                .as_str()
                .is_some_and(|s| !s.is_empty() && references.insert(s)),
            format!("invalid/duplicate tool reference {name}"),
            &mut errors,
        );
    }
    require(
        !package["contributes"]["chatParticipants"]
            .as_array()
            .is_some_and(|a| a.iter().any(|c| c["id"] == "paradoxcode.modding")),
        "retired chat participant must not return",
        &mut errors,
    );
    let native = pdc::mcp::tool_manifest();
    require(
        native
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect::<BTreeSet<_>>()
            == tools
                .iter()
                .map(|t| t["name"].as_str().unwrap())
                .collect::<BTreeSet<_>>(),
        "native MCP and extension tool surfaces differ",
        &mut errors,
    );
    for marker in [
        "paradoxcode_validate_text",
        "paradoxcode_rules",
        "paradoxcode_search",
        "paradoxcode_loc_get",
        "paradoxcode_loc_list",
        "UnknownLocalisationKey",
        "Zone discipline",
    ] {
        require(
            pdc::mcp::INSTRUCTIONS.contains(marker),
            format!("missing MCP instruction {marker}"),
            &mut errors,
        );
    }
    errors.extend(i18n::check(&ext)?);
    Ok(errors)
}
fn compile(root: &Path) -> Result<(), String> {
    let ext = root.join("editors/vscode");
    process::run(
        process::command("node")
            .arg(ext.join("node_modules/typescript/bin/tsc"))
            .args(["-p", "."])
            .current_dir(ext),
    )
}
fn vsce(root: &Path, args: &[&str]) -> Result<std::process::Output, String> {
    let ext = root.join("editors/vscode");
    process::capture(
        process::command("node")
            .arg(ext.join("node_modules/@vscode/vsce/vsce"))
            .args(args)
            .current_dir(ext),
    )
}
fn package_contract(root: &Path) -> Result<(), String> {
    let listing = vsce(root, &["ls"])?;
    let text = String::from_utf8_lossy(&listing.stdout);
    let files = text.lines().map(str::trim).collect::<BTreeSet<_>>();
    for file in [
        "node_modules/vscode-languageclient/lib/node/main.js",
        "node_modules/vscode-jsonrpc/lib/node/main.js",
        "node_modules/vscode-languageserver-protocol/lib/common/api.js",
        "l10n/bundle.l10n.json",
        "l10n/bundle.l10n.zh-cn.json",
    ] {
        if !files.contains(file) {
            return Err(format!("VSIX runtime asset omitted: {file}"));
        }
    }
    if files.iter().any(|f| {
        f.starts_with("node_modules/typescript/")
            || f.starts_with("node_modules/@vscode/test-electron/")
    }) {
        return Err("VSIX includes development dependencies".into());
    }
    Ok(())
}
pub fn execute(arguments: &[String]) -> Result<String, String> {
    let Some((name, arguments)) = arguments.split_first() else {
        return Ok(HELP.into());
    };
    let args = Args::parse(
        arguments,
        &["--root", "--repo", "--output", "--server"],
        &["--contract"],
    )?;
    if args.help() || ["help", "--help", "-h"].contains(&name.as_str()) {
        return Ok(HELP.into());
    }
    let root = args.root()?;
    match name.as_str() {
        "compile" => {
            compile(&root)?;
        }
        "check" => {
            let errors = check(&root)?;
            if !errors.is_empty() {
                return Err(errors.join("\n"));
            }
        }
        "package" => {
            compile(&root)?;
            let errors = check(&root)?;
            if !errors.is_empty() {
                return Err(errors.join("\n"));
            }
            package_contract(&root)?;
            let package = report::json(&root.join("editors/vscode/package.json"))?;
            let default = root.join("target/dist").join(format!(
                "{}-{}.vsix",
                package["name"].as_str().ok_or("missing package name")?,
                package["version"]
                    .as_str()
                    .ok_or("missing package version")?
            ));
            let output = report::output(&root, &args.path("--output").unwrap_or(default), "")?;
            if let Some(parent) = output.parent() {
                fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            vsce(
                &root,
                &[
                    "package",
                    "--out",
                    output.to_str().ok_or("non-UTF8 VSIX output")?,
                ],
            )?;
            if !output.is_file() {
                return Err("VSCE did not produce a package".into());
            }
            return Ok(output.display().to_string());
        }
        "test" => {
            let group = args
                .positional
                .first()
                .map(String::as_str)
                .unwrap_or("unit");
            if !["unit", "contract", "ci", "host"].contains(&group) {
                return Err("unknown editor test group".into());
            }
            compile(&root)?;
            let errors = check(&root)?;
            if !errors.is_empty() {
                return Err(errors.join("\n"));
            }
            if group == "host" {
                process::run(
                    process::command("node")
                        .arg(root.join("editors/vscode/test/host.mjs"))
                        .env("RUN_VSCODE_HOST_TESTS", "1"),
                )?;
            } else {
                for name in [
                    "assets.mjs",
                    "icon-picker.mjs",
                    "loc-format.mjs",
                    "server-path.mjs",
                    "search.mjs",
                    "transparent-loc.mjs",
                ] {
                    process::run(
                        process::command("node").arg(root.join("editors/vscode/test").join(name)),
                    )?;
                }
                if group == "ci" || group == "unit" {
                    crate::e2e::run(&root, args.get("--server"))?;
                }
                if group == "ci" || group == "contract" {
                    package_contract(&root)?;
                    if group == "ci" {
                        execute(&[
                            "package".into(),
                            "--root".into(),
                            root.display().to_string(),
                            "--output".into(),
                            root.join("target/paradoxcode-vscode-contract.vsix")
                                .display()
                                .to_string(),
                        ])?;
                    }
                }
            }
        }
        _ => return Err(format!("unknown editor command: {name}\n{HELP}")),
    };
    Ok(format!("editor {name} passed"))
}
pub fn rules(arguments: &[String]) -> Result<String, String> {
    let Some((command, args)) = arguments.split_first() else {
        return Ok(HELP.into());
    };
    if ["--help", "-h", "help"].contains(&command.as_str()) {
        return Ok(HELP.into());
    }
    if command == "bake" {
        let args = Args::parse(args, &["--source", "--output", "--root"], &[])?;
        if args.help() {
            return Ok("tools rules bake --source DIR [--output PATH] [--root DIR]".into());
        }
        let root = args.root()?;
        let baked = rules::bake::compile(Path::new(args.required("--source")?))?;
        let output = report::output(
            &root,
            &args
                .path("--output")
                .unwrap_or(root.join("target/rules/compiled.ir.json")),
            "",
        )?;
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        crate::release::atomic_write(&output, &baked.bytes).map_err(|e| e.to_string())?;
        return Ok(format!(
            "compiled {} schemas / {} fields (rule_hash={}); {}",
            baked.ir.schemas.len(),
            baked.ir.fields.len(),
            baked.ir.fingerprint(),
            output.display()
        ));
    }
    if !["check", "fmt", "schema"].contains(&command.as_str()) {
        return Err("rules command must be check, fmt, schema or bake".into());
    }
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    process::run(
        process::command("cargo")
            .current_dir(&root)
            .args([
                "run", "--locked", "-p", "rules", "--bin", "rulec", "--", command,
            ])
            .args(args),
    )?;
    Ok(format!("rules {command} passed"))
}
