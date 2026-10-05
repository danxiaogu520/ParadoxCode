//! Portable benchmark/baseline workflows and explicit platform tool adapters.
use crate::{args::Args, process, report};

use serde_json::{Value, json};

use std::collections::{BTreeMap, BTreeSet};

use std::fs;

use std::path::{Path, PathBuf};

use std::process::Stdio;

use std::time::{Instant, SystemTime, UNIX_EPOCH};

struct Config {
    root: PathBuf,
    source: PathBuf,
    cache: PathBuf,
    local: toml::Value,
}

impl Config {
    fn new(args: &Args) -> Result<Self, String> {
        let root = args.root()?;

        let config = args
            .path("--config")
            .unwrap_or_else(|| root.join("target/perf/config.local.toml"));

        let local = if config.is_file() {
            fs::read_to_string(config)
                .map_err(|e| e.to_string())?
                .parse::<toml::Value>()
                .map_err(|e| e.to_string())?
        } else {
            toml::Value::Table(Default::default())
        };

        let source = args
            .path("--source")
            .or_else(|| args.path("--vanilla-source"))
            .or_else(|| std::env::var_os("PDC_VANILLA_SOURCE").map(PathBuf::from))
            .or_else(|| {
                local
                    .get("source")
                    .and_then(toml::Value::as_str)
                    .map(PathBuf::from)
            })
            .unwrap_or_else(|| root.join("data/vanilla"));

        let cache = args
            .path("--cache")
            .or_else(|| args.path("--vanilla-cache"))
            .or_else(|| {
                local
                    .get("cache")
                    .and_then(toml::Value::as_str)
                    .map(PathBuf::from)
            })
            .unwrap_or_else(|| root.join("target/perf/cache/eu4.pdcindex"));

        Ok(Self {
            root,
            source,
            cache,
            local,
        })
    }

    fn setting(&self, key: &str, env: &str) -> Option<String> {
        std::env::var(env)
            .ok()
            .filter(|s| !s.is_empty())
            .or_else(|| {
                self.local
                    .get(key)
                    .and_then(toml::Value::as_str)
                    .map(str::to_owned)
            })
    }

    fn run_dir(&self, kind: &str) -> Result<PathBuf, String> {
        let time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos();

        let path = self
            .root
            .join("target/perf/runs")
            .join(format!("{time}-{kind}-{}", std::process::id()));

        fs::create_dir_all(&path).map_err(|e| e.to_string())?;

        Ok(path)
    }
}

fn safe_name(name: &str) -> Result<(), String> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
    {
        return Err("name must use letters, numbers, dots, underscores or hyphens".into());
    }

    Ok(())
}

/// Parse Cargo's human-readable output without letting terminal styling become
/// part of target identities or metrics. Keep the original log untouched.
fn bench_metrics(text: &str) -> Result<BTreeMap<String, f64>, String> {
    let ansi = regex::Regex::new(r"\x1b\[[0-?]*[ -/]*[@-~]").unwrap();
    let text = ansi.replace_all(text, "");
    let pattern = regex::Regex::new(r"^\s*(.+?):\s*([0-9]+(?:\.[0-9]+)?) ms\s*$").unwrap();
    let bench_name =
        regex::Regex::new(r"^Running\s+(?:.*[/\\])?benches[/\\]([^/\\]+)\.rs\s+\(").unwrap();
    let mut name = None;
    let mut metrics = BTreeMap::new();

    for line in text.lines().map(str::trim) {
        if line.starts_with("Running ") || line.starts_with("Running\t") {
            // A different Cargo target must not inherit the preceding bench's
            // identity if its header is unrecognized (for example a unit test).
            name = bench_name.captures(line).map(|c| c[1].to_owned());
        }

        if let Some(c) = pattern.captures(line) {
            let name = name
                .as_ref()
                .ok_or("benchmark metric has no target identity")?;
            let key = format!("{name}/{}", c[1].trim());
            let value = c[2].parse::<f64>().map_err(|e| e.to_string())?;
            if metrics.insert(key.clone(), value).is_some() {
                return Err(format!("duplicate benchmark metric {key}"));
            }
        }
    }

    Ok(metrics)
}

