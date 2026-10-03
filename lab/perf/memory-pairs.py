#!/usr/bin/env python3
"""Measure sequential full mem_probe pairs, retaining only local artifacts.

Each worker waits for its measured child with wait4, so its peak excludes the
sampler and other children. Phase RSS uses ps for that child's PID only.
"""
import argparse
import hashlib
import json
from pathlib import Path
import platform
import re
import statistics
import subprocess
import sys
import time


WORKSPACE = Path(__file__).resolve().parents[2]
REQUIRED_PHASES = {'rules', 'vanilla', 'scan-retained', 'evicted', 'dropped', 'end'}


def worker(binary, root, output):
    import os

    started = time.monotonic()
    phases = {}
    sampler_errors = []
    with output.with_suffix('.log').open('w') as log:
        process = subprocess.Popen([str(binary), str(root)], stdout=subprocess.PIPE,
                                   stderr=subprocess.STDOUT, text=True)
        for line in process.stdout:
            log.write(line)
            log.flush()
            if line.startswith('PHASE:'):
                label = line.strip().split(':', 1)[1]
                try:
                    sample = subprocess.check_output(
                        ['/bin/ps', '-o', 'rss=', '-p', str(process.pid)],
                        text=True, stderr=subprocess.STDOUT, timeout=5)
                    phases[label] = int(sample.strip()) * 1024
                except (OSError, subprocess.SubprocessError, ValueError) as error:
                    sampler_errors.append({'phase': label, 'error': str(error)})
        _, status, usage = os.wait4(process.pid, 0)
        process.returncode = os.waitstatus_to_exitcode(status)
    peak = usage.ru_maxrss * (1 if platform.system() == 'Darwin' else 1024)
    diagnostic_pass = re.search(r'diagnostics pass: files=(\d+) count=(\d+) total=([\d.]+)s digest=(0x[0-9a-f]+)',
                                output.with_suffix('.log').read_text())
    diagnostics = ({'files': int(diagnostic_pass[1]), 'count': int(diagnostic_pass[2]),
                    'seconds': float(diagnostic_pass[3]), 'digest': diagnostic_pass[4]}
                   if diagnostic_pass else None)
    result = {'binary': str(binary), 'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
              'root': str(root), 'exit_code': process.returncode,
              'elapsed_seconds': time.monotonic() - started,
              'peak_rss_bytes': peak, 'phase_rss_bytes': phases,
              'sampler_errors': sampler_errors,
              'missing_phases': sorted(REQUIRED_PHASES - phases.keys()),
              'diagnostics': diagnostics,
              'method': 'wait4 rusage of measured child; phase RSS from ps for that child PID',
              'limitations': 'OS allocator retention and memory compression affect RSS; diagnostic workloads can differ'}
    output.write_text(json.dumps(result, indent=2) + '\n')
    return process.returncode


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--before', type=Path)
    parser.add_argument('--after', type=Path)
    parser.add_argument('--root', type=Path, default=WORKSPACE / 'data/vanilla')
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--repeat', type=int, default=3)
    parser.add_argument('--worker', type=Path)
    parser.add_argument('--require-identical-diagnostics', action='store_true',
                        help='Fail if any run lacks a diagnostic signature or has different files/count/digest')
    args = parser.parse_args()
    output = args.output.resolve()
    output.relative_to(WORKSPACE / 'performance-results')
    root = args.root.resolve(strict=True)
    if args.worker:
        return worker(args.worker.resolve(strict=True), root, output)
    if not args.before or not args.after or args.repeat < 1:
        parser.error('--before, --after, and a positive --repeat are required')
    output.mkdir(parents=True, exist_ok=True)
    results = {'before': [], 'after': []}
    for iteration in range(args.repeat):
        for label, binary in [('before', args.before), ('after', args.after)]:
            record = output / f'{label}-{iteration + 1}.json'
            subprocess.run([sys.executable, __file__, '--worker', str(binary), '--root', str(root),
                            '--output', str(record)], check=True)
            results[label].append(json.loads(record.read_text()))
            print(label, iteration + 1, 'complete', flush=True)
    summary = {}
    for label, runs in results.items():
        phases = set.intersection(*(set(run['phase_rss_bytes']) for run in runs))
        summary[label] = {'peak_rss_bytes_median': statistics.median(run['peak_rss_bytes'] for run in runs),
                          'elapsed_seconds_median': statistics.median(run['elapsed_seconds'] for run in runs),
                          'diagnostic_seconds_median': (statistics.median(run['diagnostics']['seconds'] for run in runs)
                                                        if all(run['diagnostics'] for run in runs) else None),
                          'phase_rss_bytes_medians': {phase: statistics.median(run['phase_rss_bytes'][phase] for run in runs)
                                                     for phase in sorted(phases)}}
    signatures = [run['diagnostics'] for runs in results.values() for run in runs]
    identical = all(signature is not None for signature in signatures) and len({
        (signature['files'], signature['count'], signature['digest']) for signature in signatures if signature
    }) == 1
    failed = args.require_identical_diagnostics and not identical
    result = {'status': 'failed-diagnostic-comparison' if failed else 'measured-review-required',
              'repeat': args.repeat, 'runs': results, 'summary': summary,
              'diagnostic_signatures_match': identical,
              'identical_diagnostics_required': args.require_identical_diagnostics,
              'phase_sampling_complete': all(not run['sampler_errors'] and REQUIRED_PHASES <= run['phase_rss_bytes'].keys()
                                             for runs in results.values() for run in runs),
              'limitations': 'Local sequential measurements; different rule semantics can change the diagnosis workload'}
    (output / 'comparison.json').write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps(summary), flush=True)
    return 1 if failed else 0


if __name__ == '__main__':
    sys.exit(main())
