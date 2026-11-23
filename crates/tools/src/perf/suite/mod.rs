//! End-to-end latency and correctness measurements against an isolated stdio server.
mod fixture;
mod interactions;
mod metrics;
pub(super) mod pair;
mod queries;
mod rounds;

use super::sampler;
use crate::{args::Args, lsp::Client, process, report};
use fixture::{Fixture, TERRAIN_PATH};
use metrics::{Metrics, record};
use queries::{Expect, Session};
use serde_json::{Value, json};
use std::path::Path;
use std::time::{Duration, Instant};

const SCHEMA: u32 = 1;
pub(super) fn compare_reports(before: &Value, after: &Value) -> Result<String, String> {
    if after["schema_version"] != SCHEMA || after["status"] != "passed" {
        return Err("candidate must be a successful suite report".into());
    }
    let current = serde_json::from_value(after["metrics"].clone()).map_err(|e| e.to_string())?;
    let mut comparison = metrics::compare(before, &after["workload"], &current, None, 0.0)?;
    comparison["resources"] =
        metrics::resource_changes(&before["resource_summary"], &after["resource_summary"]);
    serde_json::to_string_pretty(&comparison).map_err(|e| e.to_string())
}

fn process_sample(pid: u32) -> Value {
    sampler::sample(pid).unwrap_or_else(|error| json!({"error":error}))
}
pub const HELP: &str = "tools perf suite [--server BINARY] [--files N] [--document-entries N] [--sessions N] [--samples N] [--edit-rounds N] [--soak-cycles N] [--cache VANILLA] [--timeout-ms N] [--idle-ms N] [--output REPORT] [--baseline REPORT [--fail-over PCT] [--min-delta-ms MS]]";
const WORKLOAD_OPTIONS: &[&str] = &[
    "--files",
    "--document-entries",
    "--edit-rounds",
    "--soak-cycles",
    "--samples",
    "--cache",
    "--timeout-ms",
    "--idle-ms",
];

fn distinct_report(output: &Path, input: &Path) -> Result<(), String> {
    if input.canonicalize().is_ok_and(|p| p == output) {
        return Err(format!(
            "report would overwrite an input: {}",
            input.display()
        ));
    }
    Ok(())
}

struct SessionSettings {
    samples: usize,
    edit_rounds: usize,
    soak_cycles: usize,
    timeout: Duration,
    idle: Duration,
}

fn setting(args: &Args, name: &str, default: usize, max: usize) -> Result<usize, String> {
    let n = args.number(name, default)?;
    if n > max {
        return Err(format!("{name} exceeds {max}"));
    }
    Ok(n)
}

fn nonnegative(args: &Args, name: &str) -> Result<Option<f64>, String> {
    args.get(name)
        .map(|s| {
            s.parse::<f64>()
                .ok()
                .filter(|v| v.is_finite() && *v >= 0.0)
                .ok_or_else(|| format!("{name} must be finite and nonnegative"))
        })
        .transpose()
}

