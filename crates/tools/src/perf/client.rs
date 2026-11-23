//! Real LSP lifecycle and interactive latency measurements.
use crate::{
    args::Args,
    lsp::{self, Client},
    process, report,
};
use serde_json::{Value, json};
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};
pub fn execute(mode: &str, arguments: &[String]) -> Result<String, String> {
    if arguments.first().is_some_and(|arg| arg == "--compare") {
        if arguments.len() != 3 {
            return Err("perf compare --compare BEFORE.json AFTER.json".into());
        }
        return compare_files(Path::new(&arguments[1]), Path::new(&arguments[2]));
    }
    let mut args = Args::parse(
        arguments,
        &[
            "--repo",
            "--root",
            "--server",
            "--workspace",
            "--mod",
            "--document",
            "--cache",
            "--vanilla-cache",
            "--output",
            "--out",
            "--samples",
            "--timeout-ms",
            "--memory-interval-ms",
            "--sample-interval-ms",
            "--idle-window-ms",
            "--close-settle-ms",
            "--edit-grace-ms",
            "--label",
            "--line",
            "--character",
            "--dependency",
            "--dependency-index",
        ],
        &[
            "--no-cache",
            "--keep-workspace",
            "--no-workspace-diagnostics",
        ],
    )?;
    args.environment(&[
        ("--server", "PDC_PERF_SERVER"),
        ("--workspace", "PDC_PERF_WORKSPACE"),
        ("--document", "PDC_PERF_DOCUMENT"),
        ("--cache", "PDC_PERF_CACHE"),
        ("--line", "PDC_PERF_LINE"),
        ("--character", "PDC_PERF_CHARACTER"),
        ("--timeout-ms", "PDC_PERF_TIMEOUT_MS"),
        ("--memory-interval-ms", "PDC_PERF_MEMORY_INTERVAL_MS"),
        ("--samples", "PDC_PERF_SAMPLES"),
    ]);
    if args.help() {
        return Ok(super::HELP.into());
    }
    let root = args.root()?;
    let output = report::output(
        &root,
        &args
            .path("--output")
            .or_else(|| args.path("--out"))
            .unwrap_or_else(|| {
                root.join("target/performance-results")
                    .join(format!("{mode}.json"))
            }),
        "performance-results",
    )?;
    let temporary = tempfile::Builder::new()
        .prefix("pdc-perf-")
        .tempdir()
        .map_err(|e| e.to_string())?;
    let workspace = args
        .path("--workspace")
        .or_else(|| args.path("--mod"))
        .unwrap_or_else(|| temporary.path().to_owned());
    let fixture = args.get("--workspace").is_none() && args.get("--mod").is_none();
    if mode == "compare" && fixture {
        return Err("perf compare requires --workspace".into());
    }
    if fixture {
        fs::create_dir_all(workspace.join("events")).map_err(|e| e.to_string())?;
        fs::write(
            workspace.join("events/perf.txt"),
            "country_event = {\n    id = my_perf_event\n}\n",
        )
        .map_err(|e| e.to_string())?;
    }
    let workspace = workspace.canonicalize().map_err(|e| e.to_string())?;
    let binary = process::server(&root, args.get("--server"))?;
    let timeout = Duration::from_millis(args.number(
        "--timeout-ms",
        if mode == "probe" { 60_000 } else { 300_000 },
    )? as u64);
    let sample_interval = Duration::from_millis(args.number(
        "--sample-interval-ms",
        args.number("--memory-interval-ms", 250)?,
    )? as u64);
    let cache = args
        .path("--cache")
        .or_else(|| args.path("--vanilla-cache"));
    if cache.is_some() && args.flag("--no-cache") {
        return Err("choose either --cache or --no-cache".into());
    }
    if cache.is_none() && !args.flag("--no-cache") {
        return Err("select --cache or explicit --no-cache".into());
    }
    let mut options = json!({"modDirectory":workspace,"workspaceWideDiagnostics":!args.flag("--no-workspace-diagnostics")});
    if args.flag("--no-cache") {
        options["vanillaMode"] = json!("disabled");
    } else if let Some(cache) = cache {
        options["vanillaIndexCache"] =
            json!(std::path::absolute(cache).map_err(|e| e.to_string())?);
    }
    let dependencies = super::dependencies::Dependencies::new(&args, &workspace)?;
    options["dependencies"] = json!(dependencies.options);
    let started = Instant::now();
    let mut client = Client::spawn(&binary, &workspace, timeout)?;
    let mut sampler = super::sampler::Sampler::start(client.pid(), sample_interval);
    let initial = Instant::now();
    let initialized = client.handshake(&workspace, options)?;
    let initialize_ms = initial.elapsed().as_secs_f64() * 1000.0;
    let ready = Instant::now();
    client.wait_notification("pdc/ready", |_| true)?;
    let ready_ms = ready.elapsed().as_secs_f64() * 1000.0;
    let cold_start_ms = started.elapsed().as_secs_f64() * 1000.0;
    let idle_ms = args
        .get("--idle-window-ms")
        .unwrap_or(if mode == "probe" { "0" } else { "20000" })
        .parse::<u64>()
        .map_err(|e| e.to_string())?;
    let idle_start = (idle_ms > 0)
        .then(|| super::sampler::sample(client.pid()).ok())
        .flatten();
    let idle_started = Instant::now();
    std::thread::sleep(Duration::from_millis(idle_ms));
    let idle_end = (idle_ms > 0)
        .then(|| super::sampler::sample(client.pid()).ok())
        .flatten();
    let idle_observed = idle_started.elapsed().as_secs_f64();
    let idle_cpu = idle_start
        .as_ref()
        .and_then(|s| s["cpu_seconds"].as_f64())
        .zip(idle_end.as_ref().and_then(|s| s["cpu_seconds"].as_f64()))
        .map(|(a, b)| (b - a).max(0.0));
    let idle = json!({"window_ms":idle_ms,"observed_window_ms":(idle_ms>0).then_some(idle_observed*1000.0),"cpu_seconds":idle_cpu,"cpu_rate_percent":idle_cpu.map(|seconds|seconds/idle_observed*100.0)});
    let mut files = if let Some(path) = args.path("--document") {
        vec![if path.is_absolute() {
            path
        } else {
            workspace.join(path)
        }]
    } else {
        report::files(&workspace)?
            .into_iter()
            .filter(|p| {
                p.extension()
                    .is_some_and(|e| e == "txt" || e == "gfx" || e == "yml")
            })
            .collect()
    };
    files.sort();
    let classified=client.request("pdc/classifyPaths",json!({"paths":files.iter().map(|p|p.strip_prefix(&workspace).map(|p|p.to_string_lossy().replace('\\',"/")).map_err(|_|"document outside workspace")).collect::<Result<Vec<_>,_>>()?}))?;
    let accepted = classified
        .as_array()
        .ok_or("invalid classified paths")?
        .iter()
        .filter_map(Value::as_str)
        .collect::<std::collections::BTreeSet<_>>();
    files.retain(|p| {
        accepted.contains(
            p.strip_prefix(&workspace)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/")
                .as_str(),
        )
    });
    let count = args.number("--samples", if mode == "probe" { 1 } else { 6 })?;
    if files.is_empty() {
        return Err("workspace has no diagnosable files".into());
    }
    let mut measurements = Vec::new();
    let selected = if mode == "compare" && files.len() > count {
        (0..count)
            .map(|i| {
                &files[((i as f64 * (files.len() - 1) as f64) / (count - 1).max(1) as f64).round()
                    as usize]
            })
            .collect::<Vec<_>>()
    } else {
        files.iter().take(count).collect()
    };
    let word_pattern = regex::Regex::new(r"[A-Za-z_][A-Za-z0-9_.-]*").unwrap();
    let completion_pattern = regex::Regex::new(r"^(\s*)([A-Za-z0-9_.@]+)\s*=").unwrap();
    for path in selected {
        let bytes = fs::read(path).map_err(|e| e.to_string())?;
        let text = report::text(&bytes).0;
        let uri = lsp::uri(path)?;
        let preferred = if mode == "probe" {
            text.find("my_perf_event")
                .or_else(|| word_pattern.find(&text).map(|m| m.start()))
                .unwrap_or(0)
        } else {
            text.find('=').unwrap_or(0)
        };
        let before = &text[..preferred];
        let inferred_line = before.bytes().filter(|b| *b == b'\n').count();
        let inferred_column = before
            .rsplit('\n')
            .next()
            .unwrap_or("")
            .encode_utf16()
            .count();
        let line = args
            .get("--line")
            .map(|s| s.parse::<u64>().map_err(|e| e.to_string()))
            .transpose()?
            .unwrap_or(inferred_line as u64);
        let character = args
            .get("--character")
            .map(|s| s.parse::<u64>().map_err(|e| e.to_string()))
            .transpose()?
            .unwrap_or(inferred_column as u64);
        crate::audit::client::byte_offset(&text, line as usize, character as usize)?;
        let since = client.notifications.len();
        let start = Instant::now();
        client.open(path, &text, 1)?;
        let published = client.wait_notification_since(
            "textDocument/publishDiagnostics",
            since,
            Duration::from_millis(args.number(
                "--edit-grace-ms",
                if mode == "probe" { 60_000 } else { 2000 },
            )? as u64),
            |p| p["uri"] == uri && p["version"] == 1,
        );
        let open_ms = if published.is_ok() {
            Some(start.elapsed().as_secs_f64() * 1000.0)
        } else {
            None
        };
        let mut item = json!({"path":path.strip_prefix(&workspace).unwrap().to_string_lossy().replace('\\',"/"),"open_diagnostics_ms":open_ms,"open_publication":if published.is_ok(){"observed"}else{"suppressed-or-unobserved"}});
        item["source_sha256"] = json!(report::hash(&bytes));
        item["hover_position"] = json!({"line":line,"character":character});
        let start = Instant::now();
        let hover = client.request(
            "textDocument/hover",
            json!({"textDocument":{"uri":uri},"position":{"line":line,"character":character}}),
        )?;
        item["hover_ms"] = json!(start.elapsed().as_secs_f64() * 1000.0);
        item["hover_present"] = json!(!hover.is_null());
        let positions = if mode == "compare" {
            let pattern = &completion_pattern;
            text.split('\n')
                .enumerate()
                .filter_map(|(line, text)| {
                    pattern.captures(text).map(
                        |matched| json!({"line":line,"character":matched.get(2).unwrap().end()}),
                    )
                })
                .take(12)
                .collect::<Vec<_>>()
        } else {
            vec![json!({"line":line,"character":character})]
        };
        let mut completion = Vec::new();
        for position in positions {
            let start = Instant::now();
            let response = client.request(
                "textDocument/completion",
                json!({"textDocument":{"uri":uri},"position":position}),
            )?;
            let count = response
                .as_array()
                .or_else(|| response["items"].as_array())
                .ok_or("invalid completion response")?
                .len();
            completion.push(json!({"position":position,"items":count,"elapsed_ms":start.elapsed().as_secs_f64()*1000.0,"is_incomplete":response["isIncomplete"].as_bool().unwrap_or(false)}));
        }
        item["completion_ms"] = completion
            .iter()
            .find(|p| p["items"].as_u64().unwrap_or(0) > 0)
            .or_else(|| completion.first())
            .map(|p| p["elapsed_ms"].clone())
            .unwrap_or(Value::Null);
        item["completion_probes"] = json!(completion);
        let since = client.notifications.len();
        let start = Instant::now();
        client.notify("textDocument/didChange",json!({"textDocument":{"uri":uri,"version":2},"contentChanges":[{"text":if mode=="probe"&&text.contains("my_perf_event"){text.replacen("my_perf_event","my_perf_event_renamed",1)}else{format!("{text}\n# perf-edit")}}]}))?;
        let published = client.wait_notification_since(
            "textDocument/publishDiagnostics",
            since,
            Duration::from_millis(args.number(
                "--edit-grace-ms",
                if mode == "probe" { 60_000 } else { 2000 },
            )? as u64),
            |p| p["uri"] == uri && p["version"] == 2,
        );
        item["edit_diagnostics_ms"] = json!(if published.is_ok() {
            Some(start.elapsed().as_secs_f64() * 1000.0)
        } else {
            None
        });
        item["edit_publication"] = json!(if published.is_ok() {
            "observed"
        } else {
            "suppressed-or-unobserved"
        });
        if published.is_err() {
            client.request(
                "textDocument/hover",
                json!({"textDocument":{"uri":uri},"position":{"line":line,"character":character}}),
            )?;
        }
        client.close(path)?;
        let settle = args
            .get("--close-settle-ms")
            .unwrap_or(if mode == "probe" { "0" } else { "2500" })
            .parse::<u64>()
            .map_err(|e| e.to_string())?;
        std::thread::sleep(Duration::from_millis(settle));
        measurements.push(item);
    }
    let summary = client.request("pdc/workspaceSummary", Value::Null)?;
    client.shutdown()?;
    let sampled = sampler.finish();
    let mut result = json!({"status":"measured-review-required","label":args.get("--label"),"server":{"path":binary,"version":initialized["serverInfo"]["version"],"binary_sha256":report::file_hash(&binary)?},"workspace":workspace,"cache_mode":if args.flag("--no-cache"){"disabled"}else{"explicit"},"phases":{"initialize_ms":initialize_ms,"ready_ms":ready_ms,"cold_start_ms":cold_start_ms,"total_ms":started.elapsed().as_secs_f64()*1000.0},"measurements":measurements,"idle":idle,"process":sampled,"workspace_summary":summary,"publication_note":"Absent notifications are explicitly unobserved or deduplicated; no zero latency is synthesized"});
    result["dependency_inputs"] = json!(dependencies.inventory);
    report::write(&output, &result)?;
    if fixture && args.flag("--keep-workspace") {
        let kept = temporary.keep();
        eprintln!("Fixture retained: {}", kept.display());
    }
    Ok(format!(
        "{mode}: {} samples; {}",
        measurements.len(),
        output.display()
    ))
}
pub fn compare_files(before: &Path, after: &Path) -> Result<String, String> {
    let before = report::json(before)?;
    let after = report::json(after)?;
    if before.get("workload").is_some() || after.get("workload").is_some() {
        return super::suite::compare_reports(&before, &after);
    }
    if ["/cache_mode", "/idle/window_ms", "/dependency_inputs"]
        .iter()
        .any(|p| {
            before
                .pointer(p)
                .zip(after.pointer(p))
                .is_some_and(|(a, b)| a != b)
        })
    {
        return Err("probe cache modes, dependencies or idle windows differ".into());
    }
    let mut lines = vec![
        "| Metric | Before | After | Change |".into(),
        "| --- | ---: | ---: | ---: |".into(),
    ];
    for key in ["initialize_ms", "ready_ms", "cold_start_ms", "total_ms"] {
        let left = before["phases"][key]
            .as_f64()
            .ok_or("missing baseline phase")?;
        let right = after["phases"][key]
            .as_f64()
            .ok_or("missing candidate phase")?;
        lines.push(comparison_row(key, Some(left), Some(right)));
    }
    let Some((a, b)) = before["measurements"]
        .as_array()
        .zip(after["measurements"].as_array())
    else {
        lines.push("\nPer-document samples are unavailable in this report pair.".into());
        return Ok(lines.join("\n"));
    };
    if a.len() != b.len() {
        return Err("probe document inventories differ".into());
    }
    for (old, new) in a.iter().zip(b) {
        let path = old["path"].as_str().ok_or("missing probe path")?;
        if old["path"] != new["path"]
            || ["source_sha256", "hover_position", "hover_present"]
                .iter()
                .any(|key| old.get(key).zip(new.get(key)).is_some_and(|(a, b)| a != b))
        {
            return Err(format!("probe inputs or hover outcome differ for {path}"));
        }
        let probes = |v: &Value| -> Result<Vec<Value>, String> {
            Ok(v["completion_probes"]
                .as_array()
                .ok_or("missing completion probes")?
                .iter()
                .map(|p| json!([p["position"], p["items"], p["is_incomplete"]]))
                .collect())
        };
        if old.get("completion_probes").is_some()
            && new.get("completion_probes").is_some()
            && probes(old)? != probes(new)?
        {
            return Err(format!("probe completion workload differs for {path}"));
        }
        for key in [
            "open_diagnostics_ms",
            "hover_ms",
            "completion_ms",
            "edit_diagnostics_ms",
        ] {
            lines.push(comparison_row(
                &format!("{path}/{key}"),
                old[key].as_f64(),
                new[key].as_f64(),
            ));
        }
    }
    for (name, pointer) in [
        (
            "peak working set (bytes)",
            "/process/peak_working_set_bytes",
        ),
        ("idle CPU (%)", "/idle/cpu_rate_percent"),
    ] {
        lines.push(comparison_row(
            name,
            before.pointer(pointer).and_then(Value::as_f64),
            after.pointer(pointer).and_then(Value::as_f64),
        ));
    }
    Ok(lines.join("\n"))
}

fn comparison_row(name: &str, before: Option<f64>, after: Option<f64>) -> String {
    let display = |v: Option<f64>| v.map_or_else(|| "unobserved".into(), |n| format!("{n:.3}"));
    let change = before.zip(after).filter(|(a, _)| *a > 0.0).map_or_else(
        || "n/a".into(),
        |(a, b)| format!("{:+.2}%", (b - a) / a * 100.0),
    );
    format!(
        "| {name} | {} | {} | {change} |",
        display(before),
        display(after)
    )
}
