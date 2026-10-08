//! Derived documentation and local documentation contracts.
//!
//! Manifests, the rule source model, and the diagnostic registry own machine facts.
//! This module renders their reader views and checks links/complete examples without
//! fetching URLs, executing documentation shell snippets, or reading local game data.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use ide::DiagnosticCode;
use serde_json::Value;

const REGENERATE: &str = "cargo run --locked -p tools -- documentation write";
const DIAGNOSTICS: &str = "crates/ide/DIAGNOSTICS.md";

fn read(path: &Path) -> Result<String, String> {
    fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))
}

fn json(path: &Path) -> Result<Value, String> {
    serde_json::from_str(&read(path)?).map_err(|error| format!("{}: {error}", path.display()))
}

fn pretty(value: &Value) -> Result<String, String> {
    serde_json::to_string_pretty(value).map_err(|error| error.to_string())
}

fn text(value: Option<&Value>, nls: &Value) -> Result<String, String> {
    let raw = value.and_then(Value::as_str).unwrap_or_default();
    if let Some(key) = raw.strip_prefix('%').and_then(|v| v.strip_suffix('%')) {
        nls.get(key)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| format!("missing NLS source: {key}"))
    } else {
        Ok(raw.to_owned())
    }
}

fn localized(value: &Value, nls: &Value) -> Result<Value, String> {
    match value {
        Value::String(_) => Ok(Value::String(text(Some(value), nls)?)),
        Value::Array(values) => values
            .iter()
            .map(|v| localized(v, nls))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        Value::Object(values) => values
            .iter()
            .map(|(key, value)| Ok((key.clone(), localized(value, nls)?)))
            .collect::<Result<serde_json::Map<_, _>, String>>()
            .map(Value::Object),
        _ => Ok(value.clone()),
    }
}

fn cell(value: &str) -> String {
    value.replace('|', "\\|").replace('\n', " ")
}

fn generated(title: &str, source: &str) -> String {
    format!("# {title}\n\n<!-- Generated; edit {source}, then run {REGENERATE}. -->\n\n")
}

fn extension_reference(package: &Value, nls: &Value) -> Result<String, String> {
    let mut out = generated("Extension reference", "package.json / package.nls.json");
    out.push_str("[Usage and troubleshooting](README.md). This view reflects VS Code's manifest defaults.\n\n## Settings\n\n");
    let configuration = &package["contributes"]["configuration"];
    let groups: Vec<&Value> = match configuration.as_array() {
        Some(groups) => groups.iter().collect(),
        None => vec![configuration],
    };
    for group in groups {
        let properties = group["properties"]
            .as_object()
            .ok_or_else(|| "extension configuration is missing properties".to_owned())?;
        for (name, schema) in properties {
            let description = text(
                schema
                    .get("markdownDescription")
                    .or_else(|| schema.get("description")),
                nls,
            )?;
            let mut shape = localized(schema, nls)?;
            if let Some(shape) = shape.as_object_mut() {
                shape.remove("description");
                shape.remove("markdownDescription");
            }
            out.push_str(&format!(
                "### `{name}`\n\n{description}\n\n```json\n{}\n```\n\n",
                pretty(&shape)?
            ));
        }
    }
    out.push_str("## Commands\n\n| Command | Title |\n| --- | --- |\n");
    for command in package["contributes"]["commands"]
        .as_array()
        .into_iter()
        .flatten()
    {
        out.push_str(&format!(
            "| `{}` | {} |\n",
            command["command"].as_str().unwrap_or_default(),
            cell(&text(command.get("title"), nls)?)
        ));
    }
    Ok(out)
}

