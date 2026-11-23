"""Render the Performance workflow's local JSON artifacts into its Markdown job summary."""
import json
import sys
from pathlib import Path


def read(path):
    return json.loads(path.read_bytes())


def main(root):
    report = read(root / "pair.json")
    base = (root / "baseline-commit.txt").read_text().strip()
    head = (root / "candidate-commit.txt").read_text().strip()
    print(f"**Performance {report['status']}** comparing `{base[:12]}` with `{head[:12]}`.")
    if report.get("same_binary"):
        print("\nBoth executables have the same SHA-256 (A/A calibration).")
    elif base == head:
        print("\nBoth executables were built from the same commit (same-source calibration).")
    if report.get("critical"):
        print("\n| Operation | Statistic | Baseline | Candidate | Median paired change | Pairs over limits |")
        print("|---|---|---:|---:|---:|---:|")
    for name, row in report.get("critical", {}).items():
        print(f"| {name} | {row['within_session_statistic']} (n={row['samples_per_session']}) "
              f"| {row['median_before_ms']:.2f} ms | {row['median_after_ms']:.2f} ms "
              f"| {row['median_delta_ms']:+.2f} ms | {row['pairs_exceeding_both_limits']}/{row['pairs']} |")

    if (root / "soak.json").is_file():
        suite = read(root / "soak.json")
        print(f"\n**Long session {suite['status']}**")
        for session in suite.get("sessions", []):
            soak = session.get("soak", {})
            print(f"\n{soak.get('completed_cycles', 0)} edit/close cycles completed.")
            rss = soak.get("last_minus_initial_bytes")
            latency = soak.get("last_minus_first_window_ms")
            if rss is not None:
                print(f"Working set changed by {rss / 2**20:+.1f} MiB.")
            if latency is not None:
                print(f"Last-edit median changed by {latency:+.2f} ms from the first to the last window.")

    if (root / "large-files.json").is_file():
        suite = read(root / "large-files.json")
        print(f"\n**Large-file edits {suite['status']}**")
        for name in ("round/open-to-first-completion", "round/last-edit-to-completion"):
            row = suite.get("summary", {}).get(name)
            if row:
                print(f"\n{name}: {row['median_ms']:.2f} ms median ({row['n']} trials).")


if __name__ == "__main__":
    main(Path(sys.argv[1]))
