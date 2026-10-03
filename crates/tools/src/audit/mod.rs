//! Native semantic review, preserving frozen evidence and incomplete-result rejection.
use crate::{args::Args, report};
use serde_json::{Value, json};
use std::path::Path;
pub mod client;
pub mod compare;
pub mod completions;
pub mod resources;
pub mod templates;

pub const HELP: &str = "tools audit errors --report PATH (--installation DIR | --prior-review DIR --prior-cache PATH --cache PATH) --output DIR [--decisions PATH]\ntools audit diff --before DIR --before-cache PATH --after DIR --after-cache PATH --output PATH\ntools audit completions --baseline PATH --cache PATH --output DIR\ntools audit templates --output DIR [--check-only] [--server PATH] [--samples N]\ntools audit diagnose|sweep|baseline --server PATH (--mod DIR | --vanilla-source DIR) --vanilla-cache PATH --output DIR";
pub fn execute(arguments: &[String]) -> Result<String, String> {
    let Some((command, arguments)) = arguments.split_first() else {
        return Ok(HELP.into());
    };
    if matches!(command.as_str(), "help" | "--help" | "-h") {
        return Ok(HELP.into());
    }
    if matches!(command.as_str(), "diagnose" | "sweep" | "baseline") {
        return client::execute(command, arguments);
    }
    if command == "completions" {
        return completions::execute(arguments);
    }
    if command == "templates" {
        return templates::execute(arguments);
    }
    let args = Args::parse(
        arguments,
        &[
            "--root",
            "--repo",
            "--report",
            "--installation",
            "--prior-review",
            "--prior-cache",
            "--cache",
            "--output",
            "--decisions",
            "--before",
            "--before-cache",
            "--after",
            "--after-cache",
        ],
        &[],
    )?;
    if args.help() {
        return Ok(HELP.into());
    }
    let root = args.root()?;
    let output = report::output(
        &root,
        Path::new(args.required("--output")?),
        "performance-results",
    )?;
    match command.as_str() {
        "errors" => {
            let report_path = Path::new(args.required("--report")?);
            let input = report::json(report_path)?;
            let (errors, summary) = resources::review(report_path, &input, &args)?;
            report::write(&output.join("errors.json"), &Value::Array(errors))?;
            report::write(&output.join("summary.json"), &summary)?;
            Ok(format!(
                "{} errors inventoried; {} pending ({})",
                summary["total_errors"], summary["pending_errors"], summary["status"]
            ))
        }
        "diff" | "compare" => {
            let before = compare::load(
                Path::new(args.required("--before")?),
                Path::new(args.required("--before-cache")?),
            )?;
            let after = compare::load(
                Path::new(args.required("--after")?),
                Path::new(args.required("--after-cache")?),
            )?;
            let result = compare::compare(&before, &after)?;
            report::write(&output, &result)?;
            Ok(format!(
                "Exported comparison of {} files; semantic review remains required",
                result["files_diagnosed"]
            ))
        }
        _ => Err(format!("unknown audit command: {command}\n{HELP}")),
    }
}
pub fn rule_hash(input: &Value) -> &str {
    input
        .pointer("/inputs/rules/rule_hash")
        .or_else(|| input.pointer("/inputs/rules/manifest_rule_hash"))
        .and_then(Value::as_str)
        .unwrap_or("")
}
/// Reads the rule identity frozen inside a report, accepting historical field names.
pub fn rules_identity(input: &Value) -> Result<Value, String> {
    let hash = rule_hash(input);
    if hash.is_empty() {
        return Err("report is missing embedded rule identity".into());
    }
    let mut rules = input["inputs"]["rules"]
        .as_object()
        .ok_or("missing report rules")?
        .clone();
    for key in ["rule_hash", "manifest_rule_hash"] {
        if rules.get(key).is_some_and(|value| value != hash) {
            return Err("report rule identities differ".into());
        }
    }
    rules.remove("manifest_rule_hash");
    rules.insert("rule_hash".into(), json!(hash));
    if let Some(version) = input["inputs"]["server_identity"].get("version") {
        rules.insert("lsp_version".into(), version.clone());
    }
    Ok(Value::Object(rules))
}
/// The previous Python inventory used sorted JSON with spaces. Preserve decision IDs exactly.
pub fn identity_json(value: &Value) -> String {
    match value {
        Value::Array(items) => format!(
            "[{}]",
            items
                .iter()
                .map(identity_json)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::Object(items) => format!(
            "{{{}}}",
            items
                .iter()
                .map(|(key, value)| format!("{}: {}", json!(key), identity_json(value)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        _ => value.to_string(),
    }
}