fn source_reference() -> Result<String, String> {
    let schema: Value =
        serde_json::from_str(&rules::source::json_schema_pretty()).map_err(|e| e.to_string())?;
    let mut out = generated("Rule source reference", "src/source.rs");
    out.push_str("[Language semantics](LANGUAGE.md) · [Rule-package workflow](../../rules/README.md).\n\nEach object below comes from the compiler's generated JSON Schema. `Required` and\n`default` describe source serialization; additional semantic constraints are checked by `rulec`.\n\n");
    let mut objects = vec![("RuleFile".to_owned(), &schema)];
    if let Some(definitions) = schema["$defs"].as_object() {
        objects.extend(
            definitions
                .iter()
                .map(|(name, value)| (name.clone(), value)),
        );
    }
    for (name, object) in objects {
        out.push_str(&format!("## {name}\n\n"));
        if let Some(description) = object["description"].as_str() {
            out.push_str(description);
            out.push_str("\n\n");
        }
        if let Some(properties) = object["properties"].as_object() {
            out.push_str("| Field | Required | Source default | Shape | Meaning |\n| --- | --- | --- | --- | --- |\n");
            for (field, schema) in properties {
                let required = object["required"]
                    .as_array()
                    .is_some_and(|values| values.iter().any(|v| v.as_str() == Some(field)));
                let default = schema
                    .get("default")
                    .map_or_else(|| "—".to_owned(), |value| format!("`{value}`"));
                let mut shape = schema.clone();
                if let Some(properties) = shape.as_object_mut() {
                    properties.remove("description");
                    properties.remove("default");
                }
                out.push_str(&format!(
                    "| `{field}` | {} | {} | `{}` | {} |\n",
                    if required { "yes" } else { "no" },
                    cell(&default),
                    cell(&shape.to_string()),
                    cell(schema["description"].as_str().unwrap_or_default())
                ));
            }
            out.push('\n');
        } else {
            out.push_str(&format!("```json\n{}\n```\n\n", pretty(object)?));
        }
    }
    Ok(out)
}

fn crate_dependencies(root: &Path) -> Result<String, String> {
    let workspace: toml::Value =
        toml::from_str(&read(&root.join("Cargo.toml"))?).map_err(|e| e.to_string())?;
    let members = workspace["workspace"]["members"]
        .as_array()
        .ok_or("missing workspace members")?;
    let mut out = "| Crate | Direct runtime workspace dependencies |\n| --- | --- |\n".to_owned();
    for member in members {
        let directory = member.as_str().ok_or("invalid workspace member")?;
        let manifest: toml::Value =
            toml::from_str(&read(&root.join(directory).join("Cargo.toml"))?)
                .map_err(|e| e.to_string())?;
        let name = manifest["package"]["name"]
            .as_str()
            .ok_or("missing package name")?;
        let dependencies: Vec<String> = manifest
            .get("dependencies")
            .and_then(toml::Value::as_table)
            .into_iter()
            .flatten()
            .filter_map(|(name, value)| {
                value
                    .get("path")
                    .and_then(toml::Value::as_str)
                    .map(|_| format!("`{name}`"))
            })
            .collect();
        out.push_str(&format!(
            "| [{name}]({directory}/src/lib.rs) | {} |\n",
            if dependencies.is_empty() {
                "—".to_owned()
            } else {
                dependencies.join(", ")
            }
        ));
    }
    Ok(out)
}

fn replace_section(source: &str, name: &str, body: &str) -> Result<String, String> {
    let start = format!("<!-- generated:{name}:start -->");
    let end = format!("<!-- generated:{name}:end -->");
    if source.matches(&start).count() != 1 || source.matches(&end).count() != 1 {
        return Err(format!("expected exactly one generated section: {name}"));
    }
    let before = source.find(&start).ok_or("missing start marker")? + start.len();
    let after = source.find(&end).ok_or("missing end marker")?;
    if before > after {
        return Err(format!("reversed section markers: {name}"));
    }
    Ok(format!(
        "{}\n\n{}\n{}",
        &source[..before],
        body.trim_end(),
        &source[after..]
    ))
}

