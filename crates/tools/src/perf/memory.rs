//! Exact measured-child peak RSS via the platform time utility (wait4), with phase samples.
use crate::{args::Args, process, report};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::Stdio;
use std::time::Instant;
const PHASES: [&str; 6] = [
    "rules",
    "vanilla",
    "scan-retained",
    "evicted",
    "dropped",
    "end",
];
pub fn measured(binary: &Path, source: &Path, output: &Path) -> Result<Value, String> {
    if !cfg!(unix) {
        return Err("exact peak RSS memory pairs require a Unix time utility; use perf probe for sampled Windows RSS".into());
    }
    let mut command = process::command("/usr/bin/time");
    command
        .arg(if cfg!(target_os = "macos") {
            "-l"
        } else {
            "-v"
        })
        .arg(binary)
        .arg(source)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let started = Instant::now();
    let mut child = command.spawn().map_err(|e| e.to_string())?;
    let parent_pid = child.id();
    let stderr = child.stderr.take().ok_or("missing measured stderr")?;
    let error_reader = std::thread::spawn(move || {
        use std::io::Read;
        let mut text = String::new();
        let mut reader = BufReader::new(stderr);
        reader.read_to_string(&mut text).map(|_| text)
    });
    let mut log = String::new();
    let mut phases = BTreeMap::new();
    let mut errors = Vec::new();
    let mut pid = None;
    let stdout = child.stdout.take().ok_or("missing measured stdout")?;
    for line in BufReader::new(stdout).lines() {
        let line = line.map_err(|e| e.to_string())?;
        log.push_str(&line);
        log.push('\n');
        if let Some(value) = line.strip_prefix("PROBE_PID=") {
            pid = value.parse::<u32>().ok();
        }
        if let Some(phase) = line.strip_prefix("PHASE:") {
            let sample = (|| -> Result<Value, String> {
                let pid = if let Some(pid) = pid {
                    pid
                } else {
                    let children = process::capture(
                        process::command("pgrep").args(["-P", &parent_pid.to_string()]),
                    )?;
                    String::from_utf8_lossy(&children.stdout)
                        .split_whitespace()
                        .next()
                        .ok_or("measured child PID missing")?
                        .parse::<u32>()
                        .map_err(|e| e.to_string())?
                };
                super::sampler::sample(pid)
            })();
            match sample {
                Ok(sample) => {
                    phases.insert(phase.to_owned(), sample["working_set_bytes"].clone());
                }
                Err(error) => errors.push(json!({"phase":phase,"error":error})),
            }
        }
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    let stderr = error_reader
        .join()
        .map_err(|_| "memory stderr reader panicked")?
        .map_err(|e| e.to_string())?;
    log.push_str(&stderr);
    fs::write(output.with_extension("log"), &log).map_err(|e| e.to_string())?;
    let peak_pattern = regex::Regex::new(if cfg!(target_os = "macos") {
        r"(?m)^\s*(\d+)\s+maximum resident set size"
    } else {
        r"Maximum resident set size \(kbytes\):\s*(\d+)"
    })
    .unwrap();
    let peak = peak_pattern
        .captures(&stderr)
        .and_then(|c| c[1].parse::<u64>().ok())
        .ok_or("time utility did not report measured-child peak RSS")?
        * if cfg!(target_os = "macos") { 1 } else { 1024 };
    let diagnostics_pattern = regex::Regex::new(
        r"diagnostics pass: files=(\d+) count=(\d+) total=([\d.]+)s digest=(0x[0-9a-f]+)",
    )
    .unwrap();
    let diagnostics=diagnostics_pattern.captures(&log).map(|c|json!({"files":c[1].parse::<u64>().unwrap(),"count":c[2].parse::<u64>().unwrap(),"seconds":c[3].parse::<f64>().unwrap(),"digest":&c[4]}));
    let missing = PHASES
        .iter()
        .filter(|p| !phases.contains_key(**p))
        .collect::<Vec<_>>();
    let result = json!({"binary":binary,"binary_sha256":report::file_hash(binary)?,"root":source,"exit_code":status.code(),"elapsed_seconds":started.elapsed().as_secs_f64(),"peak_rss_bytes":peak,"phase_rss_bytes":phases,"sampler_errors":errors,"missing_phases":missing,"diagnostics":diagnostics,"method":"platform time utility wait4 peak for measured child; ps phase RSS for that child PID","limitations":"OS allocator retention and memory compression affect RSS; diagnostic workloads can differ"});
    report::write(output, &result)?;
    if !status.success() {
        return Err(format!("measured child failed: {status}"));
    }
    Ok(result)
}
pub fn execute(arguments: &[String]) -> Result<String, String> {
    let args = Args::parse(
        arguments,
        &[
            "--repo", "--before", "--after", "--source", "--root", "--output", "--repeat",
        ],
        &["--require-identical-diagnostics"],
    )?;
    if args.help() {
        return Ok(super::HELP.into());
    }
    let mut repo_args = arguments.to_vec();
    if let Some(index) = repo_args.iter().position(|s| s == "--root") {
        repo_args.drain(index..index + 2);
    }
    let repo = Args::parse(
        &repo_args,
        &[
            "--repo", "--before", "--after", "--source", "--output", "--repeat",
        ],
        &["--require-identical-diagnostics"],
    )?
    .root()?;
    let source = args
        .path("--source")
        .or_else(|| args.path("--root"))
        .unwrap_or_else(|| repo.join("data/vanilla"))
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let output = report::output(
        &repo,
        Path::new(args.required("--output")?),
        "performance-results",
    )?;
    fs::create_dir_all(&output).map_err(|e| e.to_string())?;
    let before = Path::new(args.required("--before")?)
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let after = Path::new(args.required("--after")?)
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let repeat = args.number("--repeat", 3)?;
    let mut runs = BTreeMap::<String, Vec<Value>>::from([
        ("before".into(), Vec::new()),
        ("after".into(), Vec::new()),
    ]);
    for i in 0..repeat {
        for (label, binary) in [("before", &before), ("after", &after)] {
            let result = measured(
                binary,
                &source,
                &output.join(format!("{label}-{}.json", i + 1)),
            )?;
            runs.get_mut(label).unwrap().push(result);
            eprintln!("{label} {} complete", i + 1);
        }
    }
    let mut summary = BTreeMap::new();
    let mut signatures = BTreeSet::new();
    let mut all_signatures = true;
    let mut sampling_complete = true;
    for (label, records) in &runs {
        let median = |key: &str| {
            super::median(
                &records
                    .iter()
                    .filter_map(|r| r[key].as_f64())
                    .collect::<Vec<_>>(),
            )
        };
        let mut phases = BTreeMap::new();
        for phase in PHASES {
            let values = records
                .iter()
                .filter_map(|r| r["phase_rss_bytes"][phase].as_f64())
                .collect::<Vec<_>>();
            if values.len() == records.len() {
                phases.insert(phase, super::median(&values)?);
            }
        }
        let diag_seconds = records
            .iter()
            .filter_map(|r| r["diagnostics"]["seconds"].as_f64())
            .collect::<Vec<_>>();
        summary.insert(label,json!({"peak_rss_bytes_median":median("peak_rss_bytes")?,"elapsed_seconds_median":median("elapsed_seconds")?,"diagnostic_seconds_median":if diag_seconds.len()==records.len(){Some(super::median(&diag_seconds)?)}else{None},"phase_rss_bytes_medians":phases}));
        for record in records {
            if record["diagnostics"].is_null() {
                all_signatures = false;
            } else {
                signatures.insert(
                    json!([
                        record["diagnostics"]["files"],
                        record["diagnostics"]["count"],
                        record["diagnostics"]["digest"]
                    ])
                    .to_string(),
                );
            }
            sampling_complete &= record["missing_phases"]
                .as_array()
                .is_some_and(Vec::is_empty)
                && record["sampler_errors"]
                    .as_array()
                    .is_some_and(Vec::is_empty);
        }
    }
    let identical = all_signatures && signatures.len() == 1;
    let failed = args.flag("--require-identical-diagnostics") && !identical;
    let result = json!({"status":if failed{"failed-diagnostic-comparison"}else{"measured-review-required"},"repeat":repeat,"runs":runs,"summary":summary,"diagnostic_signatures_match":identical,"identical_diagnostics_required":args.flag("--require-identical-diagnostics"),"phase_sampling_complete":sampling_complete,"limitations":"Local sequential measurements; different rule semantics can change the diagnosis workload"});
    report::write(&output.join("comparison.json"), &result)?;
    if failed {
        return Err("missing or differing diagnostic signatures in memory pairs".into());
    }
    serde_json::to_string_pretty(&result["summary"]).map_err(|e| e.to_string())
}
