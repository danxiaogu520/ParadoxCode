#!/usr/bin/env python3
"""Export a local phase-five comparison; this does not approve semantic changes.

The LSP workspace summary omits lazy references. Read counts from the complete,
read-only SQLite caches instead. Reports may contain licensed corpus-derived
names/messages and must stay below the ignored performance-results directory.
"""

import argparse
from collections import Counter
import json
from pathlib import Path
import sqlite3


ROOT = Path(__file__).resolve().parents[2]


def read_json(path):
    with path.open(encoding="utf-8") as source:
        return json.load(source)


def read_cache(path):
    with sqlite3.connect(path.resolve().as_uri() + "?mode=ro", uri=True) as db:
        metadata = {
            key: value.decode("utf-8") if isinstance(value, bytes) else value
            for key, value in db.execute("SELECT key, value FROM metadata")
        }
        counts = {
            label: dict(db.execute(f'SELECT kind, COUNT(*) FROM "{table}" GROUP BY kind'))
            for label, table in (
                ("definitions", "definitions"),
                ("references", "symbol_references"),
            )
        }
        files = db.execute("SELECT COUNT(*) FROM source_files").fetchone()[0]
    return {"metadata": metadata, "files": files, **counts}


def load_run(directory, cache_path):
    baseline = read_json(directory / "baseline-latest.json")
    reports = sorted(directory.glob("project-*.json"))
    if not reports:
        raise ValueError(f"missing full diagnostic report: {directory}")
    report = read_json(reports[-1])
    cache = read_cache(cache_path)
    if baseline["metadata"]["status"] != "passed" or report["status"] != "passed":
        raise ValueError(f"incomplete audit: {directory}")
    if baseline["metadata"]["tool_errors"] or report["tool_errors"]:
        raise ValueError(f"tool errors in audit: {directory}")
    hashes = baseline["metadata"]["rules"]
    expected_hash = hashes["manifest_rule_hash"]
    if any(value != expected_hash for value in hashes.values()):
        raise ValueError(f"manifest/server identity mismatch: {directory}")
    if cache["metadata"]["rule_hash"] != expected_hash:
        raise ValueError(f"baseline/cache rule identity mismatch: {directory}")
    if report["inputs"]["rules"]["manifest_rule_hash"] != expected_hash:
        raise ValueError(f"baseline/full report identity mismatch: {directory}")
    baseline_files = {item["path"] for item in baseline["diagnostics"]}
    report_files = {item["path"] for item in report["files"]}
    if baseline_files != report_files or not baseline_files or not cache["files"]:
        raise ValueError(f"empty or mismatched diagnostic file inventory: {directory}")
    error_count = sum(
        diagnostic["severity"] == 1
        for file in report["files"] for diagnostic in file["diagnostics"]
    )
    if error_count != baseline["metadata"]["totals"]["errors"]:
        raise ValueError(f"baseline/full report totals mismatch: {directory}")
    return {"baseline": baseline, "report": report, "cache": cache, "files": baseline_files}


def count_delta(before, after):
    return [
        {"kind": key, "before": before.get(key, 0), "after": after.get(key, 0),
         "delta": after.get(key, 0) - before.get(key, 0)}
        for key in sorted(before.keys() | after.keys())
        if before.get(key, 0) != after.get(key, 0)
    ]


def errors(report):
    return Counter(
        (file["path"], diagnostic["code"],
         json.dumps(diagnostic["range"], sort_keys=True), diagnostic["message"])
        for file in report["files"] for diagnostic in file["diagnostics"]
        if diagnostic["severity"] == 1
    )


def error_groups(counter):
    grouped = Counter()
    for (_, code, _, message), count in counter.items():
        grouped[(code, message)] += count
    return [
        {"code": code, "message": message, "count": count}
        for (code, message), count in grouped.most_common()
    ]


def error_locations(counter):
    locations = Counter()
    for (path, code, range_, _), count in counter.items():
        locations[(path, code, range_)] += count
    return locations


