//! Phase-zero owned reference and real-process Template query observations.
//! Baselines record current behavior, including known gaps; they are not an acceptance oracle.
mod calls;
mod finite;
pub mod oracle;

use crate::{
    args::Args,
    lsp::{self, Client},
    process, report,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs,
    path::Path,
    time::{Duration, Instant},
};

const HELP: &str = "tools audit templates --output DIR [--check-only] [--server PATH] [--vanilla-cache PATH] [--samples N] [--timeout-ms N]";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Query {
    id: String,
    path: String,
    text: String,
    anchor: String,
    offset: usize,
    target: String,
}

fn position(source: &str, byte: usize) -> Result<Value, String> {
    let before = source.get(..byte).ok_or("query splits a UTF-8 character")?;
    Ok(
        json!({"line":before.bytes().filter(|b| *b == b'\n').count(),"character":before.rsplit('\n').next().unwrap_or("").encode_utf16().count()}),
    )
}

fn queries() -> Result<Vec<Query>, String> {
    let queries: Vec<Query> =
        serde_json::from_str(include_str!("queries.json")).map_err(|e| e.to_string())?;
    let mut ids = BTreeSet::new();
    for query in &queries {
        if !ids.insert(&query.id)
            || !Path::new(&query.path)
                .components()
                .all(|c| matches!(c, std::path::Component::Normal(_)))
            || query.text.matches(&query.anchor).count() != 1
            || query.offset > query.anchor.len()
        {
            return Err(format!("invalid owned query: {}", query.id));
        }
        position(
            &query.text,
            query.text.find(&query.anchor).ok_or("missing anchor")? + query.offset,
        )?;
    }
    Ok(queries)
}

