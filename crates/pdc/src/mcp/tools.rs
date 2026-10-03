//! Read-only agent request shaping and bounded text results.
use crate::client::{self, Client};

use serde_json::{Value, json};

use std::path::{Path, PathBuf};

fn text<'a>(input: &'a Value, key: &str) -> &'a str {
    input[key].as_str().unwrap_or("").trim()
}

fn limit(input: &Value, maximum: u64) -> u64 {
    input["limit"].as_u64().unwrap_or(20).clamp(1, maximum)
}

fn cap(value: String, n: usize) -> String {
    let len = value.chars().count();

    if len <= n {
        value
    } else {
        format!(
            "{}… (+{} more characters)",
            value.chars().take(n).collect::<String>(),
            len - n
        )
    }
}

fn collapse(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn location(value: &str, root: &Path) -> String {
    let path = url::Url::parse(value)
        .ok()
        .and_then(|u| u.to_file_path().ok())
        .unwrap_or_else(|| PathBuf::from(value));

    path.strip_prefix(root)
        .unwrap_or(&path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn localised(path: &str) -> bool {
    let path = path.replace('\\', "/").to_lowercase();

    path.starts_with("localisation/") || path.contains("/localisation/")
}

const LOC_POINTER: &str = "That is a localisation file; localisation keys belong to the localisation zone. Use paradoxcode_loc_get (exact key) or paradoxcode_loc_list (key prefix) instead.";

fn describe(d: &Value) -> String {
    format!(
        "L{}: [{}] ({}) {}",
        d["range"]["start"]["line"].as_u64().unwrap_or(0) + 1,
        d["code"].as_str().unwrap_or("unknown"),
        match d["severity"].as_u64() {
            Some(1) => "error",
            Some(2) => "warning",
            Some(3) => "info",
            Some(4) => "hint",
            _ => "diagnostic",
        },
        collapse(d["message"].as_str().unwrap_or(""))
    )
}

fn diagnostics(results: &Value, key: &str) -> String {
    let mut sections = Vec::new();

    for file in results.as_array().into_iter().flatten() {
        let empty = Vec::new();

        let all = file["diagnostics"].as_array().unwrap_or(&empty);

        let path = file[key].as_str().unwrap_or("<unknown>");

        let mut lines = all.iter().take(30).map(describe).collect::<Vec<_>>();

        if all.len() > 30 {
            lines.push(format!("… (+{} more diagnostics)", all.len() - 30));
        }

        sections.push(if all.is_empty() {
            format!("{path}: clean (0 diagnostics)")
        } else {
            format!("{path}: {} diagnostic(s)\n{}", all.len(), lines.join("\n"))
        });
    }

    cap(sections.join("\n\n"), 8192)
}

fn loc_hit(hit: &Value) -> String {
    format!(
        "{} = \"{}\" ({}) — {}",
        text(hit, "key"),
        collapse(if text(hit, "value").is_empty() {
            "<no preview>"
        } else {
            text(hit, "value")
        }),
        text(hit, "language"),
        text(hit, "file")
    )
}

pub fn call(name: &str, input: &Value, client: &mut Client, root: &Path) -> Result<String, String> {
    match name {
        "paradoxcode_workspace" => {
            let result = client.request("pdc/workspaceSummary", Value::Null)?;

            let mut lines = vec![
                format!(
                    "Game: {} (rules {}…, revision {})",
                    text(&result, "gameId"),
                    text(&result, "ruleHash")
                        .chars()
                        .take(12)
                        .collect::<String>(),
                    result["revision"]
                ),
                format!(
                    "Files: {} script, {} localisation ({} total)",
                    result["fileCounts"]["script"],
                    result["fileCounts"]["localisation"],
                    result["fileCounts"]["total"]
                ),
                format!(
                    "Scan: {} indexed of {} discovered, {} issue(s)",
                    result["scan"]["indexedFiles"],
                    result["scan"]["discoveredFiles"],
                    result["scan"]["issues"]
                ),
            ];

            for entry in result["roots"].as_array().into_iter().flatten() {
                lines.push(format!(
                    "{} root {}{}",
                    text(entry, "kind"),
                    location(text(entry, "path"), root),
                    if entry["writable"] == false {
                        " (read-only)"
                    } else {
                        ""
                    }
                ));
            }

            Ok(cap(lines.join("\n"), 4096))
        }

        "paradoxcode_search" => {
            let query = text(input, "query");

            if query.chars().count() < 2 {
                return Ok("Query too short: pass at least 2 characters of a symbol name.".into());
            }

            let limit = limit(input, 100);

            let result = client.request(
                "pdc/symbolSearch",
                json!({
                "query":query,"limit":limit}
                ),
            )?;

            let symbols = result["symbols"]
                .as_array()
                .ok_or("invalid symbol response")?;

            if symbols.is_empty() {
                return Ok(format!(
                    "No script symbols match \"{query}\" (localisation keys are excluded — use the loc tools for those)."
                ));
            }

            let mut lines = symbols
                .iter()
                .map(|s| {
                    format!(
                        "{} ({}) — {}:{}",
                        text(s, "name"),
                        text(s, "kind"),
                        location(
                            if text(s, "path").is_empty() {
                                text(s, "uri")
                            } else {
                                text(s, "path")
                            },
                            root
                        ),
                        s["line"]
                    )
                })
                .collect::<Vec<_>>();

            if result["truncated"] == true {
                lines.push(format!(
                    "… (truncated at {limit} symbols; refine the query)"
                ));
            }

            Ok(cap(
                format!("Symbols matching \"{query}\":\n{}", lines.join("\n")),
                8192,
            ))
        }

        "paradoxcode_rules" => {
            let context = text(input, "context");

            let key = text(input, "key");

            let scope = text(input, "scope");

            if context.is_empty() && key.is_empty() && scope.is_empty() {
                return Ok("Pass at least one of context, key, or scope (for example context \"trigger\", key \"add_army_tradition\").".into());
            }

            let limit = limit(input, 50);

            let mut params = json!({
            "limit":limit}
            );

            for (key, value) in [("context", context), ("key", key), ("scope", scope)] {
                if !value.is_empty() {
                    params[key] = json!(value);
                }
            }

            let result = client.request("pdc/ruleSearch", params)?;

            let rules = result["rules"].as_array().ok_or("invalid rules response")?;

            if rules.is_empty() {
                return Ok("No semantic rules match. Try a shorter key substring, or a broader context such as trigger or effect.".into());
            }

            let mut lines = rules
                .iter()
                .map(|rule| {
                    format!(
                        "{} [{}] shape={} scopes={}{}{} — {}",
                        text(rule, "key"),
                        text(rule, "context"),
                        text(rule, "shape"),
                        rule["allowedScopes"]
                            .as_array()
                            .map(|a| a
                                .iter()
                                .filter_map(Value::as_str)
                                .collect::<Vec<_>>()
                                .join("|"))
                            .filter(|s| !s.is_empty())
                            .unwrap_or("any".into()),
                        if rule["deprecated"] == true {
                            " (deprecated)"
                        } else {
                            ""
                        },
                        if rule["required"] == true {
                            " (required)"
                        } else {
                            ""
                        },
                        collapse(text(rule, "documentation"))
                    )
                })
                .collect::<Vec<_>>();

            if result["truncated"] == true {
                lines.push(format!(
                    "… (truncated at {limit} rules; narrow the filters)"
                ));
            }

            Ok(cap(
                format!(
                    "Rules matching context={context} key={key} scope={scope}:\n{}",
                    lines.join("\n")
                ),
                6144,
            ))
        }

        "paradoxcode_validate_text" => {
            let files = input["files"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter(|f| f["path"].is_string() && f["text"].is_string())
                        .take(16)
                        .cloned()
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();

            if files.is_empty() {
                return Ok(
                    "No files to validate: pass between 1 and 16 {path, text} entries.".into(),
                );
            }

            let result = client.request(
                "pdc/textDiagnostics",
                json!({
                "files":files}
                ),
            )?;

            Ok(diagnostics(&result, "path"))
        }

        "paradoxcode_diagnostics" => {
            let mut params = json!({
            "parser":"script","offset":input["offset"].as_u64().unwrap_or(0),"limit":input["limit"].as_u64().unwrap_or(16).clamp(1,128)}
            );

            if let Some(files) = input.get("files") {
                let files = files
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .filter(|s| !s.trim().is_empty())
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();

                if files.is_empty() {
                    return Ok("Pass at least one non-empty logical path in files, or omit files to diagnose the whole workspace.".into());
                }

                params["files"] = json!(files);
            }

            let result = client.request("pdc/workspaceDiagnostics", params)?;

            let mut text = diagnostics(&result["items"], "logicalPath");

            if let Some(offset) = result["nextOffset"].as_u64().filter(|n| *n > 0) {
                text.push_str(&format!("\n… (pass offset={offset} for the next page)"));
            }

            Ok(cap(text, 8192))
        }

        "paradoxcode_context" | "paradoxcode_references" => {
            let path = text(input, "path");

            if path.is_empty() {
                return Ok(
                    "Pass the file path (absolute, file: URI, or workspace-relative).".into(),
                );
            }

            if localised(path) {
                return Ok(LOC_POINTER.into());
            }

            let line = input["line"].as_u64().filter(|n| *n > 0);

            let Some(line) = line else {
                return Ok("Lines are 1-based: pass line >= 1.".into());
            };

            let path = url::Url::parse(path)
                .ok()
                .and_then(|u| u.to_file_path().ok())
                .unwrap_or_else(|| {
                    if Path::new(path).is_absolute() {
                        PathBuf::from(path)
                    } else {
                        root.join(path)
                    }
                });

            if !path.is_file() {
                return Ok(format!(
                    "Could not resolve \"{}\" to an existing file.",
                    path.display()
                ));
            }

            if localised(&path.to_string_lossy()) {
                return Ok(LOC_POINTER.into());
            }

            let mut params = json!({
            "textDocument":{
            "uri":client::uri(&path)?}
            ,"position":{
            "line":line-1,"character":input["character"].as_u64().unwrap_or(0)}
            }
            );

            if name.ends_with("references") {
                params["context"] = json!({
                "includeDeclaration":true}
                );

                let result = client.request("textDocument/references", params)?;

                let items = result.as_array().ok_or("invalid references response")?;

                let mut lines = items
                    .iter()
                    .take(100)
                    .map(|r| {
                        format!(
                            "- {}:{}",
                            location(text(r, "uri"), root),
                            r["range"]["start"]["line"].as_u64().unwrap_or(0) + 1
                        )
                    })
                    .collect::<Vec<_>>();

                if items.len() > 100 {
                    lines.push(format!("… (+{} more references)", items.len() - 100));
                }

                Ok(cap(
                    format!("References ({}):\n{}", items.len(), lines.join("\n")),
                    8192,
                ))
            } else {
                let result = client.request("textDocument/hover", params)?;

                let value = text(&result["contents"], "value");

                Ok(if value.is_empty() {
                    format!("No hover information at {}:{line}.", path.display())
                } else {
                    cap(collapse(value), 2048)
                })
            }
        }

        "paradoxcode_symbol_references" => {
            let name = text(input, "name");

            if name.is_empty() {
                return Ok(
                    "Pass the symbol name (for example an event id or a scripted effect name)."
                        .into(),
                );
            }

            let mut params = json!({
            "name":name,"limit":limit(input,100)}
            );

            if !text(input, "kind").is_empty() {
                params["kind"] = json!(text(input, "kind"));
            }

            let result = client.request("pdc/symbolReferences", params)?;

            if result["matched"] != true {
                return Ok(format!(
                    "No unique symbol for \"{name}\": {}\nCandidates:\n{}",
                    text(&result, "reason"),
                    serde_json::to_string_pretty(&result["candidates"])
                        .map_err(|e| e.to_string())?
                ));
            }

            Ok(cap(
                format!(
                    "{} ({}) defined at {}\nReferences ({}):\n{}",
                    text(&result["symbol"], "name"),
                    text(&result["symbol"], "kind"),
                    serde_json::to_string(&result["symbol"]["definition"]).unwrap_or_default(),
                    result["total"],
                    serde_json::to_string_pretty(&result["references"])
                        .map_err(|e| e.to_string())?
                ),
                8192,
            ))
        }

        "paradoxcode_loc_get" | "paradoxcode_loc_search" | "paradoxcode_loc_list" => {
            let field = match name {
                "paradoxcode_loc_get" => "key",
                "paradoxcode_loc_search" => "text",
                _ => "keyPrefix",
            };

            let filter = text(input, field);

            if filter.is_empty() {
                return Ok(format!(
                    "Pass the {field} filter for the localisation zone."
                ));
            }

            let mut params = json!({
            "limit":limit(input,50)}
            );

            match name {
                "paradoxcode_loc_get" => {
                    params["key"] = json!(filter);

                    params["keyMatch"] = json!("exact");

                    params["limit"] = json!(1);
                }

                "paradoxcode_loc_list" => {
                    params["key"] = json!(filter);

                    params["keyMatch"] = json!("prefix");
                }

                _ => {
                    params["text"] = json!(filter);
                }
            }

            let result = client.request("pdc/localisationSearch", params)?;

            let hits = result["hits"]
                .as_array()
                .ok_or("invalid localisation response")?;

            if hits.is_empty() {
                return Ok(if name == "paradoxcode_loc_get" {
                    format!(
                        "No localisation key \"{filter}\" is defined. Use paradoxcode_loc_list with the key's family prefix."
                    )
                } else {
                    format!("No localisation entries match \"{filter}\".")
                });
            }

            let mut lines = hits.iter().map(loc_hit).collect::<Vec<_>>();

            if result["truncated"] == true {
                lines.push("… (truncated; narrow the filter or raise the limit up to 50)".into());
            }

            Ok(cap(
                lines.join("\n"),
                if name == "paradoxcode_loc_get" {
                    2048
                } else {
                    4096
                },
            ))
        }

        _ => Err(format!("Unknown tool: {name}")),
    }
}