def compare(before, after):
    if before["files"] != after["files"]:
        raise ValueError("the two runs diagnosed different file inventories")
    for key in ("source_fingerprint", "source_root"):
        if before["cache"]["metadata"][key] != after["cache"]["metadata"][key]:
            raise ValueError(f"the two caches have different {key}")
    probes = []
    old = {p["id"]: p for p in before["baseline"]["completions"]}
    new = {p["id"]: p for p in after["baseline"]["completions"]}
    if old.keys() != new.keys() or not old:
        raise ValueError("missing or mismatched completion probes")
    for key in sorted(old):
        left, right = old[key], new[key]
        if any(left[name] != right[name] for name in ("position", "document_path")):
            raise ValueError(f"completion probe moved: {key}")
        left_labels, right_labels = set(left["labels"]), set(right["labels"])
        truncated = bool(left["is_incomplete"] or right["is_incomplete"])
        left_samples = {item["prefix"]: item for item in left.get("prefix_samples", [])}
        right_samples = {item["prefix"]: item for item in right.get("prefix_samples", [])}
        sample_comparisons = []
        for prefix in sorted(left_samples.keys() & right_samples.keys()):
            old_sample, new_sample = left_samples[prefix], right_samples[prefix]
            if old_sample["position"] != new_sample["position"]:
                raise ValueError(f"completion sample moved: {key}/{prefix}")
            old_labels, new_labels = set(old_sample["labels"]), set(new_sample["labels"])
            incomplete = old_sample["is_incomplete"] or new_sample["is_incomplete"]
            sample_comparisons.append({
                "prefix": prefix, "before_count": len(old_labels), "after_count": len(new_labels),
                "added": sorted(new_labels - old_labels), "removed": sorted(old_labels - new_labels),
                "comparison": "truncated-needs-review" if incomplete else (
                    "same" if old_labels == new_labels else "changed-needs-review"),
            })
        probes.append({
            "id": key, "before_count": len(left_labels), "after_count": len(right_labels),
            "added": sorted(right_labels - left_labels),
            "removed": sorted(left_labels - right_labels),
            "before_incomplete": left["is_incomplete"],
            "after_incomplete": right["is_incomplete"],
            "comparison": "truncated-needs-review" if truncated else (
                "same" if left_labels == right_labels else "changed-needs-review"),
            "prefix_samples": sample_comparisons,
            "unpaired_prefixes": sorted(left_samples.keys() ^ right_samples.keys()),
            "prefix_coverage": "representative samples; base truncation remains unaccepted",
        })
    old_errors, new_errors = errors(before["report"]), errors(after["report"])
    added, removed = new_errors - old_errors, old_errors - new_errors
    old_locations, new_locations = error_locations(old_errors), error_locations(new_errors)
    added_locations = new_locations - old_locations
    removed_locations = old_locations - new_locations
    return {
        "schema_version": 1,
        "status": "comparison-exported-semantic-review-required",
        "diagnostic_identity": "path + code + range + exact message; wording changes count as differences",
        "source_fingerprint": before["cache"]["metadata"]["source_fingerprint"],
        "rule_hashes": {side: run["cache"]["metadata"]["rule_hash"]
                        for side, run in (("before", before), ("after", after))},
        "files_diagnosed": len(before["files"]),
        "totals": {side: {**run["baseline"]["metadata"]["totals"],
                          "indexed_files": run["cache"]["files"],
                          "definitions": sum(run["cache"]["definitions"].values()),
                          "references": sum(run["cache"]["references"].values())}
                   for side, run in (("before", before), ("after", after))},
        "definition_deltas": count_delta(before["cache"]["definitions"], after["cache"]["definitions"]),
        "reference_deltas": count_delta(before["cache"]["references"], after["cache"]["references"]),
        "errors_added_by_identity": sum(added.values()),
        "errors_removed_by_identity": sum(removed.values()),
        "errors_added_by_location": sum(added_locations.values()),
        "errors_removed_by_location": sum(removed_locations.values()),
        "errors_added_by_location_by_code": dict(Counter(
            code for (_, code, _), count in added_locations.items() for _ in range(count))),
        "location_comparison": "path + code + range; ignores wording only, does not approve semantic differences",
        "added_error_groups": error_groups(added),
        "removed_error_groups": error_groups(removed),
        "completions": probes,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for side in ("before", "after"):
        parser.add_argument(f"--{side}", type=Path, required=True, help="baseline report directory")
        parser.add_argument(f"--{side}-cache", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if not args.output.resolve().is_relative_to(ROOT / "performance-results"):
        parser.error("licensed corpus-derived output must stay below performance-results/")
    try:
        result = compare(load_run(args.before, args.before_cache), load_run(args.after, args.after_cache))
    except (KeyError, ValueError, sqlite3.Error) as error:
        parser.error(str(error))
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(result, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(f"Exported comparison of {result['files_diagnosed']} files; semantic review remains required.")


if __name__ == "__main__":
    main()