fn headings(source: &str) -> Vec<String> {
    prose_lines(source)
        .into_iter()
        .filter_map(|line| {
            let text = line.trim_start_matches('#');
            (text.len() != line.len() && text.starts_with(' '))
                .then(|| text.trim().trim_end_matches('#').trim().to_owned())
        })
        .collect()
}

fn slug(heading: &str) -> String {
    heading
        .to_lowercase()
        .chars()
        .filter_map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                Some(c)
            } else if c.is_whitespace() {
                Some('-')
            } else {
                None
            }
        })
        .collect()
}

fn prose_lines(source: &str) -> Vec<&str> {
    let mut fence: Option<char> = None;
    source
        .lines()
        .filter(|line| {
            let trimmed = line.trim_start();
            let marker = if trimmed.starts_with("```") {
                Some('`')
            } else if trimmed.starts_with("~~~") {
                Some('~')
            } else {
                None
            };
            if let Some(marker) = marker {
                if fence == Some(marker) {
                    fence = None;
                } else if fence.is_none() {
                    fence = Some(marker);
                }
                false
            } else {
                fence.is_none()
            }
        })
        .collect()
}

fn views(root: &Path) -> Result<Vec<(PathBuf, String)>, String> {
    let mut index = "| Code | Registry default severity |\n| --- | --- |\n".to_owned();
    for code in DiagnosticCode::ALL {
        index.push_str(&format!(
            "| [{}](#{}) | {:?} |\n",
            code.as_str(),
            slug(code.as_str()),
            code.severity()
        ));
    }
    index.push_str("\nPublication and position-specific severity can differ from registry defaults. The explanations below describe those cases.\n");
    let diagnostics = replace_section(&read(&root.join(DIAGNOSTICS))?, "diagnostic-index", &index)?;
    Ok(vec![
        (
            PathBuf::from("editors/vscode/REFERENCE.md"),
            extension_reference(
                &json(&root.join("editors/vscode/package.json"))?,
                &json(&root.join("editors/vscode/package.nls.json"))?,
            )?,
        ),
        (
            PathBuf::from("crates/rules/SOURCE-REFERENCE.md"),
            source_reference()?,
        ),
        (
            PathBuf::from("CONTRIBUTING.md"),
            replace_section(
                &read(&root.join("CONTRIBUTING.md"))?,
                "crate-dependencies",
                &crate_dependencies(root)?,
            )?,
        ),
        (PathBuf::from(DIAGNOSTICS), diagnostics),
    ]
    .into_iter()
    .map(|(path, content)| (path, format!("{}\n", content.trim_end())))
    .collect())
}

/// Regenerates derived files and sections; human-authored prose stays in place.
pub fn write(root: &Path) -> Result<usize, String> {
    let views = views(root)?;
    for (path, content) in &views {
        fs::write(root.join(path), content).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(views.len())
}

fn markdown_files(directory: &Path, output: &mut Vec<PathBuf>) -> Result<(), String> {
    for entry in fs::read_dir(directory).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if entry.file_type().map_err(|e| e.to_string())?.is_symlink() {
            continue;
        }
        if path.is_dir() {
            if matches!(
                entry.file_name().to_str(),
                Some(
                    "target"
                        | "node_modules"
                        | "out"
                        | ".vscode-test"
                        | "runs"
                        | "baselines"
                        | "profiles"
                        | "bin"
                )
            ) {
                continue;
            }
            markdown_files(&path, output)?;
        } else if path.extension().is_some_and(|extension| extension == "md") {
            output.push(path);
        }
    }
    Ok(())
}

