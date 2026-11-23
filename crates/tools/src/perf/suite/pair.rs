//! Paired comparisons run both binaries on this host with the same measurement client.
use super::{WORKLOAD_OPTIONS, distinct_report, metrics, nonnegative, setting};
use crate::{args::Args, perf::median, process, report};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;

pub(crate) const HELP: &str = "tools perf pair --before BINARY --after BINARY [--pairs 4] [--fail-over PCT] [--min-delta-ms MS] [--enforce] [--output REPORT] [--files N] [--document-entries N] [--edit-rounds N] [--soak-cycles N] [--samples N] [--cache VANILLA] [--timeout-ms N] [--idle-ms N]";
const CRITICAL: &[&str] = &[
    "startup/fresh-ready",
    "startup/restart-ready",
    "round/open-to-first-completion",
    "round/last-edit-to-completion",
    "watch/change-to-visible",
];

fn summarize(observations: &[(f64, f64)], relative: f64, floor: f64) -> Result<Value, String> {
    let mut deltas = Vec::new();
    let mut exceeded = 0;
    for &(before, after) in observations {
        let diff = after - before;
        if diff > floor && diff > before * relative / 100.0 {
            exceeded += 1;
        }
        deltas.push(diff);
    }
    let center = median(&deltas)?;
    let deviations = deltas
        .iter()
        .map(|d| (d - center).abs())
        .collect::<Vec<_>>();
    let required = (3 * observations.len()).div_ceil(4);
    let (before, after): (Vec<_>, Vec<_>) = observations.iter().copied().unzip();
    Ok(
        json!({"pairs":observations.len(),"before_after_ms":observations,"paired_deltas_ms":deltas,
        "median_before_ms":median(&before)?,"median_after_ms":median(&after)?,
        "median_delta_ms":center,"median_absolute_deviation_ms":median(&deviations)?,
        "min_delta_ms":deltas.iter().copied().min_by(f64::total_cmp),"max_delta_ms":deltas.iter().copied().max_by(f64::total_cmp),
        "pairs_exceeding_both_limits":exceeded,"required_pairs":required,"regressed":exceeded>=required}),
    )
}

