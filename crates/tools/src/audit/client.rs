//! Full diagnostics and fixed golden baselines through the real LSP transport.
use crate::{
    args::Args,
    lsp::{self, Client},
    process, report,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub fn golden(root: &Path, name: &str) -> Result<String, String> {
    let path = root.join("crates/ide/src/tests/golden").join(name);
    if Path::new(name)
        .components()
        .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        return Err("invalid golden fixture path".into());
    }
    let text = fs::read_to_string(&path)
        .map_err(|e| e.to_string())?
        .replace("\r\n", "\n");
    let mut lines = text.split('\n');
    if !lines.by_ref().any(|line| line == "// input:") {
        return Err(format!("missing golden input: {name}"));
    }
    Ok(lines
        .take_while(|line| line.starts_with("//   "))
        .map(|line| &line[5..])
        .collect::<Vec<_>>()
        .join("\n"))
}
pub fn byte_offset(text: &str, line: usize, character: usize) -> Result<usize, String> {
    let mut base = 0;
    for (index, part) in text.split('\n').enumerate() {
        if index == line {
            let mut units = 0;
            for (offset, ch) in part.char_indices() {
                if units == character {
                    return Ok(base + offset);
                }
                units += ch.len_utf16();
                if units > character {
                    return Err("position splits a UTF-16 surrogate pair".into());
                }
            }
            if units == character {
                return Ok(base + part.len());
            }
            return Err("position outside line".into());
        }
        base += part.len() + 1;
    }
    Err("position outside document".into())
}
pub fn candidates(response: &Value) -> Result<Value, String> {
    let items = response
        .as_array()
        .or_else(|| response["items"].as_array())
        .ok_or("invalid completion response")?;
    let mut candidates = items
        .iter()
        .map(|item| json!({"label":item["label"],"kind":item["kind"],"detail":item["detail"]}))
        .collect::<Vec<_>>();
    candidates.sort_by(|a, b| {
        a["label"]
            .as_str()
            .cmp(&b["label"].as_str())
            .then_with(|| a["kind"].as_i64().cmp(&b["kind"].as_i64()))
            .then_with(|| a["detail"].as_str().cmp(&b["detail"].as_str()))
    });
    let labels = items
        .iter()
        .map(|item| {
            item["label"]
                .as_str()
                .map(str::to_owned)
                .ok_or("missing completion label")
        })
        .collect::<Result<BTreeSet<_>, _>>()?;
    Ok(
        json!({"labels":labels,"candidates":candidates,"is_incomplete":response["isIncomplete"].as_bool().unwrap_or(false)}),
    )
}
pub fn probes(
    root: &Path,
    workspace: &Path,
    client: &mut Client,
    timeout: Duration,
) -> Result<Vec<Value>, String> {
    let probes: Vec<Value> =
        serde_json::from_str(include_str!("probes.json")).map_err(|e| e.to_string())?;
    let mut result = Vec::new();
    for probe in probes {
        let name = probe["goldenFile"].as_str().ok_or("missing golden file")?;
        let text = golden(root, name)?;
        let path = workspace.join(probe["documentPath"].as_str().ok_or("missing probe path")?);
        let line = probe["line"].as_u64().ok_or("missing probe line")? as usize;
        let character = probe["character"].as_u64().ok_or("missing probe column")? as usize;
        if text.split('\n').nth(line) != probe["expectLine"].as_str() {
            return Err(format!("golden probe {} drifted", probe["id"]));
        }
        client.open(&path, &text, 1)?;
        let request = |position: Value| json!({"textDocument":{"uri":lsp::uri(&path).unwrap()},"position":position});
        let response = client.request_timeout(
            "textDocument/completion",
            request(json!({"line":line,"character":character})),
            timeout,
        )?;
        let mut item = candidates(&response)?;
        item["id"] = probe["id"].clone();
        item["golden_file"] = json!(name);
        item["document_path"] = probe["documentPath"].clone();
        item["reason"] = probe["reason"].clone();
        item["position"] = json!({"line":line,"character":character});
        item["source_sha256"] = json!(report::hash(text.as_bytes()));
        let offset = byte_offset(&text, line, character)?;
        let word = |c: char| c.is_ascii_alphanumeric() || "_-.@$".contains(c);
        let mut begin = offset;
        let mut end = offset;
        while let Some(c) = text[..begin].chars().next_back().filter(|c| word(*c)) {
            begin -= c.len_utf8();
        }
        while let Some(c) = text[end..].chars().next().filter(|c| word(*c)) {
            end += c.len_utf8();
        }
        let line_begin = text[..begin].rfind('\n').map(|i| i + 1).unwrap_or(0);
        let column = text[line_begin..begin].encode_utf16().count();
        let mut samples = Vec::new();
        let mut version = 1;
        for prefix in [
            "a", "c", "g", "p", "r", "s", "t", "_", "add_", "has_", "is_", "any_", "random_",
            "set_", "ROOT", "PREV", "FROM",
        ] {
            let edited = format!("{}{prefix}{}", &text[..begin], &text[end..]);
            version += 1;
            client.notify("textDocument/didChange",json!({"textDocument":{"uri":lsp::uri(&path)?,"version":version},"contentChanges":[{"text":edited}]}))?;
            let position = json!({"line":line,"character":column+prefix.len()});
            let response = client.request_timeout(
                "textDocument/completion",
                request(position.clone()),
                timeout,
            )?;
            let mut sample = candidates(&response)?;
            sample["prefix"] = json!(prefix);
            sample["position"] = position;
            samples.push(sample);
        }
        client.close(&path)?;
        item["prefix_samples"] = json!(samples);
        item["prefix_coverage"] =
            json!("representative samples; does not prove complete enumeration");
        result.push(item);
    }
    Ok(result)
}
fn query_samples(
    files: &[std::path::PathBuf],
    source: &Path,
    overlay: &Path,
    client: &mut Client,
    count: usize,
    timeout: Duration,
) -> Result<Value, String> {
    let mut samples = BTreeMap::<String, Vec<f64>>::new();
    let mut missing = Vec::new();
    let step = (files.len() / count).max(1);
    for path in files.iter().step_by(step).take(count) {
        let text = report::text(&fs::read(path).map_err(|e| e.to_string())?).0;
        let logical = path.strip_prefix(source).map_err(|e| e.to_string())?;
        let path = overlay.join(logical);
        let uri = lsp::uri(&path)?;
        let lines = text.split('\n').collect::<Vec<_>>();
        let mut line = lines.len() - 1;
        while line > 0 && lines[line].trim().len() < 4 {
            line -= 1;
        }
        let character = lines[line].encode_utf16().count().min(2);
        let since = client.notifications.len();
        client.open(&path, &text, 1)?;
        let _ = client.wait_notification_since(
            "textDocument/publishDiagnostics",
            since,
            Duration::from_millis(1500),
            |p| p["uri"] == uri,
        );
        for method in [
            "textDocument/completion",
            "textDocument/hover",
            "textDocument/definition",
            "textDocument/references",
            "textDocument/documentSymbol",
        ] {
            let mut params = json!({"textDocument":{"uri":uri}});
            if method != "textDocument/documentSymbol" {
                params["position"] = json!({"line":line,"character":character});
            }
            if method == "textDocument/references" {
                params["context"] = json!({"includeDeclaration":true});
            }
            let start = Instant::now();
            client.request_timeout(method, params, timeout)?;
            samples
                .entry(method.into())
                .or_default()
                .push(start.elapsed().as_secs_f64() * 1000.0);
        }
        let since = client.notifications.len();
        let start = Instant::now();
        client.notify("textDocument/didChange",json!({"textDocument":{"uri":uri,"version":2},"contentChanges":[{"text":format!("{text}\ndemo_sweep_probe_unknown_key = yes")}]}))?;
        if client
            .wait_notification_since(
                "textDocument/publishDiagnostics",
                since,
                Duration::from_millis(1500),
                |p| p["uri"] == uri && p["version"] == 2,
            )
            .is_ok()
        {
            samples
                .entry("incremental_diagnostics".into())
                .or_default()
                .push(start.elapsed().as_secs_f64() * 1000.0);
        } else {
            client.request(
                "textDocument/hover",
                json!({"textDocument":{"uri":uri},"position":{"line":line,"character":character}}),
            )?;
            missing.push(logical.to_string_lossy().into_owned());
        }
        client.close(&path)?;
    }
    let mut result = serde_json::Map::new();
    for (name, mut values) in samples {
        values.sort_by(f64::total_cmp);
        if values.is_empty() {
            continue;
        }
        let percentile = |fraction: f64| {
            values[((values.len() as f64 * fraction).floor() as usize).min(values.len() - 1)]
        };
        result.insert(name,json!({"p50":percentile(0.5),"p99":percentile(0.99),"max":values.last(),"samples":values.len()}));
    }
    result.insert("unobserved_incremental_publications".into(), json!(missing));
    Ok(Value::Object(result))
}
fn increment(object: &mut Value, key: &str) {
    object[key] = json!(object[key].as_u64().unwrap_or(0) + 1);
}
fn file_result(
    path: &Path,
    logical: &str,
    text: &str,
    encoding: &str,
    diagnostics: &Value,
    summary: &mut Value,
) -> Result<Value, String> {
    let mut results = Vec::new();
    for diagnostic in diagnostics.as_array().ok_or("invalid diagnostic array")? {
        let mut d = json!({"code":diagnostic["code"],"severity":diagnostic["severity"],"message":diagnostic["message"].as_str().unwrap_or(""),"source":diagnostic["source"].as_str().unwrap_or("paradoxcode"),"range":diagnostic["range"]});
        let severity = d["severity"].as_u64().unwrap_or(1);
        d["severity"] = json!(severity);
        let (name, key) = match severity {
            1 => ("error", "errors"),
            2 => ("warning", "warnings"),
            3 => ("info", "infos"),
            4 => ("hint", "hints"),
            _ => ("unknown", "unknown"),
        };
        d["severity_name"] = json!(name);
        let start = &d["range"]["start"];
        let end = &d["range"]["end"];
        let line = start["line"].as_u64().ok_or("invalid diagnostic range")?;
        let character = start["character"]
            .as_u64()
            .ok_or("invalid diagnostic column")?;
        let location = json!({"line":line+1,"column":character+1,"end_line":end["line"].as_u64().ok_or("invalid diagnostic end")?+1,"end_column":end["character"].as_u64().ok_or("invalid diagnostic end column")?+1});
        d["location"] = location;
        d["excerpt"] = json!(text.lines().nth(line as usize).unwrap_or(""));
        let code = match &d["code"] {
            Value::String(s) => s.clone(),
            Value::Null => "unknown".into(),
            v => v.to_string(),
        };
        d["code"] = json!(code);
        increment(summary, "total_diagnostics");
        increment(summary, key);
        if summary["by_code"].get(&code).is_none() {
            summary["by_code"][&code] =
                json!({"count":0,"errors":0,"warnings":0,"infos":0,"hints":0});
        }
        increment(&mut summary["by_code"][&code], "count");
        increment(&mut summary["by_code"][&code], key);
        results.push(d);
    }
    increment(summary, "files_analyzed");
    if !results.is_empty() {
        increment(summary, "files_with_diagnostics");
    }
    Ok(json!({"path":logical,"physical_path":path,"encoding":encoding,"diagnostics":results}))
}
pub fn execute(mode: &str, arguments: &[String]) -> Result<String, String> {
    let mut args = Args::parse(
        arguments,
        &[
            "--root",
            "--repo",
            "--server",
            "--mod",
            "--workspace",
            "--vanilla-source",
            "--vanilla-cache",
            "--cache",
            "--output",
            "--timeout-ms",
            "--file-timeout-ms",
            "--batch-size",
            "--concurrency",
            "--checkpoint-every",
            "--max-files",
            "--path-prefix",
            "--shard-count",
            "--shard-index",
            "--fail-on",
            "--label",
            "--query-samples",
            "--memory-interval-ms",
            "--previous",
            "--dependency",
        ],
        &["--completions-only"],
    )?;
    args.environment(&[
        ("--server", "PDC_DIAGNOSTIC_SERVER"),
        ("--vanilla-source", "PDC_SWEEP_VANILLA_SOURCE"),
        ("--vanilla-cache", "PDC_DIAGNOSTIC_VANILLA_CACHE"),
        ("--output", "PDC_DIAGNOSTIC_OUTPUT"),
    ]);
    if args.help() {
        return Ok(super::HELP.into());
    }
    let root = args.root()?;
    let source = args
        .path("--vanilla-source")
        .or_else(|| args.path("--mod"))
        .ok_or("select --mod or --vanilla-source")?
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let default = root
        .join("target")
        .join(if mode == "diagnose" {
            "diagnostic-reports"
        } else {
            "performance-results"
        })
        .join(mode);
    let output = report::output(
        &root,
        &args.path("--output").unwrap_or(default),
        if mode == "diagnose" {
            "diagnostic-reports"
        } else {
            "performance-results"
        },
    )?;
    fs::create_dir_all(&output).map_err(|e| e.to_string())?;
    let overlay = tempfile::Builder::new()
        .prefix(".overlays-")
        .tempdir_in(&output)
        .map_err(|e| e.to_string())?;
    let is_vanilla = args.get("--vanilla-source").is_some();
    let workspace = args
        .path("--workspace")
        .unwrap_or_else(|| {
            if is_vanilla {
                overlay.path().to_owned()
            } else {
                source.clone()
            }
        })
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let cache = args
        .path("--vanilla-cache")
        .or_else(|| args.path("--cache"));
    if is_vanilla && cache.is_none() {
        return Err("--vanilla-source requires --vanilla-cache".into());
    }
    let timeout = Duration::from_millis(args.number("--timeout-ms", 600_000)? as u64);
    let file_timeout = Duration::from_millis(args.number("--file-timeout-ms", 30_000)? as u64);
    let batch = args.number("--batch-size", 16)?.min(16);
    let checkpoint = args.number("--checkpoint-every", 100)?;
    let concurrency = args.number("--concurrency", 8)?;
    let max_files = args.number("--max-files", 100_000)?;
    let shard_count = args.number("--shard-count", 1)?;
    let shard_index = args
        .get("--shard-index")
        .unwrap_or("0")
        .parse::<usize>()
        .map_err(|_| "invalid shard index")?;
    if shard_index >= shard_count {
        return Err("shard index must be below shard count".into());
    }
    let fail_on =
        args.get("--fail-on")
            .unwrap_or(if mode == "diagnose" { "error" } else { "none" });
    if !["none", "error", "warning"].contains(&fail_on) {
        return Err("--fail-on must be none, error or warning".into());
    }
    let binary = process::server(&root, args.get("--server"))?;
    let started = Instant::now();
    let mut input = json!({"schema_version":1,"generated_at":SystemTime::now().duration_since(UNIX_EPOCH).map_err(|e|e.to_string())?.as_secs(),"status":"running","inputs":{"game":"eu4","rules":{},"mode":if is_vanilla{"vanilla-source"}else{"project"},"source":source,"project":if is_vanilla{Value::Null}else{json!(source)},"workspace":workspace,"vanilla_cache":{"path":cache,"loaded":false},"server":binary,"fail_on":fail_on,"shard":{"index":shard_index,"count":shard_count}},"scan":{},"files":[],"summary":{"files_analyzed":0,"files_with_diagnostics":0,"total_diagnostics":0,"errors":0,"warnings":0,"infos":0,"hints":0,"by_code":{}},"tool_errors":[],"tool_errors_omitted":0,"server_messages":[]});
    let mut client = Client::spawn(&binary, &workspace, timeout)?;
    let mut sampler = if mode == "sweep" {
        Some(crate::perf::sampler::Sampler::start(
            client.pid(),
            Duration::from_millis(args.number("--memory-interval-ms", 250)? as u64),
        ))
    } else {
        None
    };
    let boot = Instant::now();
    let mut completions = Vec::new();
    let mut workspace_summary = Value::Null;
    let mut phases = json!({});
    let result = (|| -> Result<(), String> {
        let mut options = json!({"modDirectory":if is_vanilla{overlay.path()}else{&source}});
        if let Some(cache) = &cache {
            options["vanillaIndexCache"] =
                json!(std::path::absolute(cache).map_err(|e| e.to_string())?);
        }
        if let Some(dependencies) = args.values.get("--dependency") {
            options["dependencies"] = json!(
                dependencies
                    .iter()
                    .map(|s| s
                        .split_once('=')
                        .map(|(id, path)| json!({"id":id,"path":path}))
                        .ok_or("--dependency must be ID=PATH"))
                    .collect::<Result<Vec<_>, _>>()?
            );
        }
        let initialized = client.initialize(&workspace, options)?;
        phases["session_boot_ms"] = json!(boot.elapsed().as_secs_f64() * 1000.0);
        input["inputs"]["server_identity"] = json!({"version":initialized["serverInfo"]["version"],"binary_sha256":report::file_hash(&binary)?});
        workspace_summary = client.request("pdc/workspaceSummary", Value::Null)?;
        let hash = workspace_summary["ruleHash"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or("server did not return embedded rule identity")?;
        input["inputs"]["rules"] = json!({"authority":"embedded first-party rules","external_source":false,"rule_hash":hash});
        match client.request("pdc/analyzerInfo", Value::Null) {
            Ok(identity) => {
                if identity["ruleHash"] != hash
                    || identity["version"] != initialized["serverInfo"]["version"]
                {
                    return Err("server analyzer identity is inconsistent".into());
                }
                for (field, key) in [
                    ("source_format_version", "sourceFormatVersion"),
                    ("target_game_version", "targetGameVersion"),
                    ("schema_count", "schemaCount"),
                    ("field_count", "fieldCount"),
                    ("matcher_count", "matcherCount"),
                    ("file_category_count", "fileCategoryCount"),
                ] {
                    input["inputs"]["rules"][field] = identity[key].clone();
                }
            }
            Err(error) if error.contains("-32601") => {}
            Err(error) => return Err(error),
        }
        if is_vanilla
            && (!workspace_summary["roots"].as_array().is_some_and(|roots| {
                roots.iter().any(|r| {
                    r["kind"] == "vanilla"
                        && r["path"].as_str().is_some_and(|p| Path::new(p) == source)
                })
            }) || workspace_summary["fileCounts"]["total"] == 0)
        {
            return Err(
                "selected Vanilla source was not indexed; build the cache before auditing".into(),
            );
        }
        input["inputs"]["vanilla_cache"]["loaded"] = json!(cache.is_some());
        let scan = Instant::now();
        let mut files = report::files(&source)?
            .into_iter()
            .filter(|p| {
                p.extension().and_then(|e| e.to_str()).is_some_and(|e| {
                    ["txt", "gfx", "yml", "yaml"].contains(&e.to_ascii_lowercase().as_str())
                })
            })
            .collect::<Vec<_>>();
        if files.len() > max_files {
            return Err("source exceeds --max-files".into());
        }
        input["scan"]["relevant_files_discovered"] = json!(files.len());
        phases["scan_ms"] = json!(scan.elapsed().as_secs_f64() * 1000.0);
        if let Some(prefix) = args.get("--path-prefix") {
            if prefix.split('/').any(|p| p == "..") {
                return Err("invalid path prefix".into());
            }
            files.retain(|p| p.strip_prefix(&source).is_ok_and(|p| p.starts_with(prefix)));
        }
        files = files
            .into_iter()
            .enumerate()
            .filter(|(i, _)| i % shard_count == shard_index)
            .map(|(_, p)| p)
            .collect();
        let classify = Instant::now();
        let mut accepted = BTreeSet::new();
        for paths in files.chunks(4096) {
            let paths = paths
                .iter()
                .map(|p| {
                    p.strip_prefix(&source)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/")
                })
                .collect::<Vec<_>>();
            let response = client.request_timeout(
                "pdc/classifyPaths",
                json!({"paths":paths}),
                file_timeout,
            )?;
            for path in response.as_array().ok_or("invalid path classification")? {
                accepted.insert(path.as_str().ok_or("invalid classified path")?.to_owned());
            }
        }
        files.retain(|p| {
            accepted.contains(
                &p.strip_prefix(&source)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/"),
            )
        });
        input["scan"]["diagnosable_files_selected"] = json!(files.len());
        phases["classify_ms"] = json!(classify.elapsed().as_secs_f64() * 1000.0);
        let diagnose = Instant::now();
        if !args.flag("--completions-only") {
            for wave in files.chunks(batch * concurrency) {
                let mut sources = BTreeMap::new();
                let mut requests = Vec::new();
                for paths in wave.chunks(batch) {
                    let mut entries = Vec::new();
                    for path in paths {
                        let bytes = fs::read(path).map_err(|e| e.to_string())?;
                        if bytes.len() > 16 * 1024 * 1024 {
                            return Err(format!("{} exceeds source-file limit", path.display()));
                        }
                        let (text, encoding) = report::text(&bytes);
                        let logical = path
                            .strip_prefix(&source)
                            .unwrap()
                            .to_string_lossy()
                            .replace('\\', "/");
                        entries.push(json!({"path":logical,"text":text}));
                        sources.insert(logical, (path.clone(), text, encoding));
                    }
                    requests.push(("pdc/textDiagnostics", json!({"files":entries})));
                }
                let responses = client.request_many(&requests, file_timeout * batch as u32)?;
                let mut seen = BTreeSet::new();
                for response in responses {
                    for item in response.as_array().ok_or("invalid diagnostics batch")? {
                        let logical = item["path"].as_str().ok_or("missing result path")?;
                        if !seen.insert(logical.to_owned()) {
                            return Err("duplicate diagnostic result".into());
                        }
                        let (path, text, encoding) =
                            sources.get(logical).ok_or("unexpected diagnostic result")?;
                        let file = file_result(
                            path,
                            logical,
                            text,
                            encoding,
                            &item["diagnostics"],
                            &mut input["summary"],
                        )?;
                        input["files"].as_array_mut().unwrap().push(file);
                    }
                }
                if seen.len() != sources.len() {
                    return Err("incomplete diagnostics batch".into());
                }
                if input["files"].as_array().unwrap().len() % checkpoint < wave.len() {
                    report::write(&output.join("project-checkpoint.json"), &input)?;
                }
            }
        }
        phases["diagnose_ms"] = json!(diagnose.elapsed().as_secs_f64() * 1000.0);
        workspace_summary = client.request("pdc/workspaceSummary", Value::Null)?;
        if mode == "baseline" {
            completions = probes(&root, overlay.path(), &mut client, file_timeout)?;
        }
        if mode == "sweep" {
            let query = Instant::now();
            let count = args.number("--query-samples", 32)?;
            input["query_latencies"] = query_samples(
                &files,
                &source,
                overlay.path(),
                &mut client,
                count,
                file_timeout,
            )?;
            phases["query_samples_ms"] = json!(query.elapsed().as_secs_f64() * 1000.0);
        }
        client.shutdown()?;
        Ok(())
    })();
    if let Err(error) = result {
        input["tool_errors"]
            .as_array_mut()
            .unwrap()
            .push(json!(error));
    }
    phases["total_ms"] = json!(started.elapsed().as_secs_f64() * 1000.0);
    input["phases"] = phases;
    input["server_stderr"] = json!(client.stderr());
    if let Some(sampler) = &mut sampler {
        let samples = sampler.finish();
        input["resources"] = json!({"cpu_seconds":samples["last"]["cpu_seconds"],"peak_working_set_bytes":samples["peak_working_set_bytes"],"samples":samples["samples"],"sample_interval_ms":args.number("--memory-interval-ms",250)?,"sample_errors":samples["sample_errors"],"method":samples["method"]});
    }
    input["server_messages"] = json!(
        client
            .notifications
            .iter()
            .filter_map(|n| n["params"]["message"].as_str())
            .collect::<Vec<_>>()
    );
    input["files"]
        .as_array_mut()
        .unwrap()
        .sort_by_key(|file| file["path"].as_str().unwrap_or("").to_owned());
    let failed = !input["tool_errors"].as_array().unwrap().is_empty();
    let findings = fail_on != "none"
        && (input["summary"]["errors"].as_u64().unwrap_or(0) > 0
            || fail_on == "warning" && input["summary"]["warnings"].as_u64().unwrap_or(0) > 0);
    input["status"] = json!(if failed {
        "incomplete"
    } else if findings {
        "failed"
    } else {
        "passed"
    });
    report::write(&output.join("project-report.json"), &input)?;
    let markdown = format!(
        "# Project diagnostic report\n\nStatus: {}\n\n```json\n{}\n```\n",
        input["status"],
        serde_json::to_string_pretty(&input["summary"]).map_err(|e| e.to_string())?
    );
    fs::write(output.join("project-report.md"), markdown).map_err(|e| e.to_string())?;
    if mode == "baseline" {
        for file in input["files"].as_array_mut().unwrap() {
            file["diagnostics"].as_array_mut().unwrap().sort_by(|a, b| {
                for (end, field) in [
                    ("start", "line"),
                    ("start", "character"),
                    ("end", "line"),
                    ("end", "character"),
                ] {
                    let order = a["range"][end][field]
                        .as_u64()
                        .cmp(&b["range"][end][field].as_u64());
                    if !order.is_eq() {
                        return order;
                    }
                }
                a["code"]
                    .as_str()
                    .cmp(&b["code"].as_str())
                    .then_with(|| a["message"].as_str().cmp(&b["message"].as_str()))
            });
        }
        completions.sort_by(|a, b| {
            a["document_path"]
                .as_str()
                .cmp(&b["document_path"].as_str())
                .then_with(|| {
                    a["position"]["line"]
                        .as_u64()
                        .cmp(&b["position"]["line"].as_u64())
                })
                .then_with(|| {
                    a["position"]["character"]
                        .as_u64()
                        .cmp(&b["position"]["character"].as_u64())
                })
                .then_with(|| a["id"].as_str().cmp(&b["id"].as_str()))
        });
        let diagnostics=input["files"].as_array().unwrap().iter().map(|f|json!({"path":f["path"],"diagnostics":f["diagnostics"].as_array().unwrap().iter().map(|d|json!({"code":d["code"],"range":d["range"],"message":d["message"]})).collect::<Vec<_>>()})).collect::<Vec<_>>();
        let hash = input["inputs"]["rules"]["rule_hash"].clone();
        let baseline = json!({"schema_version":1,"kind":if args.flag("--completions-only"){"paradoxcode-completion-baseline"}else{"paradoxcode-rules-baseline"},"metadata":{"status":input["status"],"tool_errors":input["tool_errors"],"label":args.get("--label"),"server":input["inputs"]["server_identity"],"rules":{"rule_hash":hash,"server_rule_hash":hash,"workspace_summary_rule_hash":workspace_summary["ruleHash"]},"totals":{"files":input["summary"]["files_analyzed"],"errors":input["summary"]["errors"],"diagnostics":input["summary"]["total_diagnostics"]},"coverage":{"diagnostics":!args.flag("--completions-only"),"symbols":true,"completions":"fixed golden positions and representative prefixes"}},"diagnostics":diagnostics,"symbols":workspace_summary["symbols"],"completions":completions});
        report::write(&output.join("baseline-latest.json"), &baseline)?;
    }
    if mode == "sweep" {
        let mut lines = Vec::new();
        for file in input["files"].as_array().unwrap() {
            for d in file["diagnostics"].as_array().unwrap() {
                lines.push(format!(
                    "{}|{}|{}|{}:{}|{}",
                    file["path"].as_str().unwrap(),
                    d["severity_name"].as_str().unwrap(),
                    d["code"].as_str().unwrap(),
                    d["location"]["line"],
                    d["location"]["column"],
                    d["message"].as_str().unwrap()
                ));
            }
        }
        lines.sort();
        let mut summary = json!({"schema_version":2,"status":input["status"],"phases":input["phases"],"summary":input["summary"],"diagnostics_fingerprint":report::hash(lines.join("\n").as_bytes()),"inputs":input["inputs"],"query_latencies":input["query_latencies"],"resources":input["resources"],"full_report":output.join("project-report.json")});
        if let Some(previous) = args.path("--previous") {
            let previous = report::json(&previous)?;
            summary["comparison"] = json!({"previous_fingerprint":previous["diagnostics_fingerprint"],"drifted":previous["diagnostics_fingerprint"]!=summary["diagnostics_fingerprint"]});
        }
        report::write(&output.join("sweep-summary.json"), &summary)?;
    }
    if failed || findings {
        return Err(format!(
            "{mode} {}: {}",
            input["status"],
            output.join("project-report.json").display()
        ));
    }
    Ok(format!(
        "{mode}: {} files, {} diagnostics; {}",
        input["summary"]["files_analyzed"],
        input["summary"]["total_diagnostics"],
        output.display()
    ))
}
