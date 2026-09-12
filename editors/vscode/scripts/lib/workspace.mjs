/**
 * Source-tree discovery for the diagnostic tool: walks a mod or Vanilla
 * source directory with deterministic ordering and bounded depth, symlink,
 * and file-count limits, then narrows the result by path prefix and shard.
 */

import { readdirSync } from 'node:fs';
import { extname, join } from 'node:path';

import { logicalRelative } from './paths.mjs';
import { CliUsageError } from './options.mjs';

const RELEVANT_EXTENSIONS = new Set(['.txt', '.gfx', '.yml', '.yaml']);
const MAX_SCAN_DEPTH = 64;
const MAX_REPORTED_SYMLINKS = 256;

/**
 * Walks `root` collecting every relevant source file in sorted order.
 *
 * Returns the files plus the scan-quality counters the report surfaces:
 * skipped symlinks (bounded), the omitted-symlink count, and how many
 * directories were cut off by the depth limit.
 */
export function collectSourceFiles(root, maxFiles) {
  const files = [];
  const skippedSymlinks = [];
  let omittedSymlinks = 0;
  let depthLimitedDirectories = 0;

  function walk(directory, depth) {
    if (depth > MAX_SCAN_DEPTH) {
      depthLimitedDirectories += 1;
      return;
    }
    const entries = readdirSync(directory, { withFileTypes: true }).sort((left, right) =>
      left.name < right.name ? -1 : left.name > right.name ? 1 : 0,
    );
    for (const entry of entries) {
      const path = join(directory, entry.name);
      if (entry.isSymbolicLink()) {
        if (skippedSymlinks.length < MAX_REPORTED_SYMLINKS) {
          skippedSymlinks.push(logicalRelative(root, path));
        } else {
          omittedSymlinks += 1;
        }
        continue;
      }
      if (entry.isDirectory()) {
        walk(path, depth + 1);
        continue;
      }
      if (!entry.isFile() || !RELEVANT_EXTENSIONS.has(extname(entry.name).toLowerCase())) continue;
      files.push(path);
      if (files.length > maxFiles) {
        throw new CliUsageError(
          `Current Mod contains more than --max-files ${maxFiles} relevant files`,
        );
      }
    }
  }

  walk(root, 0);
  return { files, skippedSymlinks, omittedSymlinks, depthLimitedDirectories };
}

/**
 * Narrows discovered files to the requested path prefix and stable
 * round-robin shard; both filters are no-ops at their defaults.
 */
export function filterFiles(files, source, pathPrefix, shardCount, shardIndex) {
  let selected = files;
  if (pathPrefix) {
    selected = selected.filter((file) => {
      const logicalPath = logicalRelative(source, file);
      return logicalPath === pathPrefix || logicalPath.startsWith(`${pathPrefix}/`);
    });
  }
  if (shardCount > 1) {
    selected = selected.filter((_, index) => index % shardCount === shardIndex);
  }
  return selected;
}

export { MAX_SCAN_DEPTH };