fn link_errors(path: &Path, source: &str) -> Result<Vec<String>, String> {
    let mut errors = Vec::new();
    for line in prose_lines(source) {
        let mut rest = line;
        while let Some(index) = rest.find("](") {
            rest = &rest[index + 2..];
            let Some(end) = rest.find(')') else {
                break;
            };
            let destination = rest[..end]
                .split_whitespace()
                .next()
                .unwrap_or_default()
                .trim_matches(['<', '>']);
            rest = &rest[end + 1..];
            if destination.contains(':') || destination.is_empty() {
                continue;
            }
            let (file, anchor) = destination
                .split_once('#')
                .map_or((destination, None), |(f, a)| (f, Some(a)));
            let target = if file.is_empty() {
                path.to_owned()
            } else {
                path.parent().ok_or("missing document parent")?.join(file)
            };
            if !target.exists() {
                errors.push(format!("{}: missing link {destination}", path.display()));
            } else if let Some(anchor) = anchor.filter(|a| !a.is_empty())
                && target
                    .extension()
                    .is_some_and(|extension| extension == "md")
            {
                let mut anchors = BTreeSet::new();
                for heading in headings(&read(&target)?) {
                    let base = slug(&heading);
                    let mut candidate = base.clone();
                    let mut suffix = 0;
                    while anchors.contains(&candidate) {
                        suffix += 1;
                        candidate = format!("{base}-{suffix}");
                    }
                    anchors.insert(candidate);
                }
                if !anchors.contains(anchor) {
                    errors.push(format!("{}: missing anchor {destination}", path.display()));
                }
            }
        }
    }
    Ok(errors)
}

fn example_errors(source: &str) -> Vec<String> {
    let mut examples = Vec::new();
    let mut active: Option<String> = None;
    for line in source.lines() {
        if line.trim() == "```json rule-file" {
            active = Some(String::new());
        } else if line.trim() == "```" {
            if let Some(example) = active.take() {
                examples.push(example);
            }
        } else if let Some(example) = active.as_mut() {
            example.push_str(line);
            example.push('\n');
        }
    }
    let mut errors = Vec::new();
    if active.is_some() {
        errors.push("unterminated complete rule-file example".to_owned());
    }
    if examples.is_empty() {
        errors.push("language guide needs a complete rule-file example".to_owned());
    }
    for (index, example) in examples.iter().enumerate() {
        match serde_json::from_str::<rules::source::RuleFile>(example) {
            Ok(file) => {
                for diagnostic in
                    rules::compile::check(&[(format!("documentation-example-{index}.json"), file)])
                {
                    if diagnostic.severity == rules::source::Severity::Error {
                        errors.push(format!("invalid rule example {index}: {diagnostic:?}"));
                    }
                }
            }
            Err(error) => errors.push(format!("invalid rule example {index}: {error}")),
        }
    }
    errors
}

fn stale_views(root: &Path, views: &[(PathBuf, String)]) -> Vec<String> {
    let mut errors = Vec::new();
    for (path, expected) in views {
        if fs::read_to_string(root.join(path)).ok().as_deref() != Some(expected.as_str()) {
            errors.push(format!("{} is stale; run {REGENERATE}", path.display()));
        }
    }
    errors
}