pub(crate) fn execute(arguments: &[String]) -> Result<String, String> {
    let mut options = vec![
        "--repo",
        "--root",
        "--before",
        "--after",
        "--pairs",
        "--fail-over",
        "--min-delta-ms",
        "--output",
    ];
    options.extend_from_slice(WORKLOAD_OPTIONS);
    let args = Args::parse(arguments, &options, &["--enforce"])?;
    if args.help() {
        return Ok(HELP.into());
    }
    if !args.positional.is_empty() || !args.forwarded.is_empty() {
        return Err(HELP.into());
    }
    let pairs = setting(&args, "--pairs", 4, 100)?;
    if !pairs.is_multiple_of(2) || (args.flag("--enforce") && pairs < 4) {
        return Err("use an even number of pairs; enforcement needs at least four".into());
    }
    let relative = nonnegative(&args, "--fail-over")?.unwrap_or(20.0);
    let floor = nonnegative(&args, "--min-delta-ms")?.unwrap_or(5.0);
    let root = args.root()?;
    let before = process::server(&root, Some(args.required("--before")?))?;
    let after = process::server(&root, Some(args.required("--after")?))?;
    let output = report::output(
        &root,
        &args
            .path("--output")
            .unwrap_or_else(|| root.join("target/performance-results/pair.json")),
        "performance-results",
    )?;
    for input in [
        Some(before.as_path()),
        Some(after.as_path()),
        args.get("--cache").map(Path::new),
    ]
    .into_iter()
    .flatten()
    {
        distinct_report(&output, input)?;
    }
    let parent = output.parent().ok_or("missing report directory")?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let raw = tempfile::Builder::new()
        .prefix("paired-runs-")
        .tempdir_in(parent)
        .map_err(|e| e.to_string())?
        .keep();
    let mut suite_args = vec![
        "--root".into(),
        root.display().to_string(),
        "--sessions".into(),
        "1".into(),
    ];
    for &flag in WORKLOAD_OPTIONS {
        if let Some(value) = args.get(flag) {
            suite_args.extend([flag.to_owned(), value.to_owned()]);
        }
    }
    let before_hash = report::file_hash(&before)?;
    let after_hash = report::file_hash(&after)?;
    let mut summary = json!({"schema_version":1,"status":"running","before_sha256":before_hash,"after_sha256":after_hash,
        "same_binary":before_hash==after_hash,"requested_pairs":pairs,"threshold_pct":relative,"min_delta_ms":floor,
        "enforce":args.flag("--enforce"),"raw_directory":raw.file_name().unwrap().to_string_lossy(),"pairs":[]});
    let run = (|| -> Result<(), String> {
        let mut observations = BTreeMap::new();
        let mut statistics = BTreeMap::new();
        for i in 0..pairs {
            let mut order = [
                ("before", before.as_path(), &before_hash),
                ("after", after.as_path(), &after_hash),
            ];
            if i % 2 == 1 {
                order.reverse();
            }
            summary["pairs"]
                .as_array_mut()
                .unwrap()
                .push(json!({"number":i+1,"order":[order[0].0,order[1].0]}));
            let mut arms = BTreeMap::new();
            for (label, binary, hash) in order {
                let name = format!("pair-{}-{label}.json", i + 1);
                summary["pairs"][i][label] = json!(name);
                report::write(&output, &summary)?;
                let path = raw.join(name);
                eprintln!("performance pair {}/{}: {label}", i + 1, pairs);
                let arm = run_arm(&suite_args, binary, &path)
                    .map_err(|error| format!("pair {} {label}: {error}", i + 1))?;
                if arm["server"]["sha256"] != hash.as_str() {
                    return Err(format!("{label} binary changed during comparison"));
                }
                if summary.get("workload").is_none() {
                    summary["workload"] = arm["workload"].clone();
                }
                metrics::check_workload(&arm, &summary["workload"])?;
                arms.insert(label, arm);
            }
            let base = &arms["before"];
            let candidate = &arms["after"];
            let current =
                serde_json::from_value(candidate["metrics"].clone()).map_err(|e| e.to_string())?;
            let mut all = metrics::compare(base, &candidate["workload"], &current, None, floor)?;
            all["resources"] = metrics::resource_changes(
                &base["resource_summary"],
                &candidate["resource_summary"],
            );
            for &key in CRITICAL {
                let change = &all["changes"][key];
                let value = |field: &str| {
                    change[field]
                        .as_f64()
                        .ok_or_else(|| format!("missing paired measurement {key}/{field}"))
                };
                observations
                    .entry(key)
                    .or_insert_with(Vec::new)
                    .push((value("before_ms")?, value("after_ms")?));
                statistics.insert(
                    key,
                    (change["statistic"].clone(), current[key].samples_ms.len()),
                );
            }
            let detail = format!("pair-{}-comparison.json", i + 1);
            report::write(&raw.join(&detail), &all)?;
            summary["pairs"][i]["comparison"] = json!(detail);
        }
        let mut regressions = Vec::new();
        for (key, pairs) in observations {
            let mut entry = summarize(&pairs, relative, floor)?;
            let (statistic, count) = &statistics[key];
            entry["within_session_statistic"] = statistic.clone();
            entry["samples_per_session"] = json!(count);
            if entry["regressed"] == true {
                regressions.push(key);
            }
            summary["critical"][key] = entry;
        }
        summary["regressions"] = json!(regressions);
        Ok(())
    })();
    if let Err(error) = &run {
        summary["error"] = json!(error);
    }
    let regressed = summary["regressions"]
        .as_array()
        .is_some_and(|v| !v.is_empty());
    summary["status"] = json!(if run.is_err() {
        "failed"
    } else if regressed {
        "regression"
    } else {
        "passed"
    });
    report::write(&output, &summary)?;
    run.map_err(|e| format!("{e}; {}", output.display()))?;
    if regressed && args.flag("--enforce") {
        return Err(format!(
            "paired performance regression; {}",
            output.display()
        ));
    }
    Ok(format!(
        "{} paired sessions per binary, {}: {}",
        pairs,
        summary["status"],
        output.display()
    ))
}

fn run_arm(common: &[String], binary: &Path, output: &Path) -> Result<Value, String> {
    let mut args = common.to_vec();
    args.extend([
        "--server".into(),
        binary.display().to_string(),
        "--output".into(),
        output.display().to_string(),
    ]);
    super::execute(&args)?;
    report::json(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn single_spike_and_small_deltas_do_not_trigger_a_paired_regression() {
        let spike = [(20., 90.), (21., 22.), (19., 18.), (23., 22.)];
        assert_eq!(summarize(&spike, 20., 5.).unwrap()["regressed"], false);
        assert_eq!(
            summarize(&[(1., 2.); 4], 20., 5.).unwrap()["regressed"],
            false
        );
        assert_eq!(
            summarize(&[(100., 110.); 4], 20., 5.).unwrap()["regressed"],
            false
        );
        let sustained = [(20., 34.), (18., 30.), (22., 36.), (21., 19.)];
        let report = summarize(&sustained, 20., 5.).unwrap();
        assert_eq!(report["regressed"], true);
        assert_eq!(report["pairs_exceeding_both_limits"], 3);
    }
}
