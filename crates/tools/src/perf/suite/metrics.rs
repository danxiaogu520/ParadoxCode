use crate::perf::median;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::time::Duration;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Measurements {
    pub samples_ms: Vec<f64>,
    // Cardinality of the validated result for each timed request, when applicable.
    pub result_counts: Vec<usize>,
}

pub type Metrics = BTreeMap<String, Measurements>;

pub fn record(metrics: &mut Metrics, name: &str, elapsed: Duration, count: Option<usize>) {
    let series = metrics.entry(name.to_owned()).or_default();
    series.samples_ms.push(elapsed.as_secs_f64() * 1000.0);
    if let Some(count) = count {
        series.result_counts.push(count);
    }
}

fn percentile(sorted: &[f64], p: usize, minimum: usize) -> Option<f64> {
    (sorted.len() >= minimum).then(|| sorted[(p * sorted.len()).div_ceil(100) - 1])
}

impl Measurements {
    fn stats(&self) -> Result<Value, String> {
        let mut sorted = self.samples_ms.clone();
        let median = median(&sorted)?;
        if sorted.iter().any(|v| *v < 0.0)
            || (!self.result_counts.is_empty() && self.result_counts.len() != sorted.len())
        {
            return Err("invalid suite observations".into());
        }
        sorted.sort_by(f64::total_cmp);
        Ok(
            json!({"n":sorted.len(),"median_ms":median,"p95_ms":percentile(&sorted,95,20),
            "p99_ms":percentile(&sorted,99,100),"max_ms":sorted.last()}),
        )
    }
}

pub fn summaries(metrics: &Metrics) -> Result<BTreeMap<&str, Value>, String> {
    metrics
        .iter()
        .map(|(key, values)| {
            let mut stats = values.stats()?;
            let unit = match key.as_str() {
                "burst/all-responses" => Some("requests_per_second"),
                "workspace/validation" => Some("validated_files_per_second"),
                "workspace/format" => Some("formatted_files_per_second"),
                _ => None,
            };
            if let Some(unit) = unit {
                let ms: f64 = values.samples_ms.iter().sum();
                stats[unit] = json!(
                    (ms > 0.0)
                        .then(|| values.result_counts.iter().sum::<usize>() as f64 * 1000.0 / ms)
                );
            }
            Ok((key.as_str(), stats))
        })
        .collect()
}

/// Keep the observed denominator: short-lived processes can exit between two RSS samples.
pub fn resources(sessions: &[Value]) -> Value {
    let mut summary = serde_json::Map::new();
    for (name, path) in [
        (
            "fresh_peak_bytes",
            "/resources/fresh/peak_working_set_bytes",
        ),
        (
            "restart_peak_bytes",
            "/resources/restart/peak_working_set_bytes",
        ),
        ("fresh_ready_bytes", "/at_ready/fresh/working_set_bytes"),
        ("restart_ready_bytes", "/at_ready/restart/working_set_bytes"),
        (
            "after_edits_bytes",
            "/after_queries_and_edits/working_set_bytes",
        ),
        (
            "after_close_bytes",
            "/after_close_and_watch/working_set_bytes",
        ),
        (
            "last_sampled_cpu_seconds",
            "/resources/restart/last/cpu_seconds",
        ),
        ("idle_cpu_seconds", "/idle/cpu_seconds"),
        (
            "soak_working_set_change_bytes",
            "/soak/last_minus_initial_bytes",
        ),
    ] {
        if path.starts_with("/soak/") && sessions.iter().all(|s| s.get("soak").is_none()) {
            continue;
        }
        let values: Vec<_> = sessions
            .iter()
            .filter_map(|s| s.pointer(path).and_then(Value::as_f64))
            .collect();
        summary.insert(name.into(),json!({"observed":values.len(),"sessions":sessions.len(),"median":median(&values).ok()}));
    }
    Value::Object(summary)
}

