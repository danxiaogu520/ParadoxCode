//! Real-binary LSP contracts using owned, installation-independent fixtures.
use crate::{
    args::Args,
    lsp::{self, Client},
    process, report,
};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

fn ensure(condition: bool, message: &str) -> Result<(), String> {
    if condition {
        Ok(())
    } else {
        Err(format!("E2E contract: {message}"))
    }
}
pub fn execute(arguments: &[String]) -> Result<String, String> {
    let arguments = if arguments.first().is_some_and(|s| s == "test") {
        &arguments[1..]
    } else {
        arguments
    };
    let args = Args::parse(arguments, &["--root", "--repo", "--server"], &[])?;
    if args.help() {
        return Ok("tools lsp test [--server BINARY] [--root REPO]".into());
    }
    run(&args.root()?, args.get("--server"))
}
pub fn run(root: &Path, explicit: Option<&str>) -> Result<String, String> {
    let binary = process::server(root, explicit)?;
    let version = process::capture(process::command(&binary).arg("--version"))?;
    ensure(
        String::from_utf8_lossy(&version.stdout).trim()
            == format!("paradoxcode {}", env!("CARGO_PKG_VERSION")),
        "server version must match the selected checkout",
    )?;
    ensure(
        !String::from_utf8_lossy(&version.stderr).contains("loading compiled"),
        "--version must answer before rules load",
    )?;
    for argument in ["--definitely-unknown", "mcp"] {
        let unknown = process::command(&binary)
            .arg(argument)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| e.to_string())?;
        ensure(
            !unknown.status.success()
                && String::from_utf8_lossy(&unknown.stderr)
                    .contains("unknown paradoxcode argument"),
            "unknown arguments and the removed MCP subcommand must fail before waiting on stdio",
        )?;
    }
    let fixture = tempfile::Builder::new()
        .prefix("pdc-e2e-")
        .tempdir()
        .map_err(|e| e.to_string())?;
    let mut client = Client::spawn(&binary, fixture.path(), Duration::from_secs(60))?;
    let initialize = client.initialize(fixture.path(), json!({"vanillaMode":"disabled"}))?;
    let package = report::json(&root.join("editors/vscode/package.json"))?;
    let legend = initialize["capabilities"]["semanticTokensProvider"]["legend"]["tokenTypes"]
        .as_array()
        .ok_or("missing semantic token legend")?
        .iter()
        .filter_map(Value::as_str)
        .collect::<BTreeSet<_>>();
    for scope in package["contributes"]["semanticTokenScopes"]
        .as_array()
        .filter(|a| !a.is_empty())
        .ok_or("missing semantic token scopes")?
    {
        ensure(
            scope["language"].is_string(),
            "semantic scopes need a language",
        )?;
        for key in scope["scopes"]
            .as_object()
            .ok_or("missing scope map")?
            .keys()
        {
            ensure(
                legend.contains(key.as_str()),
                "extension semantic scope absent from server legend",
            )?;
        }
    }
    let logs = client
        .notifications
        .iter()
        .filter(|n| n["method"] == "window/logMessage")
        .filter_map(|n| n["params"]["message"].as_str())
        .collect::<Vec<_>>();
    ensure(
        logs.iter().any(|s| s.contains("pdc initializing"))
            && logs.iter().any(|s| s.contains("Initialization finished")),
        "initialization must publish startup and completion logs",
    )?;
    let text = "main_tree = {\n\tslot = 1\n\ta1 = { position = 1 icon = mission_alpha }\n\ta2 = { position = 2 required_missions = { a1 } }\n}\nbranch_tree = {\n\tslot = 2\n\tb1 = { position = 1 required_missions = { external_id } }\n}\n";
    let path = fixture.path().join("missions/smoke.txt");
    let uri = lsp::uri(&path)?;
    let preview = client.request(
        "pdc/missionPreview",
        json!({"path":"missions/smoke.txt","text":text,"uri":uri,"version":7}),
    )?;
    for key in ["nodes", "arrows", "groups", "external", "diagnostics"] {
        ensure(preview[key].is_array(), "mission preview arrays missing")?;
    }
    let nodes = preview["nodes"].as_array().unwrap();
    let a1 = nodes.iter().find(|n| n["id"] == "a1").ok_or("a1 missing")?;
    for key in [
        "x",
        "y",
        "sourceRange",
        "hasError",
        "hasWarning",
        "icon",
        "titleKey",
        "required",
    ] {
        ensure(a1.get(key).is_some(), "mission node required field missing")?;
    }
    ensure(
        a1["icon"] == "mission_alpha"
            && a1["titleKey"] == "a1_title"
            && a1["x"].is_number()
            && a1["y"].is_number(),
        "mission node geometry/icon/title contract",
    )?;
    for end in ["start", "end"] {
        for field in ["line", "character"] {
            ensure(
                a1["sourceRange"][end][field].is_number(),
                "mission node must use UTF-16 source ranges",
            )?;
        }
    }
    ensure(
        a1["title"].is_null()
            || (a1["title"]["value"].is_string()
                && (a1["title"]["language"].is_null() || a1["title"]["language"].is_string())),
        "mission title shape",
    )?;
    ensure(
        nodes.iter().any(|n| {
            n["id"] == "a2"
                && n["required"]
                    .as_array()
                    .is_some_and(|a| a.iter().any(|v| v == "a1"))
        }),
        "mission prerequisite edge missing",
    )?;
    ensure(
        nodes
            .iter()
            .any(|n| n["id"] == "b1" && n["hasError"] == true),
        "dangling mission prerequisite must carry an error",
    )?;
    ensure(
        preview["external"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["label"] == "external_id"),
        "missing external prerequisite stub",
    )?;
    let arrows = preview["arrows"].as_array().unwrap();
    ensure(
        arrows.iter().any(|a| a["glyph"] == "end"),
        "missing arrow end marker",
    )?;
    let glyphs = [
        "verticalTile",
        "verticalSkipTier",
        "horizontalSkipSlot",
        "leftOut",
        "leftIn",
        "rightOut",
        "rightIn",
        "end",
    ];
    for arrow in arrows {
        ensure(
            arrow["glyph"].as_str().is_some_and(|g| glyphs.contains(&g)),
            "unknown arrow glyph",
        )?;
        ensure(
            arrow["x"].is_number()
                && arrow["y"].is_number()
                && arrow["texture"].is_string()
                && arrow["tree"].is_i64()
                && arrow["from"].is_i64(),
            "arrow geometry/texture/endpoint contract",
        )?;
    }
    ensure(
        preview.get("textures").is_none() && preview["groups"].as_array().unwrap().len() == 2,
        "preview must retain text-only assets and both groups",
    )?;
    for group in preview["groups"].as_array().unwrap() {
        ensure(
            group["sourceRange"]["start"].is_object() && group["sourceRange"]["end"].is_object(),
            "group source range",
        )?;
    }
    ensure(
        preview["documentUri"] == uri && preview["documentVersion"] == 7,
        "preview URI/version echo",
    )?;
    let document = fixture.path().join("events/smoke.txt");
    client.open(&document, "# note\n@cost = 100\n", 1)?;
    let tokens = client.request(
        "textDocument/semanticTokens/full",
        json!({"textDocument":{"uri":lsp::uri(&document)?}}),
    )?;
    ensure(
        tokens["data"].as_array().is_some_and(|a| !a.is_empty()),
        "semanticTokens/full must return relative token data",
    )?;
    client.notify("textDocument/didChange",json!({"textDocument":{"uri":lsp::uri(&document)?,"version":2},"contentChanges":[{"text":"country_event = { bad_key = yes }\n"}]}))?;
    for method in [
        "textDocument/hover",
        "textDocument/completion",
        "textDocument/definition",
        "textDocument/references",
    ] {
        let mut params = json!({"textDocument":{"uri":lsp::uri(&document)?},"position":{"line":0,"character":18}});
        if method.ends_with("references") {
            params["context"] = json!({"includeDeclaration":true});
        }
        client.request(method, params)?;
    }
    let files = client.request("pdc/workspaceFiles", Value::Null)?;
    ensure(
        files["roots"].is_array() && files["files"].is_array(),
        "workspace files must contain roots and files",
    )?;
    let result = client.request(
        "pdc/textDiagnostics",
        json!({"files":[{"path":"events/check.txt","text":"bad_key = yes\n"}]}),
    )?;
    ensure(
        result[0]["diagnostics"]
            .as_array()
            .is_some_and(|a| !a.is_empty()),
        "text diagnostics must preserve errors",
    )?;
    client.close(&document)?;
    client.shutdown()?;
    Ok("LSP end-to-end contracts passed".into())
}
