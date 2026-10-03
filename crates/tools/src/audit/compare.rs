//! Complete SQLite counts and multiset diagnostic comparisons.
use crate::report;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub fn read_cache(path: &Path) -> Result<Value, String> {
    let connection =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| e.to_string())?;
    let metadata = read_metadata(path)?;
    let mut result = json!({"metadata":metadata,"files":connection.query_row("SELECT COUNT(*) FROM source_files",[],|r|r.get::<_,i64>(0)).map_err(|e|e.to_string())?});
    for (key, table) in [
        ("definitions", "definitions"),
        ("references", "symbol_references"),
    ] {
        let mut query = connection
            .prepare(&format!("SELECT kind, COUNT(*) FROM {table} GROUP BY kind"))
            .map_err(|e| e.to_string())?;
        let rows = query
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
            .map_err(|e| e.to_string())?;
        let mut counts = serde_json::Map::new();
        for row in rows {
            let (kind, n) = row.map_err(|e| e.to_string())?;
            counts.insert(kind, json!(n));
        }
        result[key] = Value::Object(counts);
    }
    Ok(result)
}
pub fn read_metadata(path: &Path) -> Result<Value, String> {
    let connection =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| e.to_string())?;
    let mut metadata = serde_json::Map::new();
    let mut query = connection
        .prepare("SELECT key, value FROM metadata")
        .map_err(|e| e.to_string())?;
    let rows = query
        .query_map([], |row| {
            let key: String = row.get(0)?;
            let bytes = match row.get_ref(1)? {
                rusqlite::types::ValueRef::Text(b) | rusqlite::types::ValueRef::Blob(b) => b,
                _ => return Err(rusqlite::Error::InvalidQuery),
            };
            Ok((key, String::from_utf8_lossy(bytes).into_owned()))
        })
        .map_err(|e| e.to_string())?;
    for row in rows {
        let (key, value) = row.map_err(|e| e.to_string())?;
        metadata.insert(key, json!(value));
    }
    Ok(Value::Object(metadata))
}
pub struct Run {
    pub baseline: Value,
    pub report: Value,
    pub cache: Value,
    pub files: BTreeSet<String>,
}
fn paths(input: &Value) -> Result<BTreeSet<String>, String> {
    input
        .as_array()
        .ok_or("missing diagnosed files")?
        .iter()
        .map(|file| {
            file["path"]
                .as_str()
                .map(str::to_owned)
                .ok_or("invalid file path".into())
        })
        .collect()
}
pub fn load(directory: &Path, cache: &Path) -> Result<Run, String> {
    let baseline = report::json(&directory.join("baseline-latest.json"))?;
    let mut reports = std::fs::read_dir(directory)
        .map_err(|e| e.to_string())?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|s| s.to_str())
                .is_some_and(|s| s.starts_with("project-") && s.ends_with(".json"))
        })
        .collect::<Vec<_>>();
    reports.sort();
    let input = report::json(reports.last().ok_or("missing full diagnostic report")?)?;
    report::ensure_complete(&input)?;
    super::rules_identity(&input)?;
    if baseline["metadata"]["status"] != "passed"
        || !baseline["metadata"]["tool_errors"]
            .as_array()
            .is_some_and(Vec::is_empty)
    {
        return Err("incomplete baseline or tool errors".into());
    }
    let cache = read_cache(cache)?;
    let hashes = baseline["metadata"]["rules"]
        .as_object()
        .ok_or("missing baseline identities")?;
    let hash = hashes
        .get("rule_hash")
        .or_else(|| hashes.get("manifest_rule_hash"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or("missing rule identity")?;
    for key in [
        "manifest_rule_hash",
        "server_rule_hash",
        "server_log_rule_hash",
        "workspace_summary_rule_hash",
        "rule_hash",
    ] {
        if hashes.get(key).is_some_and(|value| value != hash) {
            return Err("baseline/server rule identity mismatch".into());
        }
    }
    if cache["metadata"]["rule_hash"] != hash || super::rule_hash(&input) != hash {
        return Err("baseline/cache/full report rule identity mismatch".into());
    }
    let files = paths(&baseline["diagnostics"])?;
    if files != paths(&input["files"])? || files.is_empty() || cache["files"] == 0 {
        return Err("empty or mismatched diagnostic file inventory".into());
    }
    let count = input["files"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|f| f["diagnostics"].as_array().into_iter().flatten())
        .filter(|d| d["severity"] == 1)
        .count();
    if baseline["metadata"]["totals"]["errors"].as_u64() != Some(count as u64) {
        return Err("baseline/full report totals mismatch".into());
    }
    Ok(Run {
        baseline,
        report: input,
        cache,
        files,
    })
}
type Counter = BTreeMap<String, usize>;
fn errors(input: &Value, location_only: bool) -> Counter {
    let mut result = Counter::new();
    for file in input["files"].as_array().into_iter().flatten() {
        for diagnostic in file["diagnostics"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|d| d["severity"] == 1)
        {
            let mut identity = vec![
                file["path"].clone(),
                diagnostic["code"].clone(),
                diagnostic["range"].clone(),
            ];
            if !location_only {
                identity.push(diagnostic["message"].clone());
            }
            *result.entry(json!(identity).to_string()).or_default() += 1;
        }
    }
    result
}
fn difference(before: &Counter, after: &Counter) -> Counter {
    before
        .iter()
        .filter_map(|(key, n)| {
            let delta = n.saturating_sub(*after.get(key).unwrap_or(&0));
            (delta > 0).then(|| (key.clone(), delta))
        })
        .collect()
}
fn groups(counter: &Counter) -> Vec<Value> {
    let mut result = BTreeMap::<(String, String), usize>::new();
    for (identity, n) in counter {
        let fields: Value = serde_json::from_str(identity).expect("constructed identity");
        *result
            .entry((
                fields[1].as_str().unwrap_or("unknown").into(),
                fields[3].as_str().unwrap_or("").into(),
            ))
            .or_default() += n;
    }
    let mut result = result
        .into_iter()
        .map(|((code, message), count)| json!({"code":code,"message":message,"count":count}))
        .collect::<Vec<_>>();
    result.sort_by_key(|g| std::cmp::Reverse(g["count"].as_u64().unwrap()));
    result
}
fn labels(input: &Value) -> Result<BTreeSet<String>, String> {
    input["labels"]
        .as_array()
        .ok_or("missing completion labels")?
        .iter()
        .map(|s| {
            s.as_str()
                .map(str::to_owned)
                .ok_or("invalid completion label".into())
        })
        .collect()
}
fn keyed<'a>(input: &'a Value, key: &str) -> Result<BTreeMap<String, &'a Value>, String> {
    let mut result = BTreeMap::new();
    for value in input.as_array().ok_or("missing completion probes")? {
        let id = value[key].as_str().ok_or("missing probe key")?.to_owned();
        if result.insert(id, value).is_some() {
            return Err("duplicate completion probe".into());
        }
    }
    Ok(result)
}
fn completion_delta(before: &Value, after: &Value) -> Result<Value, String> {
    if before["position"] != after["position"] {
        return Err("completion probe moved".into());
    }
    let old = labels(before)?;
    let new = labels(after)?;
    let incomplete = before["is_incomplete"]
        .as_bool()
        .ok_or("missing completion completeness")?
        || after["is_incomplete"]
            .as_bool()
            .ok_or("missing completion completeness")?;
    Ok(
        json!({"before_count":old.len(),"after_count":new.len(),"added":new.difference(&old).collect::<Vec<_>>(),"removed":old.difference(&new).collect::<Vec<_>>(),"comparison":if incomplete {"truncated-needs-review"} else if old==new {"same"} else {"changed-needs-review"}}),
    )
}
fn count_delta(before: &Value, after: &Value) -> Vec<Value> {
    let keys = before
        .as_object()
        .into_iter()
        .flatten()
        .chain(after.as_object().into_iter().flatten())
        .map(|(k, _)| k)
        .collect::<BTreeSet<_>>();
    keys.into_iter()
        .filter_map(|key| {
            let old = before[key].as_u64().unwrap_or(0);
            let new = after[key].as_u64().unwrap_or(0);
            (old != new)
                .then(|| json!({"kind":key,"before":old,"after":new,"delta":new as i64-old as i64}))
        })
        .collect()
}
pub fn compare(before: &Run, after: &Run) -> Result<Value, String> {
    if before.files != after.files {
        return Err("the two runs diagnosed different file inventories".into());
    }
    for key in ["source_fingerprint", "source_root"] {
        if before.cache["metadata"][key]
            .as_str()
            .is_none_or(str::is_empty)
            || before.cache["metadata"][key] != after.cache["metadata"][key]
        {
            return Err(format!("the two caches have different {key}"));
        }
    }
    let old = keyed(&before.baseline["completions"], "id")?;
    let new = keyed(&after.baseline["completions"], "id")?;
    if old.is_empty() || old.keys().ne(new.keys()) {
        return Err("missing or mismatched completion probes".into());
    }
    let mut probes = Vec::new();
    for (id, left) in old {
        let right = new[&id];
        if left["document_path"] != right["document_path"] {
            return Err(format!("completion probe moved: {id}"));
        }
        let mut delta = completion_delta(left, right)?;
        delta["id"] = json!(id);
        delta["before_incomplete"] = left["is_incomplete"].clone();
        delta["after_incomplete"] = right["is_incomplete"].clone();
        let empty = json!([]);
        let old_samples = keyed(left.get("prefix_samples").unwrap_or(&empty), "prefix")?;
        let new_samples = keyed(right.get("prefix_samples").unwrap_or(&empty), "prefix")?;
        let mut samples = Vec::new();
        for (prefix, sample) in &old_samples {
            if let Some(other) = new_samples.get(prefix) {
                let mut delta = completion_delta(sample, other)?;
                delta["prefix"] = json!(prefix);
                samples.push(delta);
            }
        }
        delta["prefix_samples"] = json!(samples);
        delta["unpaired_prefixes"] = json!(
            old_samples
                .keys()
                .collect::<BTreeSet<_>>()
                .symmetric_difference(&new_samples.keys().collect())
                .collect::<Vec<_>>()
        );
        delta["prefix_coverage"] =
            json!("representative samples; base truncation remains unaccepted");
        probes.push(delta);
    }
    let old = errors(&before.report, false);
    let new = errors(&after.report, false);
    let added = difference(&new, &old);
    let removed = difference(&old, &new);
    let old_locations = errors(&before.report, true);
    let new_locations = errors(&after.report, true);
    let added_locations = difference(&new_locations, &old_locations);
    let removed_locations = difference(&old_locations, &new_locations);
    let mut by_code = BTreeMap::<String, usize>::new();
    for (identity, n) in &added_locations {
        let identity: Value = serde_json::from_str(identity).unwrap();
        *by_code
            .entry(identity[1].as_str().unwrap_or("unknown").into())
            .or_default() += n;
    }
    let totals = |run: &Run| {
        let mut totals = run.baseline["metadata"]["totals"].clone();
        totals["indexed_files"] = run.cache["files"].clone();
        for key in ["definitions", "references"] {
            totals[key] = json!(
                run.cache[key]
                    .as_object()
                    .into_iter()
                    .flatten()
                    .filter_map(|(_, v)| v.as_u64())
                    .sum::<u64>()
            );
        }
        totals
    };
    Ok(
        json!({"schema_version":1,"status":"comparison-exported-semantic-review-required","diagnostic_identity":"path + code + range + exact message; wording changes count as differences","source_fingerprint":before.cache["metadata"]["source_fingerprint"],"rule_hashes":{"before":before.cache["metadata"]["rule_hash"],"after":after.cache["metadata"]["rule_hash"]},"files_diagnosed":before.files.len(),"totals":{"before":totals(before),"after":totals(after)},"definition_deltas":count_delta(&before.cache["definitions"],&after.cache["definitions"]),"reference_deltas":count_delta(&before.cache["references"],&after.cache["references"]),"errors_added_by_identity":added.values().sum::<usize>(),"errors_removed_by_identity":removed.values().sum::<usize>(),"errors_added_by_location":added_locations.values().sum::<usize>(),"errors_removed_by_location":removed_locations.values().sum::<usize>(),"errors_added_by_location_by_code":by_code,"location_comparison":"path + code + range; ignores wording only, does not approve semantic differences","added_error_groups":groups(&added),"removed_error_groups":groups(&removed),"completions":probes}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wording_and_duplicate_counts_are_preserved() {
        let diagnostic = json!({"severity":1,"code":"X","range":{},"message":"old"});
        let a = json!({"files":[{"path":"x","diagnostics":[diagnostic,diagnostic]}]});
        let mut b = a.clone();
        b["files"][0]["diagnostics"][0]["message"] = json!("new");
        assert_eq!(
            difference(&errors(&b, false), &errors(&a, false))
                .values()
                .sum::<usize>(),
            1
        );
        assert!(difference(&errors(&b, true), &errors(&a, true)).is_empty());
    }
    #[test]
    fn identical_truncated_completions_remain_unaccepted() {
        let input = json!({"position":{},"labels":["a"],"is_incomplete":true});
        assert_eq!(
            completion_delta(&input, &input).unwrap()["comparison"],
            "truncated-needs-review"
        );
    }
}