pub fn resource_changes(before: &Value, after: &Value) -> Value {
    let mut changes = serde_json::Map::new();
    if let Some(fields) = after.as_object() {
        for (key, new) in fields {
            let old = &before[key];
            let delta = old["median"]
                .as_f64()
                .zip(new["median"].as_f64())
                .filter(|_| {
                    old["observed"] == old["sessions"] && new["observed"] == new["sessions"]
                })
                .map(|(a, b)| b - a);
            changes.insert(key.clone(), json!({"before":old,"after":new,"delta":delta}));
        }
    }
    Value::Object(changes)
}

pub fn check_workload(baseline: &Value, workload: &Value) -> Result<(), String> {
    if baseline["schema_version"] != super::SCHEMA
        || baseline["status"] != "passed"
        || &baseline["workload"] != workload
    {
        return Err("baseline must be a successful suite report with matching schema, fixture, cache, platform and workload settings".into());
    }
    Ok(())
}

pub fn compare(
    baseline: &Value,
    workload: &Value,
    current: &Metrics,
    threshold: Option<f64>,
    floor_ms: f64,
) -> Result<Value, String> {
    check_workload(baseline, workload)?;
    let before: Metrics =
        serde_json::from_value(baseline["metrics"].clone()).map_err(|e| e.to_string())?;
    if before.keys().ne(current.keys()) || current.is_empty() {
        return Err("suite scenario inventories differ".into());
    }
    let mut changes = BTreeMap::new();
    let mut regressions = Vec::new();
    for (name, right) in current {
        let left = &before[name];
        if left.samples_ms.len() != right.samples_ms.len()
            || left.result_counts != right.result_counts
        {
            return Err(format!(
                "{name}: sample counts or validated workload differ"
            ));
        }
        let old = left.stats()?;
        let new = right.stats()?;
        let statistic = if old["p95_ms"].is_number() {
            "p95_ms"
        } else {
            "median_ms"
        };
        let a = old[statistic].as_f64().unwrap();
        let b = new[statistic].as_f64().unwrap();
        let diff = b - a;
        let pct = (a > 0.0).then(|| 100.0 * diff / a);
        if threshold.is_some_and(|max| diff > floor_ms && pct.is_none_or(|delta| delta > max)) {
            regressions.push(name);
        }
        changes.insert(name,json!({"statistic":statistic,"before_ms":a,"after_ms":b,"delta_ms":diff,"delta_pct":pct}));
    }
    Ok(
        json!({"changes":changes,"regressions":regressions,"fail_over_pct":threshold,"min_delta_ms":floor_ms}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tails_require_observations_and_comparison_rejects_lost_work() {
        let mut metrics = Metrics::new();
        for _ in 0..20 {
            record(&mut metrics, "query", Duration::from_millis(2), Some(3));
        }
        let stats = metrics["query"].stats().unwrap();
        assert_eq!(stats["p95_ms"], 2.0);
        assert!(stats["p99_ms"].is_null());
        let workload = json!({"fixture":"a"});
        let base = json!({"schema_version":super::super::SCHEMA,"status":"passed","workload":workload,"metrics":metrics});
        let series = metrics.get_mut("query").unwrap();
        series.samples_ms.fill(4.0);
        assert_eq!(
            compare(&base, &workload, &metrics, Some(10.0), 1.0).unwrap()["regressions"],
            json!(["query"])
        );
        metrics.get_mut("query").unwrap().result_counts[0] = 0;
        assert!(compare(&base, &workload, &metrics, None, 1.0).is_err());
    }

    #[test]
    fn incomplete_resource_pairs_keep_the_missing_observation() {
        let observed = json!({"at_ready":{"fresh":{"working_set_bytes":100}}});
        let a = resources(&[observed.clone(), Value::Null]);
        let b = resources(&[observed.clone(), observed]);
        assert_eq!(a["fresh_ready_bytes"]["observed"], 1);
        assert_eq!(a["fresh_ready_bytes"]["median"], 100.0);
        assert!(resource_changes(&a, &b)["fresh_ready_bytes"]["delta"].is_null());
    }
}