pub fn execute(arguments: &[String]) -> Result<String, String> {
    let mut options = vec![
        "--root",
        "--repo",
        "--server",
        "--output",
        "--baseline",
        "--fail-over",
        "--min-delta-ms",
        "--sessions",
    ];
    options.extend_from_slice(WORKLOAD_OPTIONS);
    let args = Args::parse(arguments, &options, &[])?;
    if args.help() {
        return Ok(HELP.into());
    }
    if !args.positional.is_empty() || !args.forwarded.is_empty() {
        return Err(HELP.into());
    }
    let root = args.root()?;
    let files = setting(&args, "--files", 32, 20_000)?;
    let entries = setting(&args, "--document-entries", 32, 10_000)?;
    let samples = setting(&args, "--samples", 5, 1000)?;
    let edit_rounds = setting(&args, "--edit-rounds", 5, 1000)?;
    let soak_cycles = args
        .get("--soak-cycles")
        .unwrap_or("0")
        .parse::<usize>()
        .ok()
        .filter(|n| *n <= 1000)
        .ok_or("--soak-cycles must be in 0..=1000")?;
    let sessions = setting(&args, "--sessions", 3, 100)?;
    let timeout = Duration::from_millis(setting(&args, "--timeout-ms", 60_000, 600_000)? as u64);
    let idle = Duration::from_millis(setting(&args, "--idle-ms", 250, 60_000)? as u64);
    let threshold = nonnegative(&args, "--fail-over")?;
    let floor = nonnegative(&args, "--min-delta-ms")?.unwrap_or(1.0);
    let baseline = args.path("--baseline");
    if baseline.is_none() && (threshold.is_some() || args.get("--min-delta-ms").is_some()) {
        return Err("suite threshold options require --baseline".into());
    }
    let output = report::output(
        &root,
        &args
            .path("--output")
            .unwrap_or_else(|| root.join("target/performance-results/suite.json")),
        "performance-results",
    )?;
    for input in [
        baseline.as_deref(),
        args.get("--cache").map(Path::new),
        args.get("--server").map(Path::new),
    ]
    .into_iter()
    .flatten()
    {
        distinct_report(&output, input)?;
    }
    let baseline = baseline.as_deref().map(report::json).transpose()?;
    let binary = match args.get("--server") {
        Some(path) => process::server(&root, Some(path))?,
        None => process::build_server(&root, true)?,
    };
    let cache = args.path("--cache");
    let settings = SessionSettings {
        samples,
        edit_rounds,
        soak_cycles,
        timeout,
        idle,
    };
    let mut workload = json!({"version":2,"dependency_files":files,"extra_events":entries,"sessions":sessions,
        "edit_rounds":edit_rounds,"soak_cycles":soak_cycles,
        "samples_per_session":samples,"timeout_ms":timeout.as_millis(),"idle_ms":idle.as_millis(),
        "platform":std::env::consts::OS,"arch":std::env::consts::ARCH,"vanilla_sha256":null});
    let mut result = json!({"schema_version":SCHEMA,"status":"running","server":{"binary":binary,"sha256":report::file_hash(&binary)?},"workload":workload,"sessions":[]});
    let mut metrics = Metrics::new();
    report::write(&output, &result)?;
    let run = (|| -> Result<(), String> {
        for index in 0..sessions {
            let f = Fixture::new(files, entries, cache.as_deref())?;
            if index > 0 && workload["vanilla_sha256"] != json!(f.vanilla_digest) {
                return Err("Vanilla cache changed between sessions".into());
            }
            workload["vanilla_sha256"] = json!(f.vanilla_digest);
            workload["fixture_sha256"] = json!(f.digest);
            workload["caller_bytes"] = json!(
                f.callers
                    .iter()
                    .map(|(_, text)| text.len())
                    .collect::<Vec<_>>()
            );
            result["workload"] = workload.clone();
            if let Some(base) = &baseline {
                metrics::check_workload(base, &workload)?;
            }
            let mut session_log = json!({"session":index+1,"cancellations":[],"resources":{}});
            result["sessions"]
                .as_array_mut()
                .unwrap()
                .push(session_log.clone());
            report::write(&output, &result)?;
            let outcome = run_session(
                &binary,
                &f,
                &mut metrics,
                &mut session_log,
                &settings,
                &mut |log| {
                    result["sessions"][index] = log.clone();
                    report::write(&output, &result)
                },
            );
            result["sessions"][index] = session_log;
            outcome?;
            eprintln!(
                "suite session {} completed ({} latency series)",
                index + 1,
                metrics.len()
            );
        }
        if let Some(base) = &baseline {
            let mut comparison = metrics::compare(base, &workload, &metrics, threshold, floor)?;
            comparison["resources"] = metrics::resource_changes(
                &base["resource_summary"],
                &metrics::resources(result["sessions"].as_array().unwrap()),
            );
            let regressed = !comparison["regressions"].as_array().unwrap().is_empty();
            result["comparison"] = comparison;
            if regressed {
                return Err("suite regression threshold exceeded".into());
            }
        }
        Ok(())
    })();
    result["metrics"] = json!(metrics);
    result["resource_summary"] = metrics::resources(result["sessions"].as_array().unwrap());
    result["summary"] = json!(metrics::summaries(&metrics)?);
    result["status"] = json!(if run.is_ok() { "passed" } else { "failed" });
    if let Err(error) = &run {
        result["error"] = json!(error);
    }
    report::write(&output, &result)?;
    run.map_err(|e| format!("{e}; {}", output.display()))?;
    Ok(format!(
        "{} latency series; {}",
        metrics.len(),
        output.display()
    ))
}

