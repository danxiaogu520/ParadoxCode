//! Replays real documents and overlapping edits through the production stdio server.
use crate::{
    args::Args,
    lsp::{self, Client},
    process, report,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    fs,
    path::Path,
    time::{Duration, Instant},
};

const HELP: &str = "tools perf completion --workspace DIR --trace JSON --output JSON [--server BINARY] [--cache PATH] [--game-directory DIR] [--samples N] [--interval-ms N] [--reindex]";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Trace {
    cases: Vec<Case>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    path: String,
    needle: String,
    #[serde(default)]
    occurrence: usize,
}

pub fn execute(arguments: &[String]) -> Result<String, String> {
    let args = Args::parse(
        arguments,
        &[
            "--root",
            "--repo",
            "--workspace",
            "--trace",
            "--output",
            "--server",
            "--cache",
            "--game-directory",
            "--samples",
            "--interval-ms",
            "--timeout-ms",
            "--publication-timeout-ms",
            "--dependency",
        ],
        &["--reindex", "--full-text"],
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
    report::write(&output, &json!({"status":"running"}))?;
    let result = run(&args, &root, &output);
    if let Err(error) = &result {
        let mut failed = report::json(&output).unwrap_or_else(|_| json!({}));
        failed["status"] = json!("failed");
        failed["error"] = json!(error);
        report::write(&output, &failed)?;
    }
    result
}

fn run(args: &Args, root: &Path, output: &Path) -> Result<String, String> {
    let workspace = fs::canonicalize(args.required("--workspace")?).map_err(|e| e.to_string())?;
    let trace: Trace = serde_json::from_value(report::json(Path::new(args.required("--trace")?))?)
        .map_err(|e| e.to_string())?;
    if trace.cases.is_empty() {
        return Err("completion trace needs at least one case".into());
    }
    let binary = process::server(root, args.get("--server"))?;
    let timeout = Duration::from_millis(args.number("--timeout-ms", 120_000)? as u64);
    let publication_timeout =
        Duration::from_millis(args.number("--publication-timeout-ms", 2_000)? as u64);
    let samples = args.number("--samples", 30)?;
    let interval = Duration::from_millis(args.number("--interval-ms", 25)? as u64);
    let mut options =
        json!({"modDirectory":workspace,"workspaceWideDiagnostics":false,"vanillaMode":"disabled"});
    if let Some(path) = args.path("--cache") {
        options["vanillaIndexCache"] = json!(std::path::absolute(path).map_err(|e| e.to_string())?);
        options["vanillaMode"] = json!("auto");
    }
    if let Some(path) = args.path("--game-directory") {
        options["gameDirectory"] = json!(fs::canonicalize(path).map_err(|e| e.to_string())?);
        options["vanillaMode"] = json!("auto");
    }
    if let Some(dependencies) = args.values.get("--dependency") {
        options["dependencies"] =
            json!(dependencies.iter().map(|value| {
            let (id, path) = value.split_once('=').ok_or("--dependency needs ID=PATH")?;
            Ok(json!({"id":id,"path":fs::canonicalize(path).map_err(|e| e.to_string())?}))
        }).collect::<Result<Vec<Value>,String>>()?);
    }
    let mut client = Client::spawn(&binary, &workspace, timeout)?;
    let started = Instant::now();
    let initialized = client
        .handshake(&workspace, options.clone())
        .map_err(|error| format!("initialize: {error}"))?;
    eprintln!(
        "completion replay: initialize response in {:.1} ms",
        started.elapsed().as_secs_f64() * 1000.0
    );
    client
        .wait_notification("pdc/ready", |_| true)
        .map_err(|error| {
            format!(
                "workspace readiness: {error}; last notifications: {:?}",
                client
                    .notifications
                    .iter()
                    .rev()
                    .take(8)
                    .collect::<Vec<_>>()
            )
        })?;
    eprintln!("completion replay: workspace ready");
    let initialization_ms = started.elapsed().as_secs_f64() * 1000.0;
    let files = client.request("pdc/workspaceFiles", Value::Null)?;
    let workspace_inventory =
        json!({"roots":files["roots"],"files":files["files"].as_array().map(Vec::len)});
    let mut runs = Vec::new();
    let mut failures = Vec::new();
    for case in trace.cases {
        if case.needle.is_empty() || !case.needle.is_ascii() {
            return Err("trace needle must be a nonempty ASCII token".into());
        }
        let path = fs::canonicalize(workspace.join(&case.path)).map_err(|e| e.to_string())?;
        if !path.starts_with(&workspace) {
            return Err("trace document outside workspace".into());
        }
        let bytes = fs::read(&path).map_err(|e| e.to_string())?;
        let source = report::text(&bytes).0;
        let offset = source
            .match_indices(&case.needle)
            .nth(case.occurrence)
            .map(|(offset, _)| offset)
            .ok_or_else(|| format!("needle missing: {}", case.path))?;
        let uri = lsp::uri(&path)?;
        let mut version = 1;
        let since = client.notifications.len();
        let opened = Instant::now();
        eprintln!(
            "completion replay: opening {} ({} bytes)",
            case.path,
            bytes.len()
        );
        client.open(&path, &source, version)?;
        let params = params(&uri, &source, offset + case.needle.len());
        let id = client.send_request("textDocument/completion", params.clone())?;
        let (cold, at) = client.wait_response(id, timeout)?;
        let cold = sample("open", version, &case.needle, opened, at, &cold)?;
        eprintln!("completion replay: cold {}", cold);
        // Separate the first interaction from settled queries. A publication
        // timeout is recorded; it must never be represented as zero latency.
        let published = client
            .wait_notification_since(
                "textDocument/publishDiagnostics",
                since,
                publication_timeout,
                |p| p["uri"] == uri && p["version"] == version,
            )
            .is_ok();
        let mut stable = Vec::new();
        let mut stable_fingerprint = Value::Null;
        let mut stable_variants = std::collections::BTreeSet::new();
        for _ in 0..samples {
            let start = Instant::now();
            let id = client.send_request("textDocument/completion", params.clone())?;
            let (response, at) = client.wait_response(id, timeout)?;
            stable_fingerprint = json!(report::hash(
                &serde_json::to_vec(&normalize(response["result"].clone()))
                    .map_err(|e| e.to_string())?
            ));
            stable_variants.insert(stable_fingerprint.clone().to_string());
            stable.push(sample(
                "stable",
                version,
                &case.needle,
                start,
                at,
                &response,
            )?);
        }
        if stable.iter().all(|record| record["items"] == 0) {
            failures.push(format!(
                "{}: all settled completion samples were empty",
                case.path
            ));
        }
        if stable_variants.len() != 1 {
            failures.push(format!(
                "{}: unchanged document returned different completion results",
                case.path
            ));
        }
        let mut phases = Vec::new();
        let mut previous_width = case.needle.len();
        for reindex in [false, true]
            .into_iter()
            .filter(|value| !value || args.flag("--reindex"))
        {
            let reindex_id = if reindex {
                Some(client.send_request(
                    "workspace/executeCommand",
                    json!({"command":"pdc/reindexWorkspace","arguments":[]}),
                )?)
            } else {
                None
            };
            let mut pending = Vec::new();
            let mut final_source = String::new();
            let mut final_params = Value::Null;
            let since = client.notifications.len();
            let mut previous = None;
            for index in 0..samples {
                if let Some(id) = previous {
                    client.notify("$/cancelRequest", json!({"id":id}))?;
                }
                let width = 1 + index % case.needle.len();
                let prefix = &case.needle[..width];
                let mut edited = source.clone();
                edited.replace_range(offset..offset + case.needle.len(), prefix);
                version += 1;
                let start = Instant::now();
                let change = if args.flag("--full-text") {
                    json!({"text":edited})
                } else {
                    let start = params_for_offset(&source, offset);
                    let mut end = start.clone();
                    end["character"] =
                        json!(start["character"].as_u64().unwrap() + previous_width as u64);
                    json!({"range":{"start":start,"end":end},"text":prefix})
                };
                client.notify(
                    "textDocument/didChange",
                    json!({"textDocument":{"uri":uri,"version":version},"contentChanges":[change]}),
                )?;
                previous_width = width;
                let position = params_for_offset(&edited, offset + width);
                final_params = json!({"textDocument":{"uri":uri},"position":position});
                let id = client.send_request("textDocument/completion", final_params.clone())?;
                pending.push((id, start, version, prefix.to_owned()));
                previous = Some(id);
                final_source = edited;
                std::thread::sleep(interval);
            }
            let mut records = Vec::new();
            let mut latest = Value::Null;
            for (id, start, version, prefix) in &pending {
                let (response, at) = client.wait_response(*id, timeout)?;
                records.push(sample("typing", *version, prefix, *start, at, &response)?);
                latest = response;
            }
            let mut reindex_error = Value::Null;
            let reindex_finished = if let Some(id) = reindex_id {
                let (response, at) = client.wait_response(id, timeout)?;
                if let Some(error) = response.get("error") {
                    if error["code"] != -32600
                        || !error["message"].as_str().is_some_and(|message| {
                            message.contains("workspace changed while reindexing")
                        })
                    {
                        return Err(format!("reindex failed: {response}"));
                    }
                    reindex_error = error.clone();
                }
                Some(at)
            } else {
                None
            };
            let overlap = reindex_finished.map(|at| {
                pending
                    .iter()
                    .filter(|(_, start, _, _)| *start < at)
                    .count()
            });
            let publication = client
                .wait_notification_since(
                    "textDocument/publishDiagnostics",
                    since,
                    publication_timeout,
                    |p| p["uri"] == uri && p["version"] == version,
                )
                .is_ok();
            let settled = client.request("textDocument/completion", final_params)?;
            let latest_matches = latest
                .get("result")
                .is_some_and(|result| normalize(result.clone()) == normalize(settled.clone()));
            if !latest_matches {
                failures.push(format!(
                    "{}: final typing response differs from settled response (reindex={reindex})",
                    case.path
                ));
            }
            // Latest edits must be relative to the latest UTF-16 document.
            if let Some(items) = settled["items"].as_array() {
                for item in items {
                    for end in ["start", "end"] {
                        let position = &item["textEdit"]["range"][end];
                        crate::audit::client::byte_offset(
                            &final_source,
                            position["line"].as_u64().ok_or("missing edit line")? as usize,
                            position["character"]
                                .as_u64()
                                .ok_or("missing edit column")? as usize,
                        )?;
                    }
                }
            }
            phases.push(json!({"reindex":reindex,"reindex_obsolete_result":reindex_error,"requests_before_reindex_response":overlap,"latest_matches_settled":latest_matches,"latest_diagnostics_observed":publication,"summary":summarize(&records),"samples":records}));
        }
        client.close(&path)?;
        eprintln!("completion replay: {}", case.path);
        runs.push(json!({"path":case.path,"source_sha256":report::hash(&bytes),"source_bytes":bytes.len(),"needle":case.needle,"occurrence":case.occurrence,"cold":cold,"open_diagnostics_observed":published,"stable_result_sha256":stable_fingerprint,"stable_result_variants":stable_variants.len(),"stable_summary":summarize(&stable),"stable_samples":stable,"typing":phases}));
        report::write(
            output,
            &json!({"status":"running","runs":runs,"failures":failures}),
        )?;
    }
    client.shutdown()?;
    let result = json!({"status":if failures.is_empty(){"passed"}else{"failed"},"failures":failures,"binary_sha256":report::file_hash(&binary)?,"initialize":initialized,"workspace_inventory":workspace_inventory,"initialization_ms":initialization_ms,"options":options,"edit_sync":if args.flag("--full-text"){"full"}else{"incremental"},"samples_per_phase":samples,"interval_ms":interval.as_millis(),"runs":runs,"limits":"Local stdio latency, not editor rendering. Nearest-rank percentiles; canceled responses and empty results are reported separately. Small sample P99 is the sample maximum."});
    report::write(output, &result)?;
    if result["status"] != "passed" {
        return Err(format!("completion replay failed; {}", output.display()));
    }
    Ok(output.display().to_string())
}

fn params(uri: &str, source: &str, offset: usize) -> Value {
    json!({"textDocument":{"uri":uri},"position":params_for_offset(source, offset)})
}
fn params_for_offset(source: &str, offset: usize) -> Value {
    let before = &source[..offset];
    json!({"line":before.bytes().filter(|byte| *byte == b'\n').count(),"character":before.rsplit('\n').next().unwrap_or_default().encode_utf16().count()})
}
fn sample(
    phase: &str,
    version: i64,
    prefix: &str,
    start: Instant,
    received: Instant,
    response: &Value,
) -> Result<Value, String> {
    let cancelled = response["error"]["code"] == -32800;
    if response.get("error").is_some() && !cancelled {
        return Err(format!("completion RPC failed: {response}"));
    }
    let items = response["result"]["items"].as_array().map(Vec::len);
    if !cancelled && items.is_none() {
        return Err("completion response has no items array".into());
    }
    Ok(
        json!({"phase":phase,"version":version,"prefix":prefix,"elapsed_ms":received.saturating_duration_since(start).as_secs_f64()*1000.0,"cancelled":cancelled,"items":items,"incomplete":response["result"]["isIncomplete"]}),
    )
}
fn summarize(records: &[Value]) -> Value {
    let successful: Vec<f64> = records
        .iter()
        .filter(|r| r["cancelled"] == false && r["items"].as_u64().is_some_and(|count| count > 0))
        .filter_map(|r| r["elapsed_ms"].as_f64())
        .collect();
    let cancelled: Vec<f64> = records
        .iter()
        .filter(|r| r["cancelled"] == true)
        .filter_map(|r| r["elapsed_ms"].as_f64())
        .collect();
    json!({"nonempty_samples":successful.len(),"empty_samples":records.iter().filter(|r|r["items"] == 0).count(),"cancelled_samples":cancelled.len(),"p50_ms":percentile(&successful,50),"p95_ms":percentile(&successful,95),"p99_ms":percentile(&successful,99),"cancel_p50_ms":percentile(&cancelled,50),"cancel_p95_ms":percentile(&cancelled,95),"cancel_p99_ms":percentile(&cancelled,99)})
}
fn percentile(values: &[f64], percent: usize) -> Option<f64> {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    sorted
        .get((sorted.len() * percent).div_ceil(100).saturating_sub(1))
        .copied()
}
fn normalize(mut value: Value) -> Value {
    if let Some(items) = value["items"].as_array_mut() {
        for item in items {
            if let Some(template) = item
                .get_mut("data")
                .and_then(|data| data.get_mut("template"))
                .and_then(Value::as_object_mut)
            {
                template.remove("revision");
            }
        }
    }
    value
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn latency_summary_separates_empty_and_cancelled_samples() {
        let records = vec![
            json!({"cancelled":false,"items":2,"elapsed_ms":10.0}),
            json!({"cancelled":false,"items":0,"elapsed_ms":0.1}),
            json!({"cancelled":true,"items":null,"elapsed_ms":1.0}),
        ];
        let summary = summarize(&records);
        assert_eq!(summary["p99_ms"], 10.0);
        assert_eq!(summary["nonempty_samples"], 1);
        assert_eq!(summary["empty_samples"], 1);
        assert_eq!(summary["cancelled_samples"], 1);
        assert_eq!(percentile(&[], 95), None);
        assert_eq!(
            percentile(&(1..=100).map(f64::from).collect::<Vec<_>>(), 95),
            Some(95.0)
        );
        assert_eq!(
            params_for_offset("# 中文😀\r\n键 = add", "# 中文😀\r\n键 = add".len()),
            json!({"line":1,"character":7})
        );
    }
}