fn bench(config: &Config, args: &Args, output: &Path) -> Result<Value, String> {
    fs::create_dir_all(output).map_err(|e| e.to_string())?;

    let repeat = args.number("--repeat", 1)?;

    let cargo_args = if args.forwarded.is_empty() {
        vec!["--locked", "--workspace", "--all-features", "--benches"]
            .into_iter()
            .map(str::to_owned)
            .collect()
    } else {
        args.forwarded.clone()
    };

    let mut metrics = BTreeMap::<String, Vec<f64>>::new();

    for pass in 0..=repeat {
        let path = output.join(if pass == 0 {
            "bench-warmup.txt".into()
        } else {
            format!("bench-pass-{pass}.txt")
        });

        let file = fs::File::create(&path).map_err(|e| e.to_string())?;

        let status = process::command("cargo")
            .arg("bench")
            .args(&cargo_args)
            .current_dir(&config.root)
            .stdout(Stdio::from(file.try_clone().map_err(|e| e.to_string())?))
            .stderr(Stdio::from(file))
            .status()
            .map_err(|e| e.to_string())?;

        let text = fs::read_to_string(&path).map_err(|e| e.to_string())?;

        if !status.success() {
            return Err(format!("cargo bench failed ({status}); {}", path.display()));
        }

        if pass > 0 {
            let measured =
                bench_metrics(&text).map_err(|error| format!("{error}; {}", path.display()))?;

            eprintln!("benchmark pass {pass}/{repeat}: {} metrics", measured.len());
            for (key, value) in measured {
                metrics.entry(key).or_default().push(value);
            }
        }
    }

    if metrics.is_empty() || metrics.values().any(|values| values.len() != repeat) {
        return Err("benchmark metrics are empty or inconsistent across repetitions".into());
    }

    let medians = metrics
        .iter()
        .map(|(key, values)| Ok((key.clone(), super::median(values)?)))
        .collect::<Result<BTreeMap<_, _>, String>>()?;

    let result = json!({
    "status":"measured-review-required","repeat":repeat,"arguments":cargo_args,"metrics_ms":medians,"runs_ms":metrics}
    );

    report::write(&output.join("bench.json"), &result)?;

    let tsv = medians
        .iter()
        .map(|(key, value)| {
            format!(
                "{}\t{}\t{value}",
                key.split_once('/').unwrap().0,
                key.split_once('/').unwrap().1
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";

    fs::write(output.join("bench.tsv"), tsv).map_err(|e| e.to_string())?;

    Ok(result)
}

fn build_server(config: &Config) -> Result<PathBuf, String> {
    process::cargo(
        &config.root,
        &[
            "build",
            "--locked",
            "--release",
            "-p",
            "pdc",
            "--bin",
            "paradoxcode",
        ],
    )?;

    Ok(config.root.join("target/release").join(if cfg!(windows) {
        "paradoxcode.exe"
    } else {
        "paradoxcode"
    }))
}

fn sweep(
    config: &Config,
    args: &Args,
    output: &Path,
    server: Option<&Path>,
) -> Result<Value, String> {
    if !config.source.is_dir() {
        return Err(format!(
            "corpus source missing: {}",
            config.source.display()
        ));
    }

    fs::create_dir_all(output).map_err(|e| e.to_string())?;

    let cache = if args.flag("--cold") {
        output.join(format!(
            "cold-vanilla-{}-{}.pdcindex",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|e| e.to_string())?
                .as_nanos(),
            std::process::id()
        ))
    } else {
        config.cache.clone()
    };

    let cache_started = Instant::now();

    let cache_build = !cache.exists();

    if cache_build {
        crate::cli::execute(&[
            "index".into(),
            "--source".into(),
            config.source.display().to_string(),
            "--output".into(),
            cache.display().to_string(),
        ])
        .map_err(|e| e.to_string())?;
    }

    let cache_build_ms = cache_build.then(|| cache_started.elapsed().as_secs_f64() * 1000.0);

    let binary = server.map(Path::to_owned).map(Ok).unwrap_or_else(|| {
        if args.get("--server").is_some() {
            process::server(&config.root, args.get("--server"))
        } else {
            build_server(config)
        }
    })?;

    let mut command = vec![
        "--repo".into(),
        config.root.display().to_string(),
        "--server".into(),
        binary.display().to_string(),
        "--vanilla-source".into(),
        config.source.display().to_string(),
        "--vanilla-cache".into(),
        cache.display().to_string(),
        "--output".into(),
        output.display().to_string(),
        "--fail-on".into(),
        "none".into(),
    ];

    if let Some(timeout) = args.get("--timeout-ms") {
        command.extend(["--timeout-ms".into(), timeout.into()]);
    }

    for key in [
        "--label",
        "--previous",
        "--query-samples",
        "--memory-interval-ms",
        "--batch-size",
        "--concurrency",
    ] {
        if let Some(value) = args.get(key) {
            command.extend([key.into(), value.into()]);
        }
    }

    crate::audit::client::execute("sweep", &command)?;

    let mut summary = report::json(&output.join("sweep-summary.json"))?;

    summary["cache_protocol"] = json!(if cache_build {
        "native-fresh-cache-build-then-server-load"
    } else {
        "existing-cache-server-load"
    });

    if let Some(ms) = cache_build_ms {
        summary["phases"]["cache_build_ms"] = json!(ms);

        summary["phases"]["total_ms"] =
            json!(summary["phases"]["total_ms"].as_f64().unwrap_or(0.0) + ms);
    }

    report::write(&output.join("sweep-summary.json"), &summary)?;

    Ok(summary)
}

fn metrics(path: &Path) -> Result<BTreeMap<String, f64>, String> {
    if path.join("bench.json").is_file() {
        return serde_json::from_value(
            report::json(&path.join("bench.json"))?["metrics_ms"].clone(),
        )
        .map_err(|e| e.to_string());
    }

    let text = fs::read_to_string(path.join("bench.tsv")).map_err(|e| e.to_string())?;

    let mut result = BTreeMap::new();

    for line in text.lines() {
        let parts = line.split('\t').collect::<Vec<_>>();

        if parts.len() != 3 {
            return Err("invalid historical bench TSV".into());
        }

        result.insert(
            format!("{}/{}", parts[0], parts[1]),
            parts[2].parse().map_err(|_| "invalid metric")?,
        );
    }

    Ok(result)
}

fn ab(config: &Config, args: &Args) -> Result<String, String> {
    let name = args
        .positional
        .first()
        .cloned()
        .or_else(|| {
            fs::read_to_string(config.root.join("target/perf/baselines/current-name"))
                .ok()
                .map(|s| s.trim().into())
        })
        .ok_or("select a baseline name")?;

    safe_name(&name)?;

    let before = config.root.join("target/perf/baselines").join(&name);

    if !before.is_dir() {
        return Err("baseline does not exist".into());
    }

    let output = config.run_dir("ab")?;

    let mut changes = Vec::new();

    let mut failed = false;

    let threshold = args
        .get("--fail-over")
        .map(|s| s.parse::<f64>().map_err(|e| e.to_string()))
        .transpose()?;

    let noise = args
        .get("--noise-pct")
        .unwrap_or("3")
        .parse::<f64>()
        .map_err(|e| e.to_string())?;

    if !noise.is_finite() || noise < 0.0 || threshold.is_some_and(|t| !t.is_finite() || t < 0.0) {
        return Err("comparison thresholds must be finite and nonnegative".into());
    }

    if args.flag("--bench-only") && args.flag("--sweep-only") {
        return Err("select only one of --bench-only and --sweep-only".into());
    }

    if !args.flag("--sweep-only") && before.join("bench.tsv").exists() {
        bench(config, args, &output)?;

        let old = metrics(&before)?;

        let new = metrics(&output)?;

        if old.keys().ne(new.keys()) {
            return Err("benchmark metric inventories differ".into());
        }

        for (key, left) in old {
            let right = new[&key];

            let delta = (left > 0.0).then(|| (right - left) / left * 100.0);

            failed |= threshold
                .is_some_and(|threshold| delta.is_some_and(|d| d > threshold && d.abs() > noise));

            changes.push(json!({
"metric":key,"before":left,"after":right,"delta_pct":delta,"within_noise":delta.is_some_and(|d|d.abs()<=noise)}
));
        }
    }

    let mut workload = Value::Null;

    if !args.flag("--bench-only") && before.join("sweep-summary.json").exists() {
        let old = report::json(&before.join("sweep-summary.json"))?;

        let new = sweep(config, args, &output.join("sweep"), None)?;

        if old["inputs"]["source"] != new["inputs"]["source"] {
            return Err("baseline/candidate corpus roots differ".into());
        }

        workload = json!({
        "before":old["diagnostics_fingerprint"],"after":new["diagnostics_fingerprint"],"identical_diagnostics":old["diagnostics_fingerprint"]==new["diagnostics_fingerprint"]}
        );

        for key in [
            "scan_ms",
            "session_boot_ms",
            "classify_ms",
            "diagnose_ms",
            "query_samples_ms",
            "total_ms",
        ] {
            let (Some(left), Some(right)) =
                (old["phases"][key].as_f64(), new["phases"][key].as_f64())
            else {
                continue;
            };

            let delta = (left > 0.0).then(|| (right - left) / left * 100.0);

            failed |= threshold.is_some_and(|t| delta.is_some_and(|d| d > t && d.abs() > noise));

            changes.push(json!({
            "metric":format!("sweep/{key}"),"before":left,"after":right,"delta_pct":delta}
            ));
        }
    }

    if changes.is_empty() {
        return Err("selected baseline has no matching benchmark or sweep measurements".into());
    }

    let result = json!({
    "status":if failed{
    "failed-performance-threshold"}
    else{
    "measured-review-required"}
    ,"baseline":name,"changes":changes,"diagnostic_workload":workload,"noise_pct":noise,"fail_over_pct":threshold}
    );

    report::write(&output.join("comparison.json"), &result)?;

    if failed {
        return Err(format!(
            "performance threshold exceeded; {}",
            output.display()
        ));
    }

    Ok(format!(
        "A/B exported; semantic review remains required: {}",
        output.display()
    ))
}

fn import(config: &Config, args: &Args) -> Result<String, String> {
    let what = args.positional.first().map(String::as_str).unwrap_or("all");

    if !["vanilla", "edg", "all"].contains(&what) {
        return Err("import-corpus vanilla|edg|all".into());
    }

    let extensions = [
        "txt", "gui", "gfx", "asset", "sfx", "json", "lua", "yml", "yaml", "mod",
    ];

    let mut records = Vec::new();

    for kind in ["vanilla", "edg"]
        .into_iter()
        .filter(|kind| what == "all" || what == *kind)
    {
        let env = if kind == "vanilla" {
            "PDC_VANILLA_ORIGIN"
        } else {
            "EDG_WORKSHOP_SOURCE"
        };

        let setting = if kind == "vanilla" {
            "vanilla_origin"
        } else {
            "edg_origin"
        };

        let source = PathBuf::from(
            config
                .setting(setting, env)
                .ok_or_else(|| format!("configure {setting} or {env}"))?,
        )
        .canonicalize()
        .map_err(|e| e.to_string())?;

        let destination = config.root.join("data").join(kind);

        if destination.exists() && destination.canonicalize().map_err(|e| e.to_string())? == source
        {
            return Err("corpus source and destination must differ".into());
        }

        fs::create_dir_all(&destination).map_err(|e| e.to_string())?;

        let mut copied = BTreeSet::new();

        let mut bytes = 0;

        for path in report::files(&source)?.into_iter().filter(|p| {
            p.extension()
                .and_then(|s| s.to_str())
                .is_some_and(|s| extensions.contains(&s.to_ascii_lowercase().as_str()))
        }) {
            let relative = path.strip_prefix(&source).map_err(|e| e.to_string())?;

            let target = destination.join(relative);

            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }

            bytes += fs::copy(&path, &target).map_err(|e| e.to_string())?;

            copied.insert(relative.to_owned());
        }

        if args.flag("--prune") {
            for path in report::files(&destination)? {
                if !copied.contains(path.strip_prefix(&destination).unwrap()) {
                    fs::remove_file(path).map_err(|e| e.to_string())?;
                }
            }
        }

        records.push(json!({
        "name":kind,"source":source,"destination":destination,"files":copied.len(),"bytes":bytes}
        ));
    }

    let output = config.root.join("target/perf/corpus-import.json");

    report::write(&output, &json!(records))?;

    Ok(format!(
        "Imported local corpus inputs: {}",
        output.display()
    ))
}

fn control(config: &Config, args: &Args) -> Result<String, String> {
    let binary = args
        .path("--binary")
        .or_else(|| {
            config
                .setting("control_binary", "CWTOOLS_NATIVE_BIN")
                .map(PathBuf::from)
        })
        .unwrap_or_else(|| {
            config.root.join("target/perf/bin").join(if cfg!(windows) {
                "cwtools.exe"
            } else {
                "cwtools"
            })
        });

    if args.flag("--build") || !binary.is_file() {
        let repo = PathBuf::from(
            config
                .setting("control_repo", "CWTOOLS_REPO")
                .ok_or("configure control_repo/CWTOOLS_REPO")?,
        );

        process::run(
            process::command("cargo")
                .args(["build", "--locked", "--release", "--manifest-path"])
                .arg(repo.join("Cargo.toml"))
                .args(["-p", "cwtools_cli"])
                .env(
                    "CARGO_TARGET_DIR",
                    config.root.join("target/perf/build/cwtools"),
                ),
        )?;

        if let Some(parent) = binary.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }

        fs::copy(
            config
                .root
                .join("target/perf/build/cwtools/release")
                .join(if cfg!(windows) {
                    "cwtools.exe"
                } else {
                    "cwtools"
                }),
            &binary,
        )
        .map_err(|e| e.to_string())?;
    }

    let rules = config
        .setting("control_rules", "CWTOOLS_RULES")
        .ok_or("configure control_rules/CWTOOLS_RULES")?;

    let output = config.run_dir("control")?;

    let warmup = args
        .get("--warmup")
        .unwrap_or("1")
        .parse::<usize>()
        .map_err(|e| e.to_string())?;

    let runs = args.number("--runs", 5)?;

    let mut times = Vec::new();

    for i in 0..warmup + runs {
        let start = Instant::now();

        let result = process::command(&binary)
            .args([
                "validate",
                "--game",
                "eu4",
                "-q",
                "--report-type",
                "json",
                "--rules",
                &rules,
                "--directory",
            ])
            .arg(&config.source)
            .arg("--output-file")
            .arg(output.join("report.json"))
            .env("CWTOOLS_TIMINGS", "1")
            .output()
            .map_err(|e| e.to_string())?;

        fs::write(output.join(format!("run-{i}.log")), &result.stderr)
            .map_err(|e| e.to_string())?;

        if !matches!(result.status.code(), Some(0 | 1)) {
            return Err(format!("control runtime failed: {}", result.status));
        }

        if i >= warmup {
            times.push(start.elapsed().as_secs_f64());
        }

        if args.flag("--timings-only") {
            break;
        }
    }

    let result = json!({
    "status":"measured-review-required","binary":binary,"binary_sha256":report::file_hash(&binary)?,"source":config.source,"rules":rules,"warmup":warmup,"seconds":times,"median_seconds":if times.is_empty(){
    None}
    else{
    Some(super::median(&times)?)}
    ,"comparison_note":"Native control has different rules and analysis tasks; throughput is a scale reference"}
    );

    report::write(&output.join("control.json"), &result)?;

    Ok(output.display().to_string())
}