fn run_session(
    binary: &Path,
    f: &Fixture,
    metrics: &mut Metrics,
    log: &mut Value,
    settings: &SessionSettings,
    checkpoint: &mut impl FnMut(&Value) -> Result<(), String>,
) -> Result<(), String> {
    let timeout = settings.timeout;
    // First process builds the dependency index, second installs it from the same private profile.
    for mode in ["fresh", "restart"] {
        if f.dependency_index.is_file() != (mode == "restart") {
            return Err(format!("{mode}: unexpected dependency cache state"));
        }
        let start = Instant::now();
        let mut client = Client::spawn_command(f.command(binary), &f.workspace, timeout)?;
        let mut sampler = sampler::Sampler::start(client.pid(), Duration::from_millis(500));
        let init = client.handshake(&f.workspace, f.options.clone())?;
        record(
            metrics,
            &format!("startup/{mode}-initialize"),
            start.elapsed(),
            None,
        );
        client.wait_notification("pdc/ready", |_| true)?;
        record(
            metrics,
            &format!("startup/{mode}-ready"),
            start.elapsed(),
            None,
        );
        log["server_version"] = init["serverInfo"]["version"].clone();
        log["at_ready"][mode] = process_sample(client.pid());
        let mut session = Session {
            client,
            measurements: metrics,
            timeout,
            verified_methods: Default::default(),
        };
        let inventory = session.query(
            &format!("startup/{mode}-inventory"),
            "pdc/workspaceFiles",
            Value::Null,
            Expect::Populated("/files"),
        )?;
        let roots = inventory["roots"]
            .as_array()
            .ok_or("workspace inventory lacks roots")?;
        let dep = roots
            .iter()
            .find(|r| r["kind"] == "dependency")
            .ok_or("missing dependency root")?;
        let files = inventory["files"]
            .as_array()
            .ok_or("invalid file inventory")?;
        if files
            .iter()
            .filter(|file| file["rootId"] == dep["id"])
            .count()
            != f.dependency_files
        {
            return Err(format!("{mode}: partial dependency inventory"));
        }
        if let Some(expected) = &f.vanilla_digest {
            let vanilla = roots
                .iter()
                .find(|r| r["kind"] == "vanilla")
                .ok_or("missing Vanilla root")?;
            if !files.iter().any(|file| file["rootId"] == vanilla["id"]) {
                return Err(format!("{mode}: empty Vanilla inventory"));
            }
            let copy = Path::new(f.options["vanillaIndexCache"].as_str().unwrap());
            if report::file_hash(copy)? != *expected {
                return Err("Vanilla cache was regenerated; use a compatible cache for the existing-cache workload".into());
            }
        }
        session.query(
            &format!("startup/{mode}-dependency-lookup"),
            "workspace/symbol",
            json!({"query":"dependency.0"}),
            Expect::Populated(""),
        )?;
        if mode == "restart" {
            exercise(&mut session, f, log, settings, checkpoint)?;
        }
        let shutting_down = Instant::now();
        session.client.shutdown()?;
        record(
            session.measurements,
            &format!("shutdown/{mode}"),
            shutting_down.elapsed(),
            None,
        );
        log["resources"][mode] = sampler.finish();
        log["verified_request_methods"][mode] = json!(session.verified_methods);
    }
    Ok(())
}

fn exercise(
    s: &mut Session<'_>,
    f: &Fixture,
    log: &mut Value,
    settings: &SessionSettings,
    checkpoint: &mut impl FnMut(&Value) -> Result<(), String>,
) -> Result<(), String> {
    let samples = settings.samples;
    let idle = settings.idle;
    let pid = s.client.pid();
    let first = sampler::sample(pid);
    let started = Instant::now();
    std::thread::sleep(idle);
    let last = sampler::sample(pid);
    let window = started.elapsed().as_secs_f64();
    log["idle"] = match (first, last) {
        (Ok(a), Ok(b)) => {
            json!({"observed_window_ms":window*1000.0,"cpu_seconds":b["cpu_seconds"].as_f64().zip(a["cpu_seconds"].as_f64()).map(|(end,start)|end-start),"working_set_bytes":b["working_set_bytes"]})
        }
        (a, b) => json!({"error":format!("idle sampling: {:?}, {:?}",a.err(),b.err())}),
    };
    s.open_documents(f)?;
    s.queries(f, "first")?;
    for _ in 0..samples {
        s.queries(f, "repeat")?;
        s.foreground_burst(f)?;
    }
    s.edit_diagnostics(f)?;
    rounds::exercise(s, f, log, settings, checkpoint)?;
    log["after_queries_and_edits"] = process_sample(pid);
    let mut version = 1;
    let mut previous = "add_prestige".to_owned();
    for _ in 0..samples {
        // Keep both prefixes represented even in a one-sample smoke run.
        for _ in 0..2 {
            let observation = s.typing(f, &mut version, &mut previous, false)?;
            log["cancellations"]
                .as_array_mut()
                .unwrap()
                .push(observation);
        }
    }
    log["reindex_during_edit"] = s.typing(f, &mut version, &mut previous, true)?;
    s.configuration_and_close(f, version + 1)?;
    s.client.close(&f.workspace.join(TERRAIN_PATH))?;
    s.client.close(&f.workspace.join(fixture::GFX_PATH))?;
    s.client.close(&f.workspace.join(fixture::LOC_PATH))?;
    s.client.close(&f.workspace.join(fixture::EFFECTS_PATH))?;
    s.watched_file(f)?;
    log["after_close_and_watch"] = process_sample(pid);
    for (name, cmd, field) in [
        ("reindex", "pdc/reindexWorkspace", "/sourceFiles"),
        ("validation", "validateWorkspace", "/validatedFiles"),
        ("format", "pdc/formatWorkspace", "/formattedFiles"),
    ] {
        s.query(
            &format!("workspace/{name}"),
            "workspace/executeCommand",
            json!({"command":cmd,"arguments":[]}),
            Expect::PositiveCount(field),
        )?;
    }
    Ok(())
}
