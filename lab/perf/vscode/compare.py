"""Compare complete VS Code/LSP responses and summarize measured edit latency.
Usage: python lab/perf/vscode/compare.py OUTPUT --baseline base-1 base-2 --candidate new-1 new-2
Only data.template.revision is normalized, by probe.cjs before writing responses.
"""
import argparse
from collections import defaultdict
import json
import math
from pathlib import Path
import re
import statistics


def read(path):
    return json.loads(path.read_text(encoding="utf-8-sig"))


def stats(values):
    ordered = sorted(values)
    if not ordered:
        return None
    return {"n": len(values), "medianMs": statistics.median(values),
            "p95Ms": ordered[math.ceil(.95 * len(ordered)) - 1], "maxMs": ordered[-1],
            "under100ms": sum(v < 100 for v in values)}


def responses_by_phase(values):
    groups = defaultdict(list)
    for row in values:
        if row.get("phase"):
            groups[row["phase"]].append(row["result"])
    return groups


def summarize(root, labels):
    groups = defaultdict(list)
    edited, first_edited, repeated_edited, stable, readiness = [], [], [], [], []
    ready_at, first_provider, auto_times = [], [], []
    resources, hashes, automatic = [], [], []
    for label in labels:
        report = read(root / f"host-{label}.json")
        assert report["status"] == "completed", (label, report.get("error"))
        hashes.append(report["serverSha256"])
        automatic.append(report.get("automaticTyping"))
        auto_times.extend(value["completionAfterMs"]
                          for value in (report.get("automaticTyping") or {}).values()
                          if value.get("responseObserved") and value.get("completionAfterMs") is not None)
        resources.append(read(root / f"host-{label}.resources.json")["summary"])
        for row in report["samples"]:
            phase, ms = row["phase"], row["elapsedMs"]
            if ":edit-to-provider:" in phase:
                edited.append(ms)
                first_round = phase.endswith(":round-1") or not re.search(r":round-\d+$", phase)
                (first_edited if first_round else repeated_edited).append(ms)
                groups[re.sub(r":round-\d+$", "", phase)].append(ms)
            elif ":raw-after-edit:" in phase:
                stable.append(ms)
                groups[re.sub(r":round-\d+$", "", phase)].append(ms)
            elif phase == "workspace.ready-and-summary":
                readiness.append(ms)
                ready_at.append(row["atMs"])
            elif phase.endswith(":provider-first"):
                first_provider.append(ms)
    return {"labels": labels, "serverSha256": sorted(set(hashes)), "edited": stats(edited),
            "firstRoundEdited": stats(first_edited), "repeatedEdited": stats(repeated_edited),
            "stable": stats(stable), "readiness": stats(readiness),
            "readyAt": stats(ready_at), "firstProvider": stats(first_provider),
            "automaticInputToLspResponse": stats(auto_times),
            "scenarios": {k: stats(v) for k, v in groups.items()},
            "cpuSecondsMedian": statistics.median(r["lastCpuSeconds"] for r in resources),
            "peakWorkingSetMiBMedian": statistics.median(r["peakWorkingSetBytes"] / 2**20 for r in resources),
            "peakPrivateMiBMedian": statistics.median(r["peakPrivateBytes"] / 2**20 for r in resources),
            "automaticTyping": automatic}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path)
    parser.add_argument("--baseline", nargs="+", required=True)
    parser.add_argument("--candidate", nargs="+", required=True)
    args = parser.parse_args()
    assert len(args.baseline) == len(args.candidate), "Use matched A/B runs"
    comparisons, failures, extra = [], [], []
    item_count = response_count = 0
    for before, after in zip(args.baseline, args.candidate):
        a, b = (read(args.directory / f"host-{name}.json") for name in (before, after))
        for field in ("sourceSha256",):
            if a[field] != b[field]:
                failures.append({"pair": [before, after], "field": field})
        if a["workspaceSummary"]["fileCounts"] != b["workspaceSummary"]["fileCounts"]:
            failures.append({"pair": [before, after], "field": "indexed file counts"})
        ar, br = (read(args.directory / f"host-{name}.responses.json") for name in (before, after))
        for kind in ("raw", "provider"):
            left, right = responses_by_phase(ar[kind]), responses_by_phase(br[kind])
            if set(left) != set(right):
                failures.append({"pair": [before, after], "kind": kind,
                                 "missing": sorted(set(left) ^ set(right))})
            for phase in sorted(set(left) & set(right)):
                expected = left[phase][0]
                equal = all(value == expected for value in left[phase] + right[phase])
                if not equal:
                    failures.append({"pair": [before, after], "kind": kind, "phase": phase})
                count = min(len(left[phase]), len(right[phase]))
                response_count += count
                items = expected if isinstance(expected, list) else (expected or {}).get("items", [])
                item_count += len(items) * count
                comparisons.append({"pair": [before, after], "kind": kind, "phase": phase, "equal": equal})
                if len(left[phase]) != len(right[phase]):
                    extra.append({"pair": [before, after], "kind": kind, "phase": phase,
                                  "baseline": len(left[phase]), "candidate": len(right[phase])})
    result = {"baseline": summarize(args.directory, args.baseline),
              "candidate": summarize(args.directory, args.candidate),
              "matchedResponses": response_count, "comparedItems": item_count,
              "equal": not failures, "failures": failures, "extraResponseCounts": extra,
              "comparisons": comparisons}
    result["latencyTarget"] = {"targetMs": 100,
                               "allEditedUnderTarget": result["candidate"]["edited"]["maxMs"] < 100,
                               "editedP95UnderTarget": result["candidate"]["edited"]["p95Ms"] < 100,
                               "everyEditedScenarioP95UnderTarget": all(
                                   value["p95Ms"] < 100 for phase, value in result["candidate"]["scenarios"].items()
                                   if ":edit-to-provider:" in phase)}
    destination = args.directory / "vscode-comparison.json"
    destination.write_text(json.dumps(result, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({k: v for k, v in result.items() if k != "comparisons"}, ensure_ascii=False, indent=2))
    raise SystemExit(bool(failures))


if __name__ == "__main__":
    main()