pub fn execute(name: &str, arguments: &[String]) -> Result<String, String> {
    let args = Args::parse(
        arguments,
        &[
            "--root",
            "--repo",
            "--config",
            "--source",
            "--vanilla-source",
            "--cache",
            "--vanilla-cache",
            "--server",
            "--output",
            "--repeat",
            "--fail-over",
            "--noise-pct",
            "--timeout-ms",
            "--flavor",
            "--binary",
            "--runs",
            "--warmup",
            "--label",
            "--previous",
            "--query-samples",
            "--memory-interval-ms",
            "--batch-size",
            "--concurrency",
        ],
        &[
            "--skip-bench",
            "--skip-sweep",
            "--force",
            "--bench-only",
            "--sweep-only",
            "--cold",
            "--build",
            "--timings-only",
            "--prune",
            "--install",
        ],
    )?;

    if args.help() {
        return Ok(super::HELP.into());
    }

    let config = Config::new(&args)?;

    match name{

        "sweep"=>{
let output=report::output(&config.root,&args.path("--output").unwrap_or(config.root.join("target/performance-results/sweep")),"performance-results")?;
sweep(&config,&args,&output,None)?;
Ok(output.display().to_string())}
,
        "bench"=>{
let output=args.path("--output").map(|p|report::output(&config.root,&p,"perf")).transpose()?.unwrap_or(config.run_dir("bench")?);
bench(&config,&args,&output)?;
Ok(output.display().to_string())}

        "baseline"=>{
let name=args.positional.first().ok_or("baseline NAME is required")?;
safe_name(name)?;
let output=config.root.join("target/perf/baselines").join(name);
if output.exists(){
if !args.flag("--force"){
return Err("baseline already exists; use --force to replace".into());
}
fs::remove_dir_all(&output).map_err(|e|e.to_string())?;
}
fs::create_dir_all(&output).map_err(|e|e.to_string())?;
let binary=build_server(&config)?;
let frozen=output.join(binary.file_name().ok_or("invalid binary")?);
fs::copy(&binary,&frozen).map_err(|e|e.to_string())?;
report::write(&output.join("metadata.json"),&json!({
"label":name,"source":config.source,"binary_sha256":report::file_hash(&frozen)?,"version":env!("CARGO_PKG_VERSION")}
))?;
if !args.flag("--skip-bench"){
bench(&config,&args,&output)?;
}

if !args.flag("--skip-sweep"){
let sweep=sweep(&config,&args,&output.join("sweep"),Some(&frozen))?;
report::write(&output.join("sweep-summary.json"),&sweep)?;
}
fs::write(config.root.join("target/perf/baselines/current-name"),name).map_err(|e|e.to_string())?;
Ok(output.display().to_string())}

        "ab"=>ab(&config,&args),
        "status"=>Ok(serde_json::to_string_pretty(&json!({
"source":config.source,"source_exists":config.source.is_dir(),"cache":config.cache,"cache_exists":config.cache.is_file(),"current_baseline":fs::read_to_string(config.root.join("target/perf/baselines/current-name")).ok(),"configuration":config.local}
)).map_err(|e|e.to_string())?),
        "init"=>{
let missing=["cargo","node","git"].into_iter().filter(|program|process::command(program).arg("--version").output().is_err()).collect::<Vec<_>>();
if args.flag("--install"){
if !cfg!(target_os="linux"){
return Err("automatic package installation is supported only by the Linux apt adapter".into());
}
process::run(process::command("sudo").args(["apt-get","install","-y","time","linux-tools-generic"]))?;
}

if !missing.is_empty(){
return Err(format!("missing prerequisite tools: {}",missing.join(", ")));
}
Ok("Required toolchains found; optional profiles use samply/perf and exact Unix memory uses /usr/bin/time".into())}

        "import-corpus"=>import(&config,&args),"control"=>control(&config,&args),
        "profile"=>{
let target=args.positional.first().ok_or("profile bench:NAME|sweep is required")?;
let flavor=args.get("--flavor").unwrap_or("samply");
let output=config.run_dir("profile")?;
let mut command=process::command(flavor);
match flavor{
"samply"=>{
command.args(["record","--save-to"]).arg(output.join("profile.json.gz")).arg("--");
}
,"perf"=>{
command.args(["record","-g","--call-graph","dwarf","-o"]).arg(output.join("perf.data")).arg("--");
}
,_=>return Err("--flavor must be samply or perf".into())}
;
if let Some(bench)=target.strip_prefix("bench:"){
safe_name(bench)?;
let package=if ["index_cache","synthetic_workspace"].contains(&bench){
"engine"}
else{
"ide"}
;
command.args(["cargo","bench","--locked","-p",package,"--bench",bench]);
}
else if target=="sweep"{
command.arg(std::env::current_exe().map_err(|e|e.to_string())?).args(["audit","sweep"]).args(&args.forwarded);
}
else{
return Err("profile target must be bench:NAME or sweep".into());
}
process::run(command.current_dir(&config.root))?;
Ok(output.display().to_string())}

        _=>Err(format!("unknown perf command: {name}\n{}",super::HELP)),
    }
}

