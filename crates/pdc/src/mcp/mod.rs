//! Shipped stdio MCP adapter; shares the extension's tool declarations and a real LSP child.
use crate::client::Client;
use serde_json::{Value, json};
use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;
mod tools;
pub const INSTRUCTIONS: &str = include_str!("instructions.txt");
pub fn tool_manifest() -> Vec<Value> {
    let manifest: Value =
        serde_json::from_str(include_str!("../../../../editors/vscode/package.json"))
            .expect("embedded extension manifest");
    manifest["contributes"]["languageModelTools"].as_array().expect("tool declarations").iter().map(|tool|json!({"name":tool["name"],"title":tool["displayName"],"description":tool["modelDescription"],"inputSchema":tool["inputSchema"]})).collect()
}
fn emit(output: &mut impl Write, value: &Value) -> Result<(), String> {
    serde_json::to_writer(&mut *output, value).map_err(|e| e.to_string())?;
    output
        .write_all(b"\n")
        .and_then(|_| output.flush())
        .map_err(|e| e.to_string())
}
fn root(explicit: Option<&Path>, roots: &Value) -> Result<Option<PathBuf>, String> {
    if let Some(path) = explicit {
        return path
            .canonicalize()
            .map(Some)
            .map_err(|e| format!("--workspace {}: {e}", path.display()));
    }
    for value in roots["roots"].as_array().into_iter().flatten() {
        if let Some(path) = value["uri"]
            .as_str()
            .and_then(|s| url::Url::parse(s).ok())
            .and_then(|u| u.to_file_path().ok())
            && path.is_dir()
        {
            return path.canonicalize().map(Some).map_err(|e| e.to_string());
        }
    }
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)
        .map(|path| path.canonicalize().unwrap_or(path));
    Ok((Some(&cwd) != home.as_ref()).then_some(cwd))
}
pub fn run(arguments: &[String]) -> Result<(), String> {
    let mut binary = std::env::var_os("PDC_MCP_SERVER").map(PathBuf::from);
    let mut explicit = std::env::var_os("PDC_MCP_WORKSPACE").map(PathBuf::from);
    let mut timeout = std::env::var("PDC_MCP_TIMEOUT_MS")
        .unwrap_or_else(|_| "120000".into())
        .parse::<u64>()
        .map_err(|e| e.to_string())?;
    let mut vanilla_mode = None;
    let mut i = 0;
    while i < arguments.len() {
        match arguments[i].as_str() {
            "--help" | "-h" => {
                eprintln!(
                    "paradoxcode mcp [--server PATH] [--workspace DIR] [--timeout-ms N] [--vanilla-mode auto|cacheOnly|disabled]\nRun from an MCP client; stdout is JSON-RPC traffic. Workspace: explicit flag, client roots, then current directory (excluding home)."
                );
                return Ok(());
            }
            key @ ("--server" | "--workspace" | "--timeout-ms" | "--vanilla-mode") => {
                i += 1;
                let value = arguments
                    .get(i)
                    .ok_or_else(|| format!("missing value for {key}"))?;
                match key {
                    "--server" => binary = Some(PathBuf::from(value)),
                    "--workspace" => explicit = Some(PathBuf::from(value)),
                    "--vanilla-mode" => vanilla_mode = Some(value.clone()),
                    _ => timeout = value.parse::<u64>().map_err(|e| e.to_string())?,
                }
            }
            other => return Err(format!("unknown MCP argument: {other}")),
        }
        i += 1;
    }
    if timeout == 0 {
        return Err("timeout must be positive".into());
    }
    if explicit.as_ref().is_some_and(|p| !p.is_dir()) {
        return Err("--workspace must be an existing directory".into());
    }
    let binary = binary.unwrap_or(std::env::current_exe().map_err(|e| e.to_string())?);
    if !binary.is_file() {
        return Err("--server executable does not exist".into());
    }
    let mut roots = json!({"roots":[]});
    let mut session: Option<(PathBuf, Client)> = None;
    let mut initialized = false;
    let mut client_roots = false;
    let mut roots_request = 0_u64;
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    loop {
        let mut line = String::new();
        let count = input
            .by_ref()
            .take(16 * 1024 * 1024 + 1)
            .read_line(&mut line)
            .map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        if count > 16 * 1024 * 1024 {
            return Err("MCP message limit exceeded".into());
        }
        let value: Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(error) => {
                emit(
                    &mut output,
                    &json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":error.to_string()}}),
                )?;
                continue;
            }
        };
        if value.get("method").is_none() {
            if value["id"]
                .as_str()
                .is_some_and(|id| id.starts_with("roots-"))
                && value.get("result").is_some()
            {
                roots = value["result"].clone();
                let selected = root(explicit.as_deref(), &roots)?;
                if session
                    .as_ref()
                    .is_some_and(|(path, _)| Some(path) != selected.as_ref())
                {
                    session.take();
                }
            }
            continue;
        }
        let method = value["method"].as_str().unwrap_or("");
        let id = value.get("id").cloned();
        let params = &value["params"];
        if id.is_none() {
            match method {
                "notifications/initialized" => {
                    initialized = true;
                    if client_roots {
                        roots_request += 1;
                        emit(
                            &mut output,
                            &json!({"jsonrpc":"2.0","id":format!("roots-{roots_request}"),"method":"roots/list","params":{}}),
                        )?;
                    }
                }
                "notifications/roots/list_changed" if client_roots => {
                    roots_request += 1;
                    emit(
                        &mut output,
                        &json!({"jsonrpc":"2.0","id":format!("roots-{roots_request}"),"method":"roots/list","params":{}}),
                    )?;
                }
                _ => {}
            }
            continue;
        }
        let id = id.unwrap();
        let result = (|| -> Result<Value, (i64, String)> {
            if method == "initialize" {
                client_roots = params["capabilities"].get("roots").is_some();
                let version = params["protocolVersion"].as_str().unwrap_or("2025-06-18");
                let version = if ["2024-11-05", "2025-03-26", "2025-06-18"].contains(&version) {
                    version
                } else {
                    "2025-06-18"
                };
                return Ok(
                    json!({"protocolVersion":version,"capabilities":{"tools":{}},"serverInfo":{"name":"paradoxcode-mcp","version":engine::LSP_VERSION},"instructions":INSTRUCTIONS}),
                );
            }
            if method == "ping" {
                return Ok(json!({}));
            }
            if !initialized {
                return Err((-32002, "MCP client must initialize first".into()));
            }
            match method {
                "tools/list" => Ok(json!({"tools":tool_manifest()})),
                "tools/call" => {
                    let name = params["name"]
                        .as_str()
                        .ok_or((-32602, "tool name is required".into()))?;
                    if !tool_manifest().iter().any(|tool| tool["name"] == name) {
                        return Err((-32602, format!("Unknown tool: {name}")));
                    }
                    let selected = root(explicit.as_deref(), &roots).map_err(|e| (-32602, e))?;
                    let Some(selected) = selected else {
                        return Ok(
                            json!({"content":[{"type":"text","text":"No workspace root is available. Pass --workspace or provide MCP client roots."}],"isError":true}),
                        );
                    };
                    if session.as_ref().is_none_or(|(path, _)| path != &selected) {
                        let mut child =
                            Client::spawn(&binary, &selected, Duration::from_millis(timeout))
                                .map_err(|e| (-32603, e))?;
                        let mut options = json!({});
                        if let Some(mode) = &vanilla_mode {
                            options["vanillaMode"] = json!(mode);
                        }
                        child
                            .initialize(&selected, options)
                            .map_err(|e| (-32603, e))?;
                        session = Some((selected.clone(), child));
                    }
                    let result = tools::call(
                        name,
                        &params["arguments"],
                        &mut session.as_mut().unwrap().1,
                        &selected,
                    );
                    Ok(match result {
                        Ok(text) => json!({"content":[{"type":"text","text":text}]}),
                        Err(error) => {
                            json!({"content":[{"type":"text","text":format!("Error: {error}")}],"isError":true})
                        }
                    })
                }
                _ => Err((-32601, "method not implemented".into())),
            }
        })();
        let response = match result {
            Ok(result) => json!({"jsonrpc":"2.0","id":id,"result":result}),
            Err((code, message)) => {
                json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
            }
        };
        emit(&mut output, &response)?;
    }
    if let Some((_, mut child)) = session {
        child.shutdown()?;
    }
    Ok(())
}