fn write_fixture(workspace: &Path) -> Result<(), String> {
    let mut effects = String::from(
        "stage0_bool = { log = $P$ set_emperor = $P$ add_prestige = $N$ }\n\
         stage0_key = { add_base_$K$ = $V$ }\n\
         stage0_scope = { $TARGET$ = { add_base_tax = 1 } }\n\
         stage0_body = { $BODY$ }\n\
         stage0_guard = { [[P] set_emperor = $P$ ] }\n",
    );
    for depth in 0..40 {
        effects.push_str(&format!(
            "stage0_chain{depth} = {{ stage0_chain{} = {{ N = $N$ }} }}\n",
            depth + 1
        ));
    }
    effects.push_str("stage0_chain40 = { add_prestige = $N$ }\n");
    let files = [
        ("common/scripted_effects/stage0.txt", effects.as_str()),
        (
            "common/scripted_triggers/stage0.txt",
            "stage0_repeated = { has_country_flag = $P$_$P$ }\n",
        ),
        (
            "events/declarations.txt",
            "country_event = { id = stage0.declarations immediate = { set_country_flag = abc_abc set_country_flag = stage0_flag } option = { name = stage0_option } }\n",
        ),
        (
            "localisation/stage0_l_english.yml",
            "l_english:\n stage0_option:0 \"Owned option\"\n",
        ),
    ];
    for (relative, text) in files {
        let path = workspace.join(relative);
        fs::create_dir_all(path.parent().ok_or("fixture path has no parent")?)
            .map_err(|e| e.to_string())?;
        fs::write(path, text).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn normalize(value: &mut Value, workspace: &Path) -> Result<(), String> {
    let uri = lsp::uri(workspace)?;
    fn walk(value: &mut Value, path: &str, uri: &str) {
        match value {
            Value::String(s) => {
                *s = s
                    .replace(uri, "owned://workspace")
                    .replace(path, "<owned-workspace>")
            }
            Value::Array(items) => items.iter_mut().for_each(|v| walk(v, path, uri)),
            Value::Object(fields) => {
                fields.remove("resultId"); // Protocol cache identity, not semantic content.
                fields.values_mut().for_each(|v| walk(v, path, uri));
            }
            _ => {}
        }
    }
    walk(value, &workspace.to_string_lossy(), &uri);
    Ok(())
}

fn percentiles(samples: &[f64]) -> Value {
    let mut ordered = samples.to_vec();
    ordered.sort_by(f64::total_cmp);
    let percentile = |p: f64| {
        let index = ((ordered.len() as f64 * p).ceil() as usize).saturating_sub(1);
        ordered.get(index).copied()
    };
    json!({"samples":samples,"count":samples.len(),"p50_ms":percentile(0.5),"p95_ms":percentile(0.95)})
}

fn measure(
    client: &mut Client,
    method: &str,
    params: Value,
    samples: usize,
    workspace: &Path,
    allow_error: bool,
) -> Result<Value, String> {
    let mut timings = Vec::new();
    let mut observed = None;
    let mut response_digest = None;
    let mut stable = true;
    let mut cold_ms = None;
    // Keep first-use cost separate; do not include it among warmed observations.
    for sample in 0..=samples {
        let start = Instant::now();
        let response = client.request(method, params.clone());
        let elapsed = start.elapsed().as_secs_f64() * 1000.0;
        if sample == 0 {
            cold_ms = Some(elapsed);
        } else {
            timings.push(elapsed);
        }
        let mut value = match response {
            Ok(value) => json!({"result":value}),
            Err(error)
                if allow_error
                    && error.starts_with(&format!("{method}: "))
                    && error.contains("\"code\"") =>
            {
                json!({"rpc_error":error})
            }
            Err(error) => return Err(error),
        };
        normalize(&mut value, workspace)?;
        let digest = report::hash(
            serde_json::to_string(&value)
                .map_err(|e| e.to_string())?
                .as_bytes(),
        );
        if let Some(prior) = &response_digest {
            stable &= prior == &digest;
        } else {
            response_digest = Some(digest);
        }
        observed.get_or_insert(value);
    }
    Ok(
        json!({"method":method,"first_use_ms":cold_ms,"warm":percentiles(&timings),"response":observed,"response_sha256":response_digest,"stable_repeated_response":stable}),
    )
}

fn baseline(
    binary: &Path,
    workspace: &Path,
    samples: usize,
    timeout: Duration,
    cache: Option<&Path>,
) -> Result<Value, String> {
    write_fixture(workspace)?;
    let mut options = json!({"modDirectory":workspace,"workspaceWideDiagnostics":true});
    let cache_identity = if let Some(cache) = cache {
        options["vanillaIndexCache"] =
            json!(std::path::absolute(cache).map_err(|e| e.to_string())?);
        json!({"path":cache,"sha256":report::file_hash(cache)?})
    } else {
        options["vanillaMode"] = json!("disabled");
        Value::Null
    };
    let boot = Instant::now();
    let mut client = Client::spawn(binary, workspace, timeout)?;
    let mut sampler = crate::perf::sampler::Sampler::start(client.pid(), Duration::from_millis(50));
    let initialized = client.initialize(workspace, options)?;
    let boot_ms = boot.elapsed().as_secs_f64() * 1000.0;
    let identity = client.request("pdc/workspaceSummary", Value::Null)?;
    let analyzer_info = match client.request("pdc/analyzerInfo", Value::Null) {
        Ok(value) => json!({"result":value}),
        Err(error) if error.contains("-32601") => {
            json!({"rpc_error":error,"status":"not-routed-by-current-server"})
        }
        Err(error) => return Err(error),
    };
    let embedded = game::eu4::first_party_ir().map_err(|e| e.to_string())?;
    if identity["ruleHash"] != embedded.fingerprint()
        || initialized["serverInfo"]["version"] != env!("CARGO_PKG_VERSION")
    {
        return Err("selected server and reference tool identity differ".into());
    }
    let mut states = Vec::new();
    let navigation = regex::Regex::new(r"stage0_[A-Za-z0-9_]+").map_err(|e| e.to_string())?;
    for (index, query) in queries()?.into_iter().enumerate() {
        let path = workspace.join(&query.path);
        let uri = lsp::uri(&path)?;
        let version = index as i64 + 1;
        let publication_since = client.notifications.len();
        let edit_start = Instant::now();
        if index > 0 && query.path == "events/trace.txt" {
            client.notify("textDocument/didChange", json!({"textDocument":{"uri":uri,"version":version},"contentChanges":[{"text":query.text}]}))?;
        } else {
            client.open(&path, &query.text, version)?;
        }
        let publication = client.wait_notification_since(
            "textDocument/publishDiagnostics",
            publication_since,
            Duration::from_secs(2),
            |v| v["uri"] == uri && v["version"] == version,
        );
        let publication = match publication {
            Ok(mut value) => {
                let latency = edit_start.elapsed().as_secs_f64() * 1000.0;
                normalize(&mut value, workspace)?;
                json!({"status":"observed","latency_ms":latency,"value":value})
            }
            Err(error) if error.contains("timeout") || error.contains("timed out") => {
                json!({"status":"unobserved-or-deduplicated","latency_ms":null,"reason":error})
            }
            Err(error) => return Err(error),
        };
        let offset = query.text.find(&query.anchor).ok_or("missing focus")? + query.offset;
        let focus = position(&query.text, offset)?;
        let nav = position(
            &query.text,
            navigation
                .find(&query.text)
                .map_or(offset, |m| m.start() + 2),
        )?;
        let end = position(&query.text, query.text.len())?;
        let document = json!({"textDocument":{"uri":uri}});
        let mut focus_params = document.clone();
        focus_params["position"] = focus.clone();
        let mut nav_params = document.clone();
        nav_params["position"] = nav.clone();
        let range = json!({"start":{"line":0,"character":0},"end":end});
        let diagnostics = client.request(
            "pdc/textDiagnostics",
            json!({"files":[{"path":query.path,"text":query.text}]}),
        )?;
        let mut requests = vec![
            ("textDocument/completion", focus_params.clone(), false),
            ("textDocument/hover", focus_params, false),
            ("textDocument/definition", nav_params.clone(), false),
            (
                "textDocument/references",
                {
                    let mut v = nav_params.clone();
                    v["context"] = json!({"includeDeclaration":true});
                    v
                },
                false,
            ),
            ("textDocument/prepareRename", nav_params.clone(), true),
            (
                "textDocument/rename",
                {
                    let mut v = nav_params;
                    v["newName"] = json!("stage0_renamed");
                    v
                },
                true,
            ),
            ("textDocument/documentSymbol", document.clone(), false),
            ("workspace/symbol", json!({"query":"stage0"}), false),
            ("textDocument/semanticTokens/full", document.clone(), false),
            (
                "textDocument/inlayHint",
                {
                    let mut v = document.clone();
                    v["range"] = range.clone();
                    v
                },
                false,
            ),
            (
                "textDocument/codeAction",
                {
                    let mut v = document;
                    v["range"] = range;
                    v["context"] = json!({"diagnostics":diagnostics[0]["diagnostics"]});
                    v
                },
                false,
            ),
            (
                "pdc/textDiagnostics",
                json!({"files":[{"path":query.path,"text":query.text}]}),
                false,
            ),
        ];
        let mut measurements = Vec::new();
        for (method, params, allowed) in requests.drain(..) {
            measurements.push(measure(
                &mut client,
                method,
                params,
                samples,
                workspace,
                allowed,
            )?);
        }
        let completion = measurements[0]["response"]["result"]["items"]
            .as_array()
            .or_else(|| measurements[0]["response"]["result"].as_array())
            .and_then(|items| items.first())
            .cloned();
        if let Some(mut item) = completion {
            // Request needs the original URI in opaque resolve data, not normalized report data.
            fn restore(value: &mut Value, original: &str) {
                match value {
                    Value::String(s) => *s = s.replace("owned://workspace", original),
                    Value::Array(items) => items.iter_mut().for_each(|v| restore(v, original)),
                    Value::Object(fields) => fields.values_mut().for_each(|v| restore(v, original)),
                    _ => {}
                }
            }
            restore(&mut item, &lsp::uri(workspace)?);
            measurements.push(measure(
                &mut client,
                "completionItem/resolve",
                item,
                samples,
                workspace,
                false,
            )?);
        }
        states.push(json!({"id":query.id,"path":query.path,"version":version,"source":query.text,"source_sha256":report::hash(query.text.as_bytes()),"focus":focus,"navigation_focus":nav,"target_contract":query.target,"current_semantic_acceptance":"observation-only","publication":publication,"queries":measurements}));
    }
    let process = sampler.finish();
    client.shutdown()?;
    Ok(
        json!({"status":"observed-current-behavior","server":{"binary_sha256":report::file_hash(binary)?,"initialize":initialized,"analyzer":identity,"analyzer_info":analyzer_info},"tool_version":env!("CARGO_PKG_VERSION"),"fixture_sha256":{"text_reference":report::hash(include_bytes!("cases.json")),"finite_reference":report::hash(include_bytes!("finite.json")),"queries":report::hash(include_bytes!("queries.json"))},"vanilla_cache":cache_identity,"workspace_mode":if cache.is_some(){"owned overlays with explicit frozen Vanilla cache"}else{"isolated owned fixtures; Vanilla disabled"},"boot_ms":boot_ms,"process":process,"states":states,"limitations":["current outputs are not the correctness oracle","call reference covers concrete scalar arguments; block arguments and EU4 engine termination remain unverified","mixed-load cancellation and generated-fact stability need dedicated later probes"]}),
    )
}

pub fn execute(arguments: &[String]) -> Result<String, String> {
    let args = Args::parse(
        arguments,
        &[
            "--root",
            "--repo",
            "--output",
            "--server",
            "--vanilla-cache",
            "--samples",
            "--timeout-ms",
        ],
        &["--check-only"],
    )?;
    if args.help() {
        return Ok(HELP.into());
    }
    if !args.positional.is_empty() || !args.forwarded.is_empty() {
        return Err(HELP.into());
    }
    let root = args.root()?;
    let output = report::output(
        &root,
        Path::new(args.required("--output")?),
        "performance-results",
    )?;
    let reference = oracle::check()?;
    let finite = finite::check()?;
    let calls = calls::check()?;
    queries()?;
    report::write(&output.join("reference.json"), &reference)?;
    report::write(&output.join("finite-reference.json"), &finite)?;
    report::write(&output.join("call-reference.json"), &calls)?;
    if args.flag("--check-only") {
        return Ok(format!(
            "{} owned text-reference cases passed; engine evidence remains unverified",
            reference["cases"].as_array().map_or(0, Vec::len)
        ));
    }
    report::write(&output.join("baseline.json"), &json!({"status":"running"}))?;
    let samples = args.number("--samples", 20)?;
    if samples > 100 {
        return Err("--samples must not exceed 100".into());
    }
    let binary = process::server(&root, args.get("--server"))?;
    fs::create_dir_all(&output).map_err(|e| e.to_string())?;
    let workspace = tempfile::Builder::new()
        .prefix("owned-template-")
        .tempdir_in(&output)
        .map_err(|e| e.to_string())?;
    let result = baseline(
        &binary,
        workspace.path(),
        samples,
        Duration::from_millis(args.number("--timeout-ms", 30_000)? as u64),
        args.path("--vanilla-cache").as_deref(),
    );
    match result {
        Ok(mut value) => {
            value["tool_binary_sha256"] = json!(report::file_hash(
                &std::env::current_exe().map_err(|e| e.to_string())?
            )?);
            report::write(&output.join("baseline.json"), &value)?;
            Ok(format!(
                "Recorded {} owned Template editing states with {samples} warmed samples per query; current behavior requires semantic review",
                value["states"].as_array().map_or(0, Vec::len)
            ))
        }
        Err(error) => {
            let failure = json!({"status":"failed","error":error,"server_sha256":report::file_hash(&binary)?});
            report::write(&output.join("failure.json"), &failure)?;
            report::write(&output.join("baseline.json"), &failure)?;
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn owned_queries_have_unique_anchors_and_valid_utf16_positions() {
        assert_eq!(queries().unwrap().len(), 12);
        let text = "界😀x";
        assert_eq!(position(text, 7).unwrap(), json!({"line":0,"character":3}));
        assert!(position(text, 4).is_err());
    }
    #[test]
    fn percentiles_keep_missing_samples_absent() {
        assert!(percentiles(&[])["p95_ms"].is_null());
        assert_eq!(percentiles(&[4., 1., 3., 2.])["p50_ms"], 2.);
    }
}