/// Checks generated projections, registered-code coverage, examples, and local links.
pub fn check(root: &Path) -> Result<Vec<String>, String> {
    let mut errors = stale_views(root, &views(root)?);
    let package = json(&root.join("editors/vscode/package.json"))?;
    let repository = package["repository"]["url"]
        .as_str()
        .ok_or("missing repository URL")?;
    let expected_base = format!("{repository}/blob/main/editors/vscode");
    if package["vsce"]["baseContentUrl"].as_str() != Some(expected_base.as_str()) {
        errors
            .push("Marketplace README needs the component-relative vsce.baseContentUrl".to_owned());
    }
    let diagnostics = read(&root.join(DIAGNOSTICS))?;
    let documented: BTreeSet<_> = headings(&diagnostics).into_iter().collect();
    let mut codes: BTreeSet<String> = DiagnosticCode::ALL
        .iter()
        .map(|code| code.as_str().to_owned())
        .collect();
    let extension = read(&root.join("editors/vscode/src/transparentLoc.ts"))?;
    for rest in extension.split("diagnostic.code").skip(1) {
        let Some(value) = rest.trim_start().strip_prefix('=').map(str::trim_start) else {
            continue;
        };
        let Some(quote @ ('\'' | '"')) = value.chars().next() else {
            continue;
        };
        if let Some((code, _)) = value[1..].split_once(quote) {
            codes.insert(code.to_owned());
        }
    }
    for code in codes {
        if !documented.contains(&code) {
            errors.push(format!("missing diagnostic explanation: {code}"));
        }
    }
    errors.extend(example_errors(&read(
        &root.join("crates/rules/LANGUAGE.md"),
    )?));
    let mut files: Vec<PathBuf> = fs::read_dir(root)
        .map_err(|e| e.to_string())?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file() && path.extension().is_some_and(|extension| extension == "md")
        })
        .collect();
    for directory in [".github", "crates", "editors", "rules", "lab", "fuzz"] {
        markdown_files(&root.join(directory), &mut files)?;
    }
    files.sort();
    for path in files {
        errors.extend(link_errors(&path, &read(&path)?)?);
    }
    Ok(errors)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derived_views_reject_missing_modified_or_unregenerated_outputs() {
        let root = tempfile::tempdir().unwrap();
        let views = vec![(PathBuf::from("view.md"), "generated\n".to_owned())];
        assert_eq!(stale_views(root.path(), &views).len(), 1);
        fs::write(root.path().join("view.md"), "generated\n").unwrap();
        assert!(stale_views(root.path(), &views).is_empty());
        fs::write(root.path().join("view.md"), "hand-edited\n").unwrap();
        assert_eq!(stale_views(root.path(), &views).len(), 1);
    }

    #[test]
    fn manifest_defaults_and_nls_are_projected_without_a_second_setting_list() {
        let mut package = serde_json::json!({"contributes":{"configuration":{"properties":{"paradoxcode.demo":{"type":"boolean","default":false,"description":"%demo%"}}}}});
        let nls = serde_json::json!({"demo":"A demo setting"});
        let before = extension_reference(&package, &nls).unwrap();
        package["contributes"]["configuration"]["properties"]["paradoxcode.demo"]["default"] =
            Value::Bool(true);
        assert_ne!(before, extension_reference(&package, &nls).unwrap());
        assert!(before.contains("A demo setting"));
        assert!(extension_reference(&package, &serde_json::json!({})).is_err());
    }

    #[test]
    fn missing_files_and_anchors_fail_but_fenced_shell_examples_are_not_links() {
        let root = tempfile::tempdir().unwrap();
        let guide = root.path().join("guide.md");
        fs::write(&guide, "# Hello `World`\n\n## Repeated\n\n## Repeated\n").unwrap();
        assert!(link_errors(&guide, "[ok](guide.md#hello-world) [duplicate](guide.md#repeated-1)\n```sh\n[example](missing.md)\n```\n").unwrap().is_empty());
        assert_eq!(
            link_errors(&guide, "[file](missing.md) [anchor](guide.md#missing)")
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn complete_examples_reject_unknown_schema_and_missing_required_cardinality() {
        assert!(!example_errors("```json rule-file\n{\"files\":{\"x\":{\"path\":\"events\",\"root\":\"missing\"}}}\n```\n").is_empty());
        assert!(!example_errors("```json rule-file\n{\"schemas\":{\"x\":{\"fields\":{\"id\":{\"value\":\"scalar\"}}}}}\n```\n").is_empty());
    }

    #[test]
    fn generated_sections_require_unique_ordered_markers_and_preserve_prose() {
        let source = "before\n<!-- generated:x:start -->\nold\n<!-- generated:x:end -->\nafter\n";
        assert_eq!(
            replace_section(source, "x", "new\n").unwrap(),
            "before\n<!-- generated:x:start -->\n\nnew\n<!-- generated:x:end -->\nafter\n"
        );
        assert!(replace_section(&format!("{source}{source}"), "x", "new").is_err());
        assert!(
            replace_section(
                "<!-- generated:x:end --><!-- generated:x:start -->",
                "x",
                "new"
            )
            .is_err()
        );
    }
}
