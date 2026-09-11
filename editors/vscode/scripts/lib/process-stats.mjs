/**
 * Child-process resource sampling shared by the performance scripts.
 *
 * `WorkingSetSampler` in `lsp-e2e.mjs` only reports memory; comparing against
 * CWTools also needs the child's cumulative CPU time, so this module samples
 * both in one probe per tick. On Windows each probe is a short PowerShell
 * invocation (`Get-Process`), on Unix a single `ps` call.
 */

import { execFile } from 'node:child_process';
import { promisify } from 'node:util';

const execFileAsync = promisify(execFile);

/**
 * @typedef {object} ProcessSample
 * @property {number} cpuSeconds Cumulative user+kernel processor seconds.
 * @property {number} workingSetBytes Resident set size in bytes.
 */

/**
 * Reads the cumulative CPU seconds and working set of a live process.
 *
 * Returns `undefined` when the process has exited or the platform probe
 * fails; callers treat that as "sample unavailable" rather than zero.
 *
 * @param {number} pid
 * @returns {Promise<ProcessSample | undefined>}
 */
export async function sampleProcessStats(pid) {
  if (!pid) return undefined;
  try {
    if (process.platform === 'win32') {
      const { stdout } = await execFileAsync(
        'powershell',
        [
          '-NoProfile',
          '-NonInteractive',
          '-Command',
          `$p = Get-Process -Id ${pid} -ErrorAction SilentlyContinue; if ($p) { "{0} {1}" -f $p.CPU, $p.WorkingSet64 }`,
        ],
        { windowsHide: true, maxBuffer: 1_048_576 },
      );
      const match = stdout.trim().match(/^(\d+(?:\.\d+)?)\s+(\d+)$/);
      if (!match) return undefined;
      return { cpuSeconds: Number(match[1]), workingSetBytes: Number(match[2]) };
    }
    const { stdout } = await execFileAsync(
      'ps',
      ['-o', 'time=,rss=', '-p', String(pid)],
      { maxBuffer: 1_048_576 },
    );
    const columns = stdout.trim().split(/\s+/);
    if (columns.length < 2) return undefined;
    return { cpuSeconds: parsePosixTime(columns[0]), workingSetBytes: Number(columns[1]) * 1024 };
  } catch {
    return undefined;
  }
}

/** Parses the `[DD-]HH:MM:SS[.cc]` cumulative time reported by `ps`. */
function parsePosixTime(value) {
  let days = 0;
  let rest = value;
  const dash = value.indexOf('-');
  if (dash >= 0) {
    days = Number(value.slice(0, dash));
    rest = value.slice(dash + 1);
  }
  const parts = rest.split(':').map((part) => Number(part));
  const [hours = 0, minutes = 0, seconds = 0] = parts;
  return days * 86_400 + hours * 3_600 + minutes * 60 + seconds;
}

/**
 * Samples a process on a fixed interval and keeps the peak/last observations.
 *
 * Probes never overlap: while one is in flight further ticks reuse its promise.
 */
export class ProcessSampler {
  /**
   * @param {number} pid
   * @param {number} intervalMs
   */
  constructor(pid, intervalMs) {
    this.pid = pid;
    this.intervalMs = intervalMs;
    this.timer = undefined;
    this.inFlight = undefined;
    this.samples = 0;
    this.last = undefined;
    this.first = undefined;
    this.peakWorkingSetBytes = undefined;
  }

  start() {
    this.timer = setInterval(() => {
      void this.sample();
    }, this.intervalMs);
    void this.sample();
  }

  /** @returns {Promise<void>} */
  sample() {
    if (this.inFlight) return this.inFlight;
    this.inFlight = sampleProcessStats(this.pid)
      .then((stats) => {
        if (stats === undefined) return;
        this.samples += 1;
        if (this.first === undefined) this.first = stats;
        this.last = stats;
        this.peakWorkingSetBytes =
          this.peakWorkingSetBytes === undefined
            ? stats.workingSetBytes
            : Math.max(this.peakWorkingSetBytes, stats.workingSetBytes);
      })
      .finally(() => {
        this.inFlight = undefined;
      });
    return this.inFlight;
  }

  /** @returns {Promise<void>} */
  async stop() {
    if (this.timer) clearInterval(this.timer);
    this.timer = undefined;
    await this.sample();
  }

  /** Cumulative CPU seconds at the most recent successful sample. */
  get cpuSeconds() {
    return this.last?.cpuSeconds;
  }

  /** CPU seconds gained between two sample indices, or `undefined`. */
  cpuDelta(fromSeconds, toSeconds) {
    if (fromSeconds === undefined || toSeconds === undefined) return undefined;
    return Math.max(0, toSeconds - fromSeconds);
  }
}

/** Formats bytes as a human-readable string. */
export function formatBytes(bytes) {
  if (bytes === undefined || bytes === null || !Number.isFinite(bytes)) return 'n/a';
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KiB`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MiB`;
  return `${(bytes / (1024 * 1024 * 1024)).toFixed(2)} GiB`;
}