#[cfg(test)]
mod tests {
    use super::bench_metrics;

    #[test]
    fn benchmark_metrics_accept_plain_and_colored_cargo_output() {
        let plain = "     Running benches/index_cache.rs (target/release/deps/index_cache-123)\nload: 1.25 ms\n     Running benches/synthetic_workspace.rs (target/release/deps/synthetic_workspace-456)\nload: 2 ms\n";
        // Cargo colors `Running` separately, placing a reset before ` benches/`.
        let colored = plain
            .replace("Running", "\x1b[1m\x1b[92mRunning\x1b[0m")
            .replace("load:", "\x1b[32mload:\x1b[0m")
            .replace('\n', "\r\n");
        let expected = std::collections::BTreeMap::from([
            ("index_cache/load".to_owned(), 1.25),
            ("synthetic_workspace/load".to_owned(), 2.0),
        ]);
        assert_eq!(bench_metrics(plain).unwrap(), expected);
        assert_eq!(bench_metrics(&colored).unwrap(), expected);
    }

    #[test]
    fn benchmark_metrics_accept_platform_paths_and_spacing() {
        for path in [
            "benches/fixture.rs",
            "benches\\fixture.rs",
            "crates/engine/benches/fixture.rs",
            "C:\\repo with spaces\\benches\\fixture.rs",
        ] {
            let log =
                format!("  Running\t{path} (target/release/deps/fixture-123)\n load: 3.5 ms\n");
            assert_eq!(bench_metrics(&log).unwrap()["fixture/load"], 3.5);
        }
    }

    #[test]
    fn benchmark_metrics_reject_missing_or_stale_target_identity() {
        for log in [
            "load: 1 ms\n",
            "Running benches/fixture.rs (target/release/deps/fixture-123)\nload: 1 ms\nRunning unittests src/lib.rs (target/release/deps/lib-456)\nother: 2 ms\n",
        ] {
            assert_eq!(
                bench_metrics(log).unwrap_err(),
                "benchmark metric has no target identity"
            );
        }
    }

    #[test]
    fn benchmark_metrics_reject_duplicate_metrics() {
        let log = "Running benches/fixture.rs (target/release/deps/fixture-123)\nload: 1 ms\nload: 2 ms\n";
        assert_eq!(
            bench_metrics(log).unwrap_err(),
            "duplicate benchmark metric fixture/load"
        );
    }

    #[test]
    fn benchmark_metrics_do_not_invent_measurements() {
        assert!(bench_metrics("Finished `bench` profile\nRunning benches/fixture.rs (target/release/deps/fixture-123)\n").unwrap().is_empty());
    }
}
